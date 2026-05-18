// Smoke test: tap Alt once via SendInput.
//
// Run with SuperWhisper (or whatever your Alt-hotkey app is) running and
// focused away from this terminal. Watch for the dictation overlay.
//
//   cargo run --example alt_tap
//
// 2s grace period so you can switch focus to the target app first.

use std::mem::size_of;
use std::thread::sleep;
use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_RMENU,
};

fn main() {
    println!("alt_tap: waiting 2s, then tapping Right Alt once...");
    sleep(Duration::from_secs(2));

    unsafe {
        let mut inputs: [INPUT; 2] = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_RMENU as VIRTUAL_KEY,
                        wScan: 0,
                        dwFlags: KEYEVENTF_EXTENDEDKEY,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_RMENU as VIRTUAL_KEY,
                        wScan: 0,
                        dwFlags: KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];

        let sent = SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        );
        println!("alt_tap: SendInput returned {sent} (expected 2)");
    }

    println!("alt_tap: done. Did SuperWhisper react?");
}
