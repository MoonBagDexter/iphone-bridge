// Synthetic keyboard events for PC-side control from the iPhone web client.
// All taps are best-effort: failures are logged, never bubbled.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK,
    VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MENU, VK_RETURN, VK_RIGHT,
    VK_RMENU, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};

fn make_kbd(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &mut [INPUT]) {
    unsafe {
        let sent = SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        );
        if sent != inputs.len() as u32 {
            eprintln!(
                "[keyboard] SendInput returned {sent}, expected {}",
                inputs.len()
            );
        }
    }
}

/// Tap a single virtual key once (down + up). `extended` should be true for
/// keys whose physical scan code is prefixed with 0xE0 -- right Alt, the
/// dedicated arrow cluster, etc. -- so that low-level keyboard hooks see the
/// same shape they'd see from a physical press.
fn tap_vk(vk: VIRTUAL_KEY, extended: bool) {
    let base: KEYBD_EVENT_FLAGS = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
    let mut inputs = [
        make_kbd(vk, base),
        make_kbd(vk, base | KEYEVENTF_KEYUP),
    ];
    send_inputs(&mut inputs);
}

/// Press Win + Ctrl + (extended) `vk`, then release in reverse order.
/// Used for Windows virtual-desktop navigation (Win+Ctrl+Left/Right).
fn tap_win_ctrl_chord(vk: VIRTUAL_KEY) {
    let mut inputs = [
        make_kbd(VK_LWIN as VIRTUAL_KEY, 0),
        make_kbd(VK_CONTROL as VIRTUAL_KEY, 0),
        make_kbd(vk, KEYEVENTF_EXTENDEDKEY),
        make_kbd(vk, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
        make_kbd(VK_CONTROL as VIRTUAL_KEY, KEYEVENTF_KEYUP),
        make_kbd(VK_LWIN as VIRTUAL_KEY, KEYEVENTF_KEYUP),
    ];
    send_inputs(&mut inputs);
}

pub fn tap_arrow_up() { tap_vk(VK_UP as VIRTUAL_KEY, true); }
pub fn tap_arrow_down() { tap_vk(VK_DOWN as VIRTUAL_KEY, true); }
pub fn tap_arrow_left() { tap_vk(VK_LEFT as VIRTUAL_KEY, true); }
pub fn tap_arrow_right() { tap_vk(VK_RIGHT as VIRTUAL_KEY, true); }
pub fn tap_enter() { tap_vk(VK_RETURN as VIRTUAL_KEY, false); }
pub fn tap_escape() { tap_vk(VK_ESCAPE as VIRTUAL_KEY, false); }
pub fn tap_backspace() { tap_vk(VK_BACK as VIRTUAL_KEY, false); }
pub fn tap_space() { tap_vk(VK_SPACE as VIRTUAL_KEY, false); }
pub fn tap_desktop_left() { tap_win_ctrl_chord(VK_LEFT as VIRTUAL_KEY); }
pub fn tap_desktop_right() { tap_win_ctrl_chord(VK_RIGHT as VIRTUAL_KEY); }

/// Press Win + extended `vk`, then release in reverse. Used for Win+Down to
/// minimise the foreground window (and other single-modifier Win shortcuts).
fn tap_win_chord(vk: VIRTUAL_KEY) {
    let mut inputs = [
        make_kbd(VK_LWIN as VIRTUAL_KEY, 0),
        make_kbd(vk, KEYEVENTF_EXTENDEDKEY),
        make_kbd(vk, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
        make_kbd(VK_LWIN as VIRTUAL_KEY, KEYEVENTF_KEYUP),
    ];
    send_inputs(&mut inputs);
}

pub fn tap_min_window() { tap_win_chord(VK_DOWN as VIRTUAL_KEY); }

// Win+Tab opens Task View -- the full-screen overlay of all open windows (and
// virtual desktops). Tab is NOT an extended key, so this rides tap_mod_chord
// rather than tap_win_chord (which forces KEYEVENTF_EXTENDEDKEY, wrong for Tab).
pub fn tap_task_view() { tap_mod_chord(VK_LWIN as VIRTUAL_KEY, VK_TAB as VIRTUAL_KEY); }

/// Press `modifier`, tap `key`, release `modifier`. Used for the standard
/// Alt+Tab / Ctrl+Tab / Shift+Tab chords. None of these keys are extended.
fn tap_mod_chord(modifier: VIRTUAL_KEY, key: VIRTUAL_KEY) {
    let mut inputs = [
        make_kbd(modifier, 0),
        make_kbd(key, 0),
        make_kbd(key, KEYEVENTF_KEYUP),
        make_kbd(modifier, KEYEVENTF_KEYUP),
    ];
    send_inputs(&mut inputs);
}

// Alt+Tab needs special handling. The Windows task switcher uses an MRU stack:
// a one-shot Alt+Tab always swaps the top two entries, so repeated single taps
// just ping-pong between two windows. To cycle through more, Alt must stay
// HELD while Tab is tapped multiple times. We synthesise that by keeping Alt
// down on the first tap, tapping Tab on each subsequent tap, and releasing
// Alt after ~800 ms of no further taps.
static ALT_HELD: AtomicBool = AtomicBool::new(false);
static ALT_TAB_TOKEN: AtomicU64 = AtomicU64::new(0);
// Idle window after the last Alt+Tab tap before Alt is released and the
// task-switcher selection commits. Short enough to feel snappy, long enough
// that a normal fast tap rhythm (~5 taps/sec) keeps Alt held.
const ALT_TAB_RELEASE_MS: u64 = 350;

pub fn alt_tab_cycle() {
    let token = ALT_TAB_TOKEN.fetch_add(1, Ordering::SeqCst) + 1;
    let was_held = ALT_HELD.swap(true, Ordering::SeqCst);

    let mut events: Vec<INPUT> = Vec::with_capacity(3);
    if !was_held {
        events.push(make_kbd(VK_MENU as VIRTUAL_KEY, 0));
    }
    events.push(make_kbd(VK_TAB as VIRTUAL_KEY, 0));
    events.push(make_kbd(VK_TAB as VIRTUAL_KEY, KEYEVENTF_KEYUP));
    send_inputs(&mut events);

    // Schedule auto-release. If a newer tap bumped the token, this thread is
    // a stale timer and bails out without releasing.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ALT_TAB_RELEASE_MS));
        if ALT_TAB_TOKEN.load(Ordering::SeqCst) == token {
            if ALT_HELD.swap(false, Ordering::SeqCst) {
                send_inputs(&mut [make_kbd(VK_MENU as VIRTUAL_KEY, KEYEVENTF_KEYUP)]);
            }
        }
    });
}

pub fn tap_ctrl_tab() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, VK_TAB as VIRTUAL_KEY); }
pub fn tap_shift_tab() { tap_mod_chord(VK_SHIFT as VIRTUAL_KEY, VK_TAB as VIRTUAL_KEY); }

// Readline-style line editing shortcuts. VK codes for letters A-Z are the
// same as their ASCII upper-case codepoints (0x41..0x5A), so b'X' works.
pub fn tap_ctrl_u() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, b'U' as VIRTUAL_KEY); }
pub fn tap_ctrl_w() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, b'W' as VIRTUAL_KEY); }
pub fn tap_ctrl_k() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, b'K' as VIRTUAL_KEY); }
pub fn tap_ctrl_a() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, b'A' as VIRTUAL_KEY); }
pub fn tap_ctrl_e() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, b'E' as VIRTUAL_KEY); }
pub fn tap_ctrl_c() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, b'C' as VIRTUAL_KEY); }
// Newline-without-send in Claude Code on Windows Terminal: Ctrl+Enter is what
// actually works (Shift+Enter isn't recognized despite /terminal-setup claiming
// it is). VK_RETURN here is non-extended -- same shape as plain Enter.
pub fn tap_ctrl_enter() { tap_mod_chord(VK_CONTROL as VIRTUAL_KEY, VK_RETURN as VIRTUAL_KEY); }
pub fn tap_tab() { tap_vk(VK_TAB as VIRTUAL_KEY, false); }

/// One UTF-16 code unit as a synthetic Unicode key event. Bypasses keyboard
/// layout entirely -- works the same on QWERTY, AZERTY, Dvorak, etc.
fn unicode_kbd(unit: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: flags | KEYEVENTF_UNICODE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Type a literal string into the focused control by synthesising Unicode
/// key events. Useful for canned prompts ("/btw ", "push", etc).
pub fn type_text(text: &str) {
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
    let mut buf = [0u16; 2];
    for ch in text.chars() {
        let units = ch.encode_utf16(&mut buf);
        for &u in units.iter() {
            inputs.push(unicode_kbd(u, 0));
            inputs.push(unicode_kbd(u, KEYEVENTF_KEYUP));
        }
    }
    send_inputs(&mut inputs);
}

// ---- Configurable PTT hotkey ----------------------------------------------
// The PTT view triggers whichever dictation app the user runs on the PC by
// pressing that app's global hands-free toggle (tap to start, tap to stop).
// The key lives in config.json (`ptt_hotkey`) so switching dictation apps --
// or rebinding inside one -- never needs a rebuild.

/// What a fresh config gets: Right Alt, the owner's hands-free binding in
/// Wispr Flow (and formerly SuperWhisper's hotkey). Wispr Flow's own factory
/// default is "ctrl+win+space", which the parser also accepts.
pub const DEFAULT_PTT_HOTKEY: &str = "right_alt";

/// One key of a parsed chord: virtual-key code plus whether the physical scan
/// code is 0xE0-prefixed (Right Alt, arrows...), which low-level hooks check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChordKey {
    pub vk: u16,
    pub extended: bool,
}

/// Parse "ctrl+win+space" style text into a chord, in press order.
/// Case/whitespace-insensitive. Returns None for anything unrecognized so the
/// caller can fall back to the default rather than silently pressing wrong keys.
pub fn parse_hotkey(s: &str) -> Option<Vec<ChordKey>> {
    let plain = |vk: u16| Some(ChordKey { vk, extended: false });
    let tokens: Vec<String> = s.split('+').map(|t| t.trim().to_ascii_lowercase()).collect();
    if tokens.is_empty() || tokens.iter().any(|t| t.is_empty()) {
        return None;
    }
    let mut chord = Vec::with_capacity(tokens.len());
    for t in &tokens {
        let key = match t.as_str() {
            "ctrl" | "control" => plain(VK_CONTROL),
            "win" | "windows" | "super" => plain(VK_LWIN),
            "alt" => plain(VK_MENU),
            "shift" => plain(VK_SHIFT),
            // Right Alt is what SuperWhisper listened for; keep it available
            // so old setups can put "right_alt" in config.json and carry on.
            "right_alt" | "ralt" | "altgr" => Some(ChordKey { vk: VK_RMENU, extended: true }),
            "space" | "spacebar" => plain(VK_SPACE),
            "enter" | "return" => plain(VK_RETURN),
            "tab" => plain(VK_TAB),
            "esc" | "escape" => plain(VK_ESCAPE),
            t if t.len() == 1 && t.as_bytes()[0].is_ascii_alphanumeric() => {
                // VK codes for A-Z and 0-9 equal their ASCII upper-case codepoints.
                plain(t.as_bytes()[0].to_ascii_uppercase() as u16)
            }
            t if t.starts_with('f') && t[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
                plain(0x70 + (t[1..].parse::<u8>().unwrap() as u16 - 1)) // VK_F1 = 0x70
            }
            _ => None,
        };
        chord.push(key?);
    }
    Some(chord)
}

/// Press a configured chord: keys down in order, up in reverse -- the same
/// shape a human makes, which is what global hotkey hooks expect. Falls back
/// to `DEFAULT_PTT_HOTKEY` (with a log) when the configured string is invalid.
pub fn tap_hotkey(s: &str) {
    let chord = match parse_hotkey(s) {
        Some(c) => c,
        None => {
            crate::logging::log_both(&format!(
                "[keyboard] ptt_hotkey {s:?} in config.json is invalid; using default {DEFAULT_PTT_HOTKEY:?}"
            ));
            parse_hotkey(DEFAULT_PTT_HOTKEY).expect("default hotkey must parse; see tests")
        }
    };
    let base = |k: &ChordKey| if k.extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
    let mut inputs: Vec<INPUT> = Vec::with_capacity(chord.len() * 2);
    for k in &chord {
        inputs.push(make_kbd(k.vk, base(k)));
    }
    for k in chord.iter().rev() {
        inputs.push(make_kbd(k.vk, base(k) | KEYEVENTF_KEYUP));
    }
    send_inputs(&mut inputs);
}

pub fn type_btw() { type_text("/btw "); }
pub fn type_push() {
    type_text("push");
    tap_enter();
}
pub fn type_clear() {
    type_text("/clear");
    tap_enter();
}
pub fn type_resume() {
    type_text("/resume");
    tap_enter();
}

#[cfg(test)]
mod tests {
    use super::*;

    // Raw VK values so the tests don't just mirror the implementation's
    // constant imports: Ctrl=0x11, Win=0x5B, Alt=0x12, Shift=0x10,
    // RightAlt=0xA5, Space=0x20.
    fn vks(chord: &[ChordKey]) -> Vec<u16> {
        chord.iter().map(|k| k.vk).collect()
    }

    #[test]
    fn parses_the_wispr_flow_default_chord() {
        let chord = parse_hotkey("ctrl+win+space").expect("default chord must parse");
        assert_eq!(vks(&chord), vec![0x11, 0x5B, 0x20]);
        assert!(
            chord.iter().all(|k| !k.extended),
            "none of ctrl/win/space are extended keys"
        );
    }

    #[test]
    fn parses_the_legacy_superwhisper_right_alt() {
        let chord = parse_hotkey("right_alt").expect("right_alt must parse");
        assert_eq!(vks(&chord), vec![0xA5]);
        assert!(chord[0].extended, "right alt is an extended key");
    }

    #[test]
    fn is_case_and_whitespace_insensitive() {
        let a = parse_hotkey("ctrl+win+space").unwrap();
        let b = parse_hotkey("  Ctrl + WIN +  Space ").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parses_letters_and_function_keys() {
        let chord = parse_hotkey("ctrl+alt+f7").expect("f-keys must parse");
        assert_eq!(vks(&chord), vec![0x11, 0x12, 0x76]); // F7 = 0x70 + 6
        let chord = parse_hotkey("win+j").expect("letters must parse");
        assert_eq!(vks(&chord), vec![0x5B, b'J' as u16]);
    }

    #[test]
    fn rejects_garbage_instead_of_guessing() {
        assert_eq!(parse_hotkey(""), None, "empty string");
        assert_eq!(parse_hotkey("   "), None, "blank string");
        assert_eq!(parse_hotkey("ctrl+"), None, "trailing plus");
        assert_eq!(parse_hotkey("banana"), None, "unknown key name");
        assert_eq!(parse_hotkey("ctrl+banana"), None, "unknown key in a chord");
    }

    #[test]
    fn accepts_common_aliases() {
        assert_eq!(parse_hotkey("control+windows+spacebar"), parse_hotkey("ctrl+win+space"));
        assert_eq!(parse_hotkey("ralt"), parse_hotkey("right_alt"));
        assert_eq!(parse_hotkey("altgr"), parse_hotkey("right_alt"));
    }

    #[test]
    fn the_shipped_default_always_parses() {
        assert!(
            parse_hotkey(DEFAULT_PTT_HOTKEY).is_some(),
            "DEFAULT_PTT_HOTKEY must never be unparsable -- it is the fallback"
        );
    }
}
