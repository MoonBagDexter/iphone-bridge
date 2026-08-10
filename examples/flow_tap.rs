// Smoke test: tap Ctrl+Win+Space once via SendInput -- Wispr Flow's default
// hands-free toggle on Windows.
//
// Run with Wispr Flow running. Watch for its Flow Bar to start listening,
// then run again (or press the chord yourself) to stop it.
//
//   cargo run --example flow_tap
//
// 2s grace period so you can switch focus to the target app first.

use std::mem::size_of;
use std::thread::sleep;
use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
    VK_CONTROL, VK_LWIN, VK_SPACE,
};

fn kbd(vk: VIRTUAL_KEY, flags: u32) -> INPUT {
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

fn main() {
    println!("flow_tap: waiting 2s, then tapping Ctrl+Win+Space once...");
    sleep(Duration::from_secs(2));

    // Down in order, up in reverse -- the same shape a human press makes.
    let mut inputs: [INPUT; 6] = [
        kbd(VK_CONTROL, 0),
        kbd(VK_LWIN, 0),
        kbd(VK_SPACE, 0),
        kbd(VK_SPACE, KEYEVENTF_KEYUP),
        kbd(VK_LWIN, KEYEVENTF_KEYUP),
        kbd(VK_CONTROL, KEYEVENTF_KEYUP),
    ];

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    println!("flow_tap: SendInput returned {sent} (expected 6)");
    println!("flow_tap: done. Did Wispr Flow start listening?");
}
