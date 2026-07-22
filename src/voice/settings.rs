//! Voice/dictation settings: modes, replacements, vocabulary, AI provider, history cap.
//! Stored inside the existing `config.json` under a `voice` key so there is a single
//! config file. Every field is `serde(default)` so pre-existing configs load clean.

use serde::{Deserialize, Serialize};

/// A deterministic post-transcription find/replace. Case-insensitive, not AI-mediated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    pub from: String,
    pub to: String,
}

/// Which language model service post-processing calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// No AI: modes pass the raw transcript through.
    None,
    Anthropic,
    Openai,
    Gemini,
    /// One key, many models (incl. DeepSeek/Qwen/Kimi/GLM).
    Openrouter,
}

impl Default for Provider {
    fn default() -> Self {
        Provider::None
    }
}

impl Provider {
    /// Default model id for each provider, used when the user hasn't picked one.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::None => "",
            Provider::Anthropic => "claude-opus-4-8",
            Provider::Openai => "gpt-5",
            Provider::Gemini => "gemini-2.5-flash",
            Provider::Openrouter => "deepseek/deepseek-chat",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default)]
    pub provider: Provider,
    /// Per-provider keys, so switching providers doesn't lose the other key.
    #[serde(default)]
    pub anthropic_key: String,
    #[serde(default)]
    pub openai_key: String,
    #[serde(default)]
    pub gemini_key: String,
    #[serde(default)]
    pub openrouter_key: String,
    /// Empty means "use `provider.default_model()`".
    #[serde(default)]
    pub model: String,
}

impl AiSettings {
    /// The key for the currently-selected provider.
    pub fn active_key(&self) -> &str {
        match self.provider {
            Provider::None => "",
            Provider::Anthropic => &self.anthropic_key,
            Provider::Openai => &self.openai_key,
            Provider::Gemini => &self.gemini_key,
            Provider::Openrouter => &self.openrouter_key,
        }
    }

    /// Model id to send, falling back to the provider default.
    pub fn active_model(&self) -> String {
        if self.model.trim().is_empty() {
            self.provider.default_model().to_string()
        } else {
            self.model.trim().to_string()
        }
    }

    /// Can we actually make a call? (provider chosen and a key present)
    pub fn is_ready(&self) -> bool {
        self.provider != Provider::None && !self.active_key().trim().is_empty()
    }
}

/// A mode bundles a post-processing prompt with the toggles that shape a dictation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mode {
    pub id: String,
    pub name: String,
    /// Empty prompt = raw transcription, no AI call.
    #[serde(default)]
    pub prompt: String,
    /// Foreground-process names (e.g. "code.exe") that auto-select this mode.
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default = "yes")]
    pub use_replacements: bool,
    #[serde(default = "yes")]
    pub use_vocabulary: bool,
    /// Include the focused window title as context for the AI.
    #[serde(default)]
    pub use_context: bool,
}

fn yes() -> bool {
    true
}

fn default_history_cap() -> usize {
    500
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceSettings {
    #[serde(default)]
    pub replacements: Vec<Replacement>,
    /// Terms fed to whisper as an initial prompt to bias recognition.
    #[serde(default)]
    pub vocabulary: Vec<String>,
    #[serde(default)]
    pub modes: Vec<Mode>,
    /// Id of the manually-selected mode. Wins over app auto-selection.
    #[serde(default)]
    pub active_mode: String,
    #[serde(default)]
    pub ai: AiSettings,
    /// Keep at most this many history entries.
    #[serde(default = "default_history_cap")]
    pub history_cap: usize,
    /// Let the focused Windows app pick the mode when it matches one.
    #[serde(default)]
    pub auto_mode: bool,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        VoiceSettings {
            replacements: Vec::new(),
            vocabulary: Vec::new(),
            modes: default_modes(),
            active_mode: "raw".to_string(),
            ai: AiSettings::default(),
            history_cap: default_history_cap(),
            auto_mode: false,
        }
    }
}

/// The built-in modes, mirroring SuperWhisper's set plus one tuned for Claude Code.
pub fn default_modes() -> Vec<Mode> {
    let plain = |id: &str, name: &str, prompt: &str| Mode {
        id: id.to_string(),
        name: name.to_string(),
        prompt: prompt.to_string(),
        apps: Vec::new(),
        use_replacements: true,
        use_vocabulary: true,
        use_context: false,
    };
    vec![
        plain("raw", "Voice to Text", ""),
        plain(
            "message",
            "Message",
            "Rewrite the transcript as a casual chat message. Fix speech errors and \
             filler words. Keep the original meaning and voice. Output only the message.",
        ),
        plain(
            "email",
            "Email",
            "Rewrite the transcript as a clear, professional email body. Fix speech \
             errors and filler words. No subject line, no sign-off unless dictated. \
             Output only the email body.",
        ),
        plain(
            "note",
            "Note",
            "Rewrite the transcript as tidy notes. Use short bullet points where the \
             content is a list. Fix speech errors and filler words. Output only the notes.",
        ),
        plain(
            "prompt",
            "Claude Prompt",
            "Rewrite the transcript as a clear instruction to a coding assistant. Fix \
             speech errors and filler words. Keep every technical detail, file name, and \
             identifier exactly as dictated. Do not answer the request or add commentary. \
             Output only the rewritten instruction.",
        ),
    ]
}

impl VoiceSettings {
    /// Look up a mode by id.
    pub fn mode(&self, id: &str) -> Option<&Mode> {
        self.modes.iter().find(|m| m.id == id)
    }

    /// The mode a dictation should run under.
    ///
    /// Manual selection always wins — `auto_mode` only fills in when the user has
    /// not pinned a mode. This is deliberately unlike SuperWhisper, where an
    /// auto-activated mode cannot be overridden.
    pub fn effective_mode(&self, foreground_exe: Option<&str>) -> &Mode {
        if let Some(m) = self.mode(&self.active_mode) {
            return m;
        }
        if self.auto_mode {
            if let Some(exe) = foreground_exe {
                if let Some(m) = self
                    .modes
                    .iter()
                    .find(|m| m.apps.iter().any(|a| a.eq_ignore_ascii_case(exe)))
                {
                    return m;
                }
            }
        }
        // Fall back to the first mode; `default_modes` guarantees one exists.
        self.modes
            .first()
            .expect("modes must never be empty; see VoiceSettings::normalize")
    }

    /// Guarantee the invariants the rest of the code relies on: at least one mode
    /// exists, and `active_mode` names a real mode (or is empty for "auto").
    pub fn normalize(&mut self) {
        if self.modes.is_empty() {
            self.modes = default_modes();
        }
        if self.history_cap == 0 {
            self.history_cap = default_history_cap();
        }
        if !self.active_mode.is_empty() && self.mode(&self.active_mode).is_none() {
            self.active_mode = String::new();
        }
    }

    /// Whisper's `--prompt` biasing string, or None when vocabulary is unused.
    pub fn whisper_prompt(&self, mode: &Mode) -> Option<String> {
        if !mode.use_vocabulary {
            return None;
        }
        let terms: Vec<&str> = self
            .vocabulary
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        if terms.is_empty() {
            None
        } else {
            Some(terms.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_without_voice_key_uses_defaults() {
        let v: VoiceSettings = serde_json::from_str("{}").expect("empty object must load");
        assert_eq!(v.history_cap, 500);
        assert_eq!(v.ai.provider, Provider::None);
        assert!(v.replacements.is_empty());
        // serde default for the struct fields, not Default::default(), so modes are empty
        // until normalize() runs -- that is what normalize is for.
        let mut v = v;
        v.normalize();
        assert!(!v.modes.is_empty(), "normalize must guarantee a mode exists");
    }

    #[test]
    fn roundtrips_through_json() {
        let mut v = VoiceSettings::default();
        v.replacements.push(Replacement {
            from: "at sign".into(),
            to: "@".into(),
        });
        v.vocabulary.push("Tailscale".into());
        v.ai.provider = Provider::Openrouter;
        v.ai.openrouter_key = "sk-or-test".into();
        let json = serde_json::to_string(&v).unwrap();
        let back: VoiceSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.replacements, v.replacements);
        assert_eq!(back.vocabulary, v.vocabulary);
        assert_eq!(back.ai.provider, Provider::Openrouter);
        assert_eq!(back.ai.active_key(), "sk-or-test");
    }

    #[test]
    fn active_model_falls_back_to_provider_default() {
        let mut ai = AiSettings {
            provider: Provider::Anthropic,
            ..Default::default()
        };
        assert_eq!(ai.active_model(), "claude-opus-4-8");
        ai.model = "  claude-sonnet-5  ".into();
        assert_eq!(ai.active_model(), "claude-sonnet-5");
    }

    #[test]
    fn is_ready_requires_provider_and_key() {
        let mut ai = AiSettings::default();
        assert!(!ai.is_ready(), "no provider selected");
        ai.provider = Provider::Openai;
        assert!(!ai.is_ready(), "provider but no key");
        ai.openai_key = "sk-test".into();
        assert!(ai.is_ready());
        // Keys are per-provider: switching away must not borrow the other key.
        ai.provider = Provider::Gemini;
        assert!(!ai.is_ready(), "gemini key is empty");
    }

    #[test]
    fn manual_mode_selection_beats_app_auto_selection() {
        let mut v = VoiceSettings::default();
        v.auto_mode = true;
        v.modes
            .iter_mut()
            .find(|m| m.id == "email")
            .unwrap()
            .apps
            .push("outlook.exe".into());

        // Pinned to raw -> stays raw even though Outlook is focused.
        v.active_mode = "raw".into();
        assert_eq!(v.effective_mode(Some("outlook.exe")).id, "raw");

        // Unpinned -> the app decides.
        v.active_mode = String::new();
        assert_eq!(v.effective_mode(Some("outlook.exe")).id, "email");
        assert_eq!(v.effective_mode(Some("OUTLOOK.EXE")).id, "email");

        // Unpinned, unknown app -> first mode.
        assert_eq!(v.effective_mode(Some("notepad.exe")).id, "raw");
    }

    #[test]
    fn auto_mode_off_ignores_the_foreground_app() {
        let mut v = VoiceSettings::default();
        v.auto_mode = false;
        v.active_mode = String::new();
        v.modes
            .iter_mut()
            .find(|m| m.id == "email")
            .unwrap()
            .apps
            .push("outlook.exe".into());
        assert_eq!(v.effective_mode(Some("outlook.exe")).id, "raw");
    }

    #[test]
    fn normalize_clears_a_dangling_active_mode() {
        let mut v = VoiceSettings::default();
        v.active_mode = "deleted-mode".into();
        v.normalize();
        assert_eq!(v.active_mode, "");
    }

    #[test]
    fn whisper_prompt_joins_vocabulary_and_skips_blanks() {
        let mut v = VoiceSettings::default();
        let mode = v.mode("raw").unwrap().clone();
        assert_eq!(v.whisper_prompt(&mode), None, "no vocabulary yet");

        v.vocabulary = vec!["Tailscale".into(), "  ".into(), "whisper.cpp".into()];
        assert_eq!(
            v.whisper_prompt(&mode),
            Some("Tailscale, whisper.cpp".to_string())
        );

        let mut off = mode.clone();
        off.use_vocabulary = false;
        assert_eq!(v.whisper_prompt(&off), None);
    }
}
