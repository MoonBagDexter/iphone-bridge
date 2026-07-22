//! HTTP handlers for the Dictate tab: voice settings, provider keys, history,
//! stats, and re-processing a stored transcript under a different mode.
//!
//! Every route is registered inside the PIN-gated `api` router in
//! `net::server`, so auth is inherited rather than re-implemented here.
//!
//! The shape of each response is built by hand rather than by serializing
//! `VoiceSettings` directly. That is deliberate: `AiSettings` holds four API
//! keys, and this JSON crosses a network to a phone. Serializing the struct
//! would leak them the moment anyone adds a field.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::files::config;
use crate::logging::log_both;
use crate::net::server::AppState;
use crate::voice::history::{self, Entry};
use crate::voice::settings::{AiSettings, Mode, Provider, Replacement, VoiceSettings};
use crate::voice::{ai, replacements, Plan};

/// How many history entries to return when the phone doesn't ask for a count.
const DEFAULT_HISTORY_LIMIT: usize = 100;

/// Every handler returns JSON, so they share one return type.
type ApiResponse = (StatusCode, Json<Value>);

fn ok(body: Value) -> ApiResponse {
    (StatusCode::OK, Json(body))
}

fn err(status: StatusCode, msg: &str) -> ApiResponse {
    (status, Json(json!({ "error": msg })))
}

// --- settings serialization (pure) -----------------------------------------

fn provider_id(p: Provider) -> &'static str {
    match p {
        Provider::None => "none",
        Provider::Anthropic => "anthropic",
        Provider::Openai => "openai",
        Provider::Gemini => "gemini",
        Provider::Openrouter => "openrouter",
    }
}

/// Which providers currently have a key, without saying what any of them is.
fn has_key_json(ai: &AiSettings) -> Value {
    json!({
        "anthropic": !ai.anthropic_key.trim().is_empty(),
        "openai": !ai.openai_key.trim().is_empty(),
        "gemini": !ai.gemini_key.trim().is_empty(),
        "openrouter": !ai.openrouter_key.trim().is_empty(),
    })
}

fn default_models_json() -> Value {
    json!({
        "anthropic": Provider::Anthropic.default_model(),
        "openai": Provider::Openai.default_model(),
        "gemini": Provider::Gemini.default_model(),
        "openrouter": Provider::Openrouter.default_model(),
    })
}

fn mode_json(m: &Mode) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "prompt": m.prompt,
        "apps": m.apps,
        "use_replacements": m.use_replacements,
        "use_vocabulary": m.use_vocabulary,
        "use_context": m.use_context,
    })
}

/// The full settings payload the phone renders from. Field-by-field on purpose
/// -- see the module header.
pub fn settings_json(v: &VoiceSettings) -> Value {
    json!({
        "modes": v.modes.iter().map(mode_json).collect::<Vec<_>>(),
        "active_mode": v.active_mode,
        "auto_mode": v.auto_mode,
        "replacements": v
            .replacements
            .iter()
            .map(|r| json!({ "from": r.from, "to": r.to }))
            .collect::<Vec<_>>(),
        "vocabulary": v.vocabulary,
        "history_cap": v.history_cap,
        "ai": {
            "provider": provider_id(v.ai.provider),
            "model": v.ai.model,
            "has_key": has_key_json(&v.ai),
            "default_models": default_models_json(),
        },
    })
}

// --- settings mutation (pure) ----------------------------------------------

/// An incoming settings update. Notably absent: any key field. Serde ignores
/// unknown fields, so a client that mistakenly posts `anthropic_key` has it
/// dropped on the floor rather than written to disk from an unaudited path --
/// `/api/voice/key` is the only way in.
#[derive(Debug, Default, Deserialize)]
pub struct SettingsPayload {
    #[serde(default)]
    modes: Option<Vec<Mode>>,
    #[serde(default)]
    active_mode: Option<String>,
    #[serde(default)]
    auto_mode: Option<bool>,
    #[serde(default)]
    replacements: Option<Vec<Replacement>>,
    #[serde(default)]
    vocabulary: Option<Vec<String>>,
    #[serde(default)]
    history_cap: Option<usize>,
    #[serde(default)]
    ai: Option<AiPayload>,
}

#[derive(Debug, Default, Deserialize)]
struct AiPayload {
    #[serde(default)]
    provider: Option<Provider>,
    #[serde(default)]
    model: Option<String>,
}

/// Overlay `payload` onto `cfg`. Absent fields are left as they were, and the
/// stored API keys are never touched -- the UI never sends them back, so
/// replacing `ai` wholesale would silently wipe them on every save.
pub fn apply_settings_payload(cfg: &mut VoiceSettings, payload: SettingsPayload) {
    if let Some(v) = payload.modes {
        cfg.modes = v;
    }
    if let Some(v) = payload.active_mode {
        cfg.active_mode = v;
    }
    if let Some(v) = payload.auto_mode {
        cfg.auto_mode = v;
    }
    if let Some(v) = payload.replacements {
        cfg.replacements = v;
    }
    if let Some(v) = payload.vocabulary {
        cfg.vocabulary = v;
    }
    if let Some(v) = payload.history_cap {
        cfg.history_cap = v;
    }
    if let Some(a) = payload.ai {
        if let Some(p) = a.provider {
            cfg.ai.provider = p;
        }
        if let Some(m) = a.model {
            cfg.ai.model = m;
        }
    }
    // Restores the invariants the rest of the code assumes: at least one mode,
    // a live `active_mode`, and a non-zero history cap.
    cfg.normalize();
}

/// Store (or, for an empty string, clear) one provider's key. Returns false for
/// `Provider::None`, which names no key slot.
pub fn set_provider_key(ai: &mut AiSettings, provider: Provider, key: &str) -> bool {
    // Keys pasted on a phone routinely carry a trailing space or newline, and a
    // key that is only whitespace is a clear.
    let key = key.trim().to_string();
    let slot = match provider {
        Provider::None => return false,
        Provider::Anthropic => &mut ai.anthropic_key,
        Provider::Openai => &mut ai.openai_key,
        Provider::Gemini => &mut ai.gemini_key,
        Provider::Openrouter => &mut ai.openrouter_key,
    };
    *slot = key;
    true
}

// --- history selection (pure) ----------------------------------------------

/// A query string only means "search" when it actually carries a term; `?q=`
/// from an empty search box must list everything.
fn wants_search(q: Option<&str>) -> bool {
    q.is_some_and(|s| !s.trim().is_empty())
}

/// Apply the caller's `limit` (or the default) to an already-ordered list.
fn take_limit(mut entries: Vec<Entry>, limit: Option<usize>) -> Vec<Entry> {
    entries.truncate(limit.unwrap_or(DEFAULT_HISTORY_LIMIT));
    entries
}

fn entry_json(e: &Entry) -> Value {
    json!({
        "id": e.id,
        "at": e.at,
        "mode": e.mode,
        "seconds": e.seconds,
        "raw": e.raw,
        "text": e.text,
    })
}

// --- reprocessing (pure apart from the AI call) ----------------------------

/// Re-run a stored transcript through `plan`'s mode.
///
/// Deliberately *not* `voice::apply`: that records a history entry, and
/// re-processing an old dictation must not mint a second one every time the
/// user tries a different mode. The replacements-then-AI order is the same;
/// only the history write is left out.
fn reprocess_text(raw: &str, cfg: &VoiceSettings, plan: &Plan) -> (String, Option<String>) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (String::new(), None);
    }

    let mut text = if plan.mode.use_replacements {
        replacements::apply_replacements(raw, &cfg.replacements)
    } else {
        raw.to_string()
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
                    warning = Some("AI returned nothing; kept the unrewritten text".to_string());
                }
                Err(e) => {
                    log_both(&format!("[voice] reprocess AI rewrite failed: {e}"));
                    warning = Some(format!("AI step failed ({e}); kept the unrewritten text"));
                }
            }
        } else {
            warning = Some(format!(
                "mode \"{}\" needs an AI provider; set one in Settings",
                plan.mode.name
            ));
        }
    }

    (text, warning)
}

// --- GET /api/voice/settings ------------------------------------------------

pub async fn get_settings(State(state): State<AppState>) -> ApiResponse {
    let voice = {
        let cfg = state.config.read().unwrap();
        cfg.voice.clone()
    };
    ok(settings_json(&voice))
}

// --- PUT /api/voice/settings ------------------------------------------------

pub async fn put_settings(
    State(state): State<AppState>,
    Json(payload): Json<SettingsPayload>,
) -> ApiResponse {
    let snapshot = {
        let mut cfg = state.config.write().unwrap();
        apply_settings_payload(&mut cfg.voice, payload);
        cfg.clone()
    };
    config::save(&snapshot);
    log_both(&format!(
        "[voice] settings saved: {} mode(s), provider={}, cap={}",
        snapshot.voice.modes.len(),
        provider_id(snapshot.voice.ai.provider),
        snapshot.voice.history_cap
    ));
    ok(settings_json(&snapshot.voice))
}

// --- POST /api/voice/key ----------------------------------------------------

#[derive(Deserialize)]
pub struct KeyBody {
    provider: Provider,
    key: String,
}

pub async fn set_key(State(state): State<AppState>, Json(body): Json<KeyBody>) -> ApiResponse {
    let (snapshot, accepted) = {
        let mut cfg = state.config.write().unwrap();
        let accepted = set_provider_key(&mut cfg.voice.ai, body.provider, &body.key);
        (cfg.clone(), accepted)
    };
    if !accepted {
        return err(StatusCode::BAD_REQUEST, "\"none\" is not a key-holding provider");
    }
    config::save(&snapshot);
    // The action is worth a log line; the key is not, at any length.
    log_both(&format!(
        "[voice] {} key {}",
        provider_id(body.provider),
        if body.key.trim().is_empty() { "cleared" } else { "set" }
    ));
    ok(json!({ "ok": true, "has_key": has_key_json(&snapshot.voice.ai) }))
}

// --- GET /api/voice/history?q=&limit= ---------------------------------------

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn get_history(Query(q): Query<HistoryQuery>) -> ApiResponse {
    // History reads hit the disk behind a process-wide mutex, so they belong on
    // a blocking thread -- stalling the runtime would also stall the audio WS.
    let outcome = tokio::task::spawn_blocking(move || {
        let entries = if wants_search(q.q.as_deref()) {
            history::search(q.q.as_deref().unwrap_or_default())
        } else {
            history::list()
        };
        take_limit(entries, q.limit)
    })
    .await;

    match outcome {
        Ok(entries) => ok(json!({
            "entries": entries.iter().map(entry_json).collect::<Vec<_>>()
        })),
        Err(e) => {
            log_both(&format!("[voice] history read panicked: {e}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not read history")
        }
    }
}

// --- GET /api/voice/stats ---------------------------------------------------

pub async fn get_stats() -> ApiResponse {
    match tokio::task::spawn_blocking(history::stats).await {
        Ok(s) => ok(serde_json::to_value(s).unwrap_or_else(|_| json!({}))),
        Err(e) => {
            log_both(&format!("[voice] stats read panicked: {e}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not read stats")
        }
    }
}

// --- DELETE /api/voice/history/{id} -----------------------------------------

pub async fn delete_entry(Path(id): Path<String>) -> ApiResponse {
    match tokio::task::spawn_blocking(move || history::delete(&id)).await {
        Ok(Ok(())) => ok(json!({ "ok": true })),
        Ok(Err(e)) => {
            log_both(&format!("[voice] history delete failed: {e:#}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not delete the entry")
        }
        Err(e) => {
            log_both(&format!("[voice] history delete panicked: {e}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not delete the entry")
        }
    }
}

// --- DELETE /api/voice/history ----------------------------------------------

pub async fn clear_history() -> ApiResponse {
    match tokio::task::spawn_blocking(history::clear).await {
        Ok(Ok(())) => {
            log_both("[voice] history cleared");
            ok(json!({ "ok": true }))
        }
        Ok(Err(e)) => {
            log_both(&format!("[voice] history clear failed: {e:#}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not clear history")
        }
        Err(e) => {
            log_both(&format!("[voice] history clear panicked: {e}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "could not clear history")
        }
    }
}

// --- POST /api/voice/history/{id}/reprocess ---------------------------------

#[derive(Deserialize)]
pub struct ReprocessBody {
    mode: String,
    #[serde(default)]
    deliver: bool,
}

pub async fn reprocess(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<ReprocessBody>,
) -> ApiResponse {
    // Clone the settings out of the lock before anything awaits: holding a
    // std RwLock guard across an await point is a deadlock waiting to happen.
    let voice = {
        let cfg = state.config.read().unwrap();
        cfg.voice.clone()
    };

    let Some(mode) = voice.mode(&body.mode).cloned() else {
        return err(StatusCode::BAD_REQUEST, "unknown mode");
    };
    let mode_name = mode.name.clone();
    // Built here rather than via `voice::plan()`: the user is reprocessing from
    // their phone, so whatever window happens to be focused on the PC is not
    // context for this text.
    let plan = Plan {
        whisper_prompt: voice.whisper_prompt(&mode),
        mode,
        window_title: None,
    };
    let should_deliver = body.deliver;

    // The AI call inside `reprocess_text` can take up to 60s, and history reads
    // touch the disk; neither may run on the async runtime.
    let outcome = tokio::task::spawn_blocking(move || {
        let entry = history::get(&id)?;
        let (text, mut warning) = reprocess_text(&entry.raw, &voice, &plan);
        if should_deliver {
            if let Err(e) = crate::dictate::deliver(&text) {
                let msg = format!("could not type the text ({e})");
                warning = Some(match warning {
                    Some(w) => format!("{w}; {msg}"),
                    None => msg,
                });
            }
        }
        Some((text, warning))
    })
    .await;

    match outcome {
        Ok(Some((text, warning))) => {
            log_both(&format!(
                "[voice] reprocessed as {mode_name}: {} chars, delivered={should_deliver}",
                text.chars().count()
            ));
            ok(json!({ "text": text, "warning": warning }))
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "no such history entry"),
        Err(e) => {
            log_both(&format!("[voice] reprocess panicked: {e}"));
            err(StatusCode::INTERNAL_SERVER_ERROR, "reprocessing crashed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings with every provider key populated with a distinctive, greppable
    /// value, so a leak in any response is unmistakable.
    fn keyed_settings() -> VoiceSettings {
        let mut v = VoiceSettings::default();
        v.ai.provider = Provider::Anthropic;
        v.ai.model = "claude-opus-4-8".into();
        v.ai.anthropic_key = "SECRET-ANTHROPIC-abc123".into();
        v.ai.openai_key = "SECRET-OPENAI-def456".into();
        v.ai.gemini_key = "SECRET-GEMINI-ghi789".into();
        v.ai.openrouter_key = "SECRET-OPENROUTER-jkl012".into();
        v
    }

    // --- the security-critical one ---

    #[test]
    fn settings_json_never_contains_an_api_key() {
        let v = keyed_settings();
        let text = serde_json::to_string(&settings_json(&v)).expect("serialize");
        for key in [
            "SECRET-ANTHROPIC-abc123",
            "SECRET-OPENAI-def456",
            "SECRET-GEMINI-ghi789",
            "SECRET-OPENROUTER-jkl012",
        ] {
            assert!(
                !text.contains(key),
                "{key} leaked into the settings response: {text}"
            );
        }
        // And nothing key-shaped snuck in under another name.
        assert!(!text.contains("SECRET"), "some key material survived: {text}");
        // None of the key-bearing field names may appear either -- if one does,
        // someone has started serializing `AiSettings` directly.
        for field in [
            "anthropic_key",
            "openai_key",
            "gemini_key",
            "openrouter_key",
        ] {
            assert!(
                !text.contains(field),
                "the {field} field leaked into the response: {text}"
            );
        }
    }

    #[test]
    fn has_key_reports_presence_without_the_value() {
        let mut v = keyed_settings();
        v.ai.openai_key = String::new();
        v.ai.gemini_key = "   ".into(); // whitespace is not a key
        let j = settings_json(&v);
        assert_eq!(j["ai"]["has_key"]["anthropic"], json!(true));
        assert_eq!(j["ai"]["has_key"]["openai"], json!(false));
        assert_eq!(j["ai"]["has_key"]["gemini"], json!(false));
        assert_eq!(j["ai"]["has_key"]["openrouter"], json!(true));
    }

    // --- contract shape ---

    #[test]
    fn settings_json_has_every_contracted_field() {
        let mut v = VoiceSettings::default();
        v.replacements.push(Replacement {
            from: "at sign".into(),
            to: "@".into(),
        });
        v.vocabulary.push("Tailscale".into());
        v.auto_mode = true;
        let j = settings_json(&v);

        assert_eq!(j["active_mode"], json!("raw"));
        assert_eq!(j["auto_mode"], json!(true));
        assert_eq!(j["history_cap"], json!(500));
        assert_eq!(j["vocabulary"], json!(["Tailscale"]));
        assert_eq!(j["replacements"], json!([{ "from": "at sign", "to": "@" }]));

        let modes = j["modes"].as_array().expect("modes must be an array");
        assert!(!modes.is_empty());
        for field in [
            "id",
            "name",
            "prompt",
            "apps",
            "use_replacements",
            "use_vocabulary",
            "use_context",
        ] {
            assert!(
                modes[0].get(field).is_some(),
                "mode is missing the {field} field: {}",
                modes[0]
            );
        }

        assert_eq!(j["ai"]["provider"], json!("none"));
        assert_eq!(j["ai"]["model"], json!(""));
        for p in ["anthropic", "openai", "gemini", "openrouter"] {
            assert!(j["ai"]["has_key"].get(p).is_some(), "has_key missing {p}");
            assert!(
                j["ai"]["default_models"][p].is_string(),
                "default_models missing {p}"
            );
        }
        assert_eq!(j["ai"]["default_models"]["anthropic"], json!("claude-opus-4-8"));
    }

    #[test]
    fn provider_ids_match_the_wire_names() {
        assert_eq!(provider_id(Provider::None), "none");
        assert_eq!(provider_id(Provider::Anthropic), "anthropic");
        assert_eq!(provider_id(Provider::Openai), "openai");
        assert_eq!(provider_id(Provider::Gemini), "gemini");
        assert_eq!(provider_id(Provider::Openrouter), "openrouter");
    }

    // --- settings PUT ---

    fn payload(v: Value) -> SettingsPayload {
        serde_json::from_value(v).expect("payload must deserialize")
    }

    #[test]
    fn a_settings_put_never_wipes_the_stored_keys() {
        // The UI is never given the keys, so it can never send them back. If a
        // save dropped them the user would be silently logged out of their
        // provider -- this is the regression this test exists for.
        let mut v = keyed_settings();
        apply_settings_payload(
            &mut v,
            payload(json!({
                "active_mode": "email",
                "auto_mode": true,
                "ai": { "provider": "openai", "model": "gpt-5" }
            })),
        );
        assert_eq!(v.ai.anthropic_key, "SECRET-ANTHROPIC-abc123");
        assert_eq!(v.ai.openai_key, "SECRET-OPENAI-def456");
        assert_eq!(v.ai.gemini_key, "SECRET-GEMINI-ghi789");
        assert_eq!(v.ai.openrouter_key, "SECRET-OPENROUTER-jkl012");
        assert_eq!(v.ai.provider, Provider::Openai);
        assert_eq!(v.ai.model, "gpt-5");
        assert_eq!(v.active_mode, "email");
        assert!(v.auto_mode);
    }

    #[test]
    fn a_payload_carrying_key_fields_has_them_ignored() {
        let mut v = keyed_settings();
        apply_settings_payload(
            &mut v,
            payload(json!({
                "ai": { "provider": "gemini", "anthropic_key": "INJECTED", "gemini_key": "INJECTED" }
            })),
        );
        assert_eq!(
            v.ai.anthropic_key, "SECRET-ANTHROPIC-abc123",
            "only /api/voice/key may write a key"
        );
        assert_eq!(v.ai.gemini_key, "SECRET-GEMINI-ghi789");
    }

    #[test]
    fn a_settings_put_replaces_the_collections_it_sends() {
        let mut v = VoiceSettings::default();
        apply_settings_payload(
            &mut v,
            payload(json!({
                "replacements": [{ "from": "at sign", "to": "@" }],
                "vocabulary": ["Tailscale", "whisper.cpp"],
                "modes": [{ "id": "only", "name": "Only", "prompt": "" }]
            })),
        );
        assert_eq!(v.replacements.len(), 1);
        assert_eq!(v.vocabulary, vec!["Tailscale", "whisper.cpp"]);
        assert_eq!(v.modes.len(), 1, "sent modes replace, not merge");
        assert_eq!(v.modes[0].id, "only");
        // Mode toggles fall back to the serde defaults when the UI omits them.
        assert!(v.modes[0].use_replacements);
        assert!(v.modes[0].use_vocabulary);
        assert!(!v.modes[0].use_context);
    }

    #[test]
    fn an_empty_payload_leaves_everything_alone() {
        let mut v = keyed_settings();
        v.vocabulary.push("Tailscale".into());
        apply_settings_payload(&mut v, payload(json!({})));
        assert_eq!(v.vocabulary, vec!["Tailscale"]);
        assert_eq!(v.active_mode, "raw");
        assert_eq!(v.history_cap, 500);
        assert_eq!(v.ai.anthropic_key, "SECRET-ANTHROPIC-abc123");
    }

    #[test]
    fn a_put_normalizes_a_dangling_active_mode() {
        let mut v = VoiceSettings::default();
        apply_settings_payload(&mut v, payload(json!({ "active_mode": "no-such-mode" })));
        assert_eq!(v.active_mode, "", "a mode id that names nothing is cleared");
    }

    #[test]
    fn a_put_that_deletes_the_active_mode_clears_it() {
        let mut v = VoiceSettings::default();
        v.active_mode = "email".into();
        apply_settings_payload(
            &mut v,
            payload(json!({ "modes": [{ "id": "raw", "name": "Voice to Text" }] })),
        );
        assert_eq!(v.active_mode, "");
    }

    #[test]
    fn a_zero_history_cap_is_replaced_by_the_default() {
        let mut v = VoiceSettings::default();
        apply_settings_payload(&mut v, payload(json!({ "history_cap": 0 })));
        assert_eq!(v.history_cap, 500, "a cap of 0 would silently discard every dictation");
    }

    #[test]
    fn an_empty_mode_list_falls_back_to_the_builtins() {
        let mut v = VoiceSettings::default();
        apply_settings_payload(&mut v, payload(json!({ "modes": [] })));
        assert!(!v.modes.is_empty(), "normalize must guarantee a mode exists");
    }

    #[test]
    fn the_put_response_round_trips_through_the_get_shape() {
        let mut v = keyed_settings();
        apply_settings_payload(&mut v, payload(json!({ "history_cap": 25 })));
        let j = settings_json(&v);
        assert_eq!(j["history_cap"], json!(25));
        assert_eq!(j["ai"]["has_key"]["anthropic"], json!(true));
    }

    // --- key setting ---

    #[test]
    fn setting_one_key_leaves_the_others_intact() {
        let mut ai = AiSettings::default();
        assert!(set_provider_key(&mut ai, Provider::Anthropic, "sk-ant-1"));
        assert!(set_provider_key(&mut ai, Provider::Openai, "sk-oai-2"));
        assert_eq!(ai.anthropic_key, "sk-ant-1");
        assert_eq!(ai.openai_key, "sk-oai-2");
        assert_eq!(ai.gemini_key, "");
        assert_eq!(ai.openrouter_key, "");

        set_provider_key(&mut ai, Provider::Openrouter, "sk-or-3");
        assert_eq!(ai.anthropic_key, "sk-ant-1", "an unrelated key must not move");
        assert_eq!(ai.openrouter_key, "sk-or-3");
    }

    #[test]
    fn an_empty_key_clears_that_provider_only() {
        let mut ai = keyed_settings().ai;
        assert!(set_provider_key(&mut ai, Provider::Gemini, ""));
        assert_eq!(ai.gemini_key, "");
        assert_eq!(ai.anthropic_key, "SECRET-ANTHROPIC-abc123");
        assert_eq!(ai.openai_key, "SECRET-OPENAI-def456");
        assert_eq!(ai.openrouter_key, "SECRET-OPENROUTER-jkl012");

        // Whitespace is a clear too -- a phone keyboard adds it freely.
        assert!(set_provider_key(&mut ai, Provider::Openai, "  \n "));
        assert_eq!(ai.openai_key, "");
    }

    #[test]
    fn a_pasted_key_is_trimmed() {
        let mut ai = AiSettings::default();
        set_provider_key(&mut ai, Provider::Anthropic, "  sk-ant-trimmed\n");
        assert_eq!(ai.anthropic_key, "sk-ant-trimmed");
    }

    #[test]
    fn none_is_rejected_as_a_key_target() {
        let mut ai = keyed_settings().ai;
        assert!(!set_provider_key(&mut ai, Provider::None, "whatever"));
        assert_eq!(ai.anthropic_key, "SECRET-ANTHROPIC-abc123", "nothing was written");
    }

    #[test]
    fn set_key_response_reports_presence_only() {
        let mut ai = AiSettings::default();
        set_provider_key(&mut ai, Provider::Gemini, "sk-gem");
        let body = json!({ "ok": true, "has_key": has_key_json(&ai) });
        let text = serde_json::to_string(&body).unwrap();
        assert!(!text.contains("sk-gem"), "the key was echoed back: {text}");
        assert_eq!(body["has_key"]["gemini"], json!(true));
        assert_eq!(body["has_key"]["openai"], json!(false));
    }

    // --- history selection ---

    #[test]
    fn a_blank_query_lists_rather_than_searches() {
        assert!(!wants_search(None));
        assert!(!wants_search(Some("")));
        assert!(!wants_search(Some("   ")));
        assert!(wants_search(Some("dave")));
        assert!(wants_search(Some("  dave  ")));
    }

    fn entries(n: usize) -> Vec<Entry> {
        (0..n)
            .map(|i| Entry {
                id: format!("id-{i}"),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn limit_defaults_to_100_and_keeps_the_newest() {
        let out = take_limit(entries(250), None);
        assert_eq!(out.len(), 100);
        assert_eq!(
            out[0].id, "id-0",
            "the list is already newest-first, so truncation keeps the newest"
        );
    }

    #[test]
    fn an_explicit_limit_is_honoured() {
        assert_eq!(take_limit(entries(250), Some(5)).len(), 5);
        assert_eq!(take_limit(entries(250), Some(0)).len(), 0);
        assert_eq!(
            take_limit(entries(3), Some(500)).len(),
            3,
            "a limit above the count is not an error"
        );
    }

    #[test]
    fn entry_json_carries_every_contracted_field() {
        let e = Entry::new("email", 12.5, "raw words", "Polished words.");
        let j = entry_json(&e);
        assert_eq!(j["mode"], json!("email"));
        assert_eq!(j["seconds"], json!(12.5));
        assert_eq!(j["raw"], json!("raw words"));
        assert_eq!(j["text"], json!("Polished words."));
        assert!(j["id"].is_string());
        assert!(j["at"].is_i64());
    }

    // --- reprocessing ---

    /// The reprocess handler builds its own `Plan` (no foreground window is
    /// meaningful when the request came from a phone); mirror that here.
    fn plan_for(cfg: &VoiceSettings, mode_id: &str) -> Plan {
        let mode = cfg.mode(mode_id).expect("test mode must exist").clone();
        Plan {
            whisper_prompt: cfg.whisper_prompt(&mode),
            mode,
            window_title: None,
        }
    }

    #[test]
    fn reprocess_applies_replacements_without_an_ai_call() {
        let mut cfg = VoiceSettings::default();
        cfg.replacements.push(Replacement {
            from: "at sign".into(),
            to: "@".into(),
        });
        let (text, warning) = reprocess_text("me at sign example", &cfg, &plan_for(&cfg, "raw"));
        assert_eq!(text, "me @ example");
        assert!(warning.is_none());
    }

    #[test]
    fn reprocess_warns_when_the_mode_needs_an_absent_provider() {
        let cfg = VoiceSettings::default();
        let (text, warning) = reprocess_text("hey send that over", &cfg, &plan_for(&cfg, "email"));
        assert_eq!(text, "hey send that over", "the words must survive");
        assert!(warning.expect("must warn").contains("Email"));
    }

    #[test]
    fn reprocess_of_a_blank_transcript_is_empty_and_silent() {
        let cfg = VoiceSettings::default();
        let (text, warning) = reprocess_text("  \n ", &cfg, &plan_for(&cfg, "email"));
        assert_eq!(text, "");
        assert!(warning.is_none());
    }
}
