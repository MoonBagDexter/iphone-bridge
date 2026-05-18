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

pub fn tap_alt() { tap_vk(VK_RMENU as VIRTUAL_KEY, true); }
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
const ALT_TAB_RELEASE_MS: u64 = 800;

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

pub fn type_btw() { type_text("/btw "); }
pub fn type_push() {
    type_text("push");
    tap_enter();
}
