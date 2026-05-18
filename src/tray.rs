use anyhow::{anyhow, Result};
use arboard::Clipboard;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder};
use wasapi::initialize_mta;
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::audio::capture::enumerate_render_devices;
use crate::state::CaptureCtl;

const AUTOSTART_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_NAME: &str = "iphone-bridge";

pub fn run_tray(ctl: Arc<CaptureCtl>, iphone_url: String) -> Result<()> {
    let _ = initialize_mta();
    let devices = enumerate_render_devices().unwrap_or_default();

    let menu = Menu::new();

    let copy_url = MenuItem::new("Copy iPhone URL", true, None);
    menu.append(&copy_url).map_err(|e| anyhow!("menu append: {e}"))?;
    menu.append(&PredefinedMenuItem::separator()).ok();

    let capture_submenu = Submenu::new("Capture from", true);
    let default_item = CheckMenuItem::new("(Default device)", true, true, None);
    capture_submenu.append(&default_item).ok();
    capture_submenu.append(&PredefinedMenuItem::separator()).ok();
    let mut device_items: Vec<CheckMenuItem> = Vec::new();
    for name in &devices {
        let item = CheckMenuItem::new(name, true, false, None);
        capture_submenu.append(&item).ok();
        device_items.push(item);
    }
    menu.append(&capture_submenu).ok();
    menu.append(&PredefinedMenuItem::separator()).ok();

    let autostart_item = CheckMenuItem::new("Start with Windows", true, is_autostart_set(), None);
    menu.append(&autostart_item).ok();
    menu.append(&PredefinedMenuItem::separator()).ok();

    let exit_item = MenuItem::new("Exit", true, None);
    menu.append(&exit_item).ok();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!("iPhone Bridge -- {iphone_url}"))
        .with_icon(make_icon())
        .build()
        .map_err(|e| anyhow!("tray build: {e}"))?;

    let copy_id = copy_url.id().clone();
    let default_id = default_item.id().clone();
    let autostart_id = autostart_item.id().clone();
    let exit_id = exit_item.id().clone();
    let device_ids: Vec<_> = device_items.iter().map(|i| i.id().clone()).collect();

    let receiver = MenuEvent::receiver();

    pump_messages(|| {
        while let Ok(event) = receiver.try_recv() {
            let id = event.id();
            if id == &copy_id {
                match Clipboard::new().and_then(|mut c| c.set_text(iphone_url.clone())) {
                    Ok(_) => eprintln!("[tray] copied URL to clipboard"),
                    Err(e) => eprintln!("[tray] clipboard failed: {e}"),
                }
            } else if id == &default_id {
                *ctl.selected_device.lock().unwrap() = None;
                ctl.restart.store(true, Ordering::SeqCst);
                default_item.set_checked(true);
                for it in &device_items {
                    it.set_checked(false);
                }
                eprintln!("[tray] capture device: (default)");
            } else if id == &autostart_id {
                let want = autostart_item.is_checked();
                match set_autostart(want) {
                    Ok(()) => eprintln!("[tray] autostart -> {}", want),
                    Err(e) => {
                        eprintln!("[tray] autostart toggle failed: {e}");
                        autostart_item.set_checked(!want);
                    }
                }
            } else if id == &exit_id {
                eprintln!("[tray] exit requested");
                std::process::exit(0);
            } else {
                for (i, did) in device_ids.iter().enumerate() {
                    if id == did {
                        let name = devices[i].clone();
                        *ctl.selected_device.lock().unwrap() = Some(name.clone());
                        ctl.restart.store(true, Ordering::SeqCst);
                        default_item.set_checked(false);
                        for (j, it) in device_items.iter().enumerate() {
                            it.set_checked(j == i);
                        }
                        eprintln!("[tray] capture device: {name}");
                        break;
                    }
                }
            }
        }
    });

    Ok(())
}

fn pump_messages<F: FnMut()>(mut on_tick: F) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        loop {
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
            on_tick();
        }
    }
}

fn make_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let cx = SIZE as f32 / 2.0;
    let cy = SIZE as f32 / 2.0;
    let r = SIZE as f32 / 2.0 - 1.0;
    let r2 = r * r;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d2 = dx * dx + dy * dy;
            let i = ((y * SIZE + x) * 4) as usize;
            if d2 <= r2 {
                rgba[i] = 0x4a;
                rgba[i + 1] = 0xde;
                rgba[i + 2] = 0x80;
                rgba[i + 3] = 0xff;
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("icon from rgba")
}

fn current_exe_str() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_autostart_set() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run) = hkcu.open_subkey(AUTOSTART_KEY) else {
        return false;
    };
    let Ok(val): std::io::Result<String> = run.get_value(AUTOSTART_NAME) else {
        return false;
    };
    !val.is_empty()
}

fn set_autostart(enable: bool) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu
        .create_subkey(AUTOSTART_KEY)
        .map_err(|e| anyhow!("create_subkey: {e}"))?;
    if enable {
        let exe = current_exe_str();
        let quoted = format!("\"{exe}\"");
        run.set_value(AUTOSTART_NAME, &quoted)
            .map_err(|e| anyhow!("set_value: {e}"))?;
    } else {
        let _ = run.delete_value(AUTOSTART_NAME);
    }
    Ok(())
}
