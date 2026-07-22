//! Dictation post-processing: modes, replacements, vocabulary, AI rewriting, history.
//! `src/dictate.rs` owns capture and transcription; this module owns everything that
//! happens to the text afterwards.

pub mod ai;
pub mod api;
pub mod appctx;
pub mod history;
pub mod replacements;
pub mod settings;

use settings::{Mode, VoiceSettings};

/// Everything decided *before* the audio is transcribed.
///
/// Mode selection has to happen up front because the mode decides the whisper
/// vocabulary prompt, which is an argument to transcription -- not something that
/// can be applied afterwards. Capturing the window title here too means it reflects
/// where the user was actually speaking, not wherever focus drifted during the
/// (multi-second) transcription.
pub struct Plan {
    pub mode: Mode,
    pub whisper_prompt: Option<String>,
    window_title: Option<String>,
}

/// Choose the mode and capture context for a dictation that is about to start.
pub fn plan(cfg: &VoiceSettings) -> Plan {
    let fg = appctx::foreground();
    let exe = fg.as_ref().and_then(|f| f.exe.as_deref());
    let mode = cfg.effective_mode(exe).clone();
    let whisper_prompt = cfg.whisper_prompt(&mode);
    // Only carry the title when the mode actually wants it -- see the injection
    // note in `ai::rewrite`; unused context is context that can't be abused.
    let window_title = if mode.use_context {
        fg.and_then(|f| f.title)
    } else {
        None
    };
    Plan {
        mode,
        whisper_prompt,
        window_title,
    }
}

/// The result of running a raw transcript through a mode.
pub struct Processed {
    /// Straight from whisper, before replacements or AI.
    pub raw: String,
    /// What actually gets typed and put on the clipboard.
    pub text: String,
    /// Populated when post-processing partially failed; the caller still delivers
    /// `text` (which falls back to the replaced-but-not-rewritten transcript), but
    /// the phone should be told the AI step did not run.
    pub warning: Option<String>,
}

/// Apply the mode to a raw transcript: replacements, then AI rewriting, then record
/// it in history.
///
/// Deliberately infallible. A failed AI call must not lose the user's words -- we
/// degrade to the deterministic result and report the failure alongside it, because
/// silently delivering nothing is the worst outcome for a dictation tool.
pub fn apply(raw: &str, cfg: &VoiceSettings, plan: &Plan, seconds: f32) -> Processed {
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return Processed {
            raw,
            text: String::new(),
            warning: None,
        };
    }

    let mut text = if plan.mode.use_replacements {
        replacements::apply_replacements(&raw, &cfg.replacements)
    } else {
        raw.clone()
    };

    let mut warning = None;
    if !plan.mode.prompt.trim().is_empty() {
        if cfg.ai.is_ready() {
            match ai::rewrite(
                &cfg.ai,
                &plan.mode.prompt,
                &text,
                plan.window_title.as_deref(),
            ) {
                Ok(rewritten) if !rewritten.trim().is_empty() => text = rewritten,
                Ok(_) => {
                    warning = Some("AI returned nothing; delivered the raw transcript".into());
                }
                Err(e) => {
                    crate::logging::log_both(&format!("[voice] AI rewrite failed: {e}"));
                    warning = Some(format!("AI step failed ({e}); delivered the raw transcript"));
                }
            }
        } else {
            warning = Some(format!(
                "mode \"{}\" needs an AI provider; set one in Settings",
                plan.mode.name
            ));
        }
    }

    if let Err(e) = history::append(
        history::Entry::new(&plan.mode.id, seconds, &raw, &text),
        cfg.history_cap,
    ) {
        // History is a convenience; never let it break a dictation.
        crate::logging::log_both(&format!("[voice] history append failed: {e}"));
    }

    Processed { raw, text, warning }
}

#[cfg(test)]
mod tests {
    use super::*;
    use history::tests::Sandbox;
    use settings::Replacement;

    /// A plan pinned to a mode, bypassing the real foreground-window lookup so the
    /// tests don't depend on what happens to be focused on the machine.
    fn plan_for(cfg: &VoiceSettings, mode_id: &str) -> Plan {
        let mode = cfg.mode(mode_id).expect("test mode must exist").clone();
        let whisper_prompt = cfg.whisper_prompt(&mode);
        Plan {
            mode,
            whisper_prompt,
            window_title: None,
        }
    }

    #[test]
    fn raw_mode_applies_replacements_but_no_ai() {
        let _s = Sandbox::new("voice-apply-raw");
        let mut cfg = VoiceSettings::default();
        cfg.replacements.push(Replacement {
            from: "at sign".into(),
            to: "@".into(),
        });
        let plan = plan_for(&cfg, "raw");
        let out = apply("email me at sign example dot com", &cfg, &plan, 3.0);
        assert_eq!(out.text, "email me @ example dot com");
        assert_eq!(out.raw, "email me at sign example dot com");
        assert!(out.warning.is_none(), "raw mode needs no AI, so no warning");
    }

    #[test]
    fn a_prompted_mode_without_a_provider_warns_and_still_delivers() {
        let _s = Sandbox::new("voice-apply-noprovider");
        let cfg = VoiceSettings::default();
        assert!(!cfg.ai.is_ready(), "default config has no provider");
        let plan = plan_for(&cfg, "email");
        let out = apply("hey can you send that over", &cfg, &plan, 2.0);
        assert_eq!(
            out.text, "hey can you send that over",
            "the words must survive even with no AI configured"
        );
        let warning = out.warning.expect("must warn that the mode needs a provider");
        assert!(
            warning.contains("Email"),
            "warning should name the mode, got: {warning}"
        );
    }

    #[test]
    fn blank_transcript_short_circuits() {
        let _s = Sandbox::new("voice-apply-blank");
        let cfg = VoiceSettings::default();
        let plan = plan_for(&cfg, "email");
        let out = apply("   \n  ", &cfg, &plan, 0.4);
        assert_eq!(out.text, "");
        assert_eq!(out.raw, "");
        assert!(
            out.warning.is_none(),
            "an empty recording is not an AI failure"
        );
    }

    #[test]
    fn a_mode_can_opt_out_of_replacements() {
        let _s = Sandbox::new("voice-apply-noreplace");
        let mut cfg = VoiceSettings::default();
        cfg.replacements.push(Replacement {
            from: "at sign".into(),
            to: "@".into(),
        });
        let mut plan = plan_for(&cfg, "raw");
        plan.mode.use_replacements = false;
        let out = apply("me at sign example", &cfg, &plan, 1.0);
        assert_eq!(out.text, "me at sign example");
    }

    #[test]
    fn plan_carries_the_vocabulary_prompt_for_whisper() {
        let mut cfg = VoiceSettings::default();
        cfg.vocabulary = vec!["Tailscale".into(), "whisper.cpp".into()];
        let plan = plan_for(&cfg, "raw");
        assert_eq!(
            plan.whisper_prompt.as_deref(),
            Some("Tailscale, whisper.cpp")
        );
    }

    #[test]
    fn apply_records_into_the_sandbox_not_the_real_history() {
        // Regression guard: `apply` writes to history, so any test calling it must
        // be sandboxed. Without this, `cargo test` silently appends junk entries to
        // the user's real dictation history -- which is exactly what happened once.
        let _s = Sandbox::new("voice-apply-isolation");
        let cfg = VoiceSettings::default();
        let plan = plan_for(&cfg, "raw");
        assert!(
            history::list().is_empty(),
            "sandbox must start empty; if this fails the redirect is not in effect \
             and the assertion below would be reading real user data"
        );
        apply("one two three", &cfg, &plan, 1.0);
        let entries = history::list();
        assert_eq!(entries.len(), 1, "apply must record exactly one entry");
        assert_eq!(entries[0].text, "one two three");
    }

    #[test]
    fn context_is_only_captured_when_the_mode_asks_for_it() {
        let cfg = VoiceSettings::default();
        let plan = plan_for(&cfg, "raw");
        assert!(
            !plan.mode.use_context,
            "built-in modes must not enable context by default"
        );
        assert!(plan.window_title.is_none());
    }
}
