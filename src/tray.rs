use anyhow::{anyhow, Result};
use arboard::Clipboard;
use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, RwLock};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIconBuilder};
use wasapi::initialize_mta;
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::audio::capture::enumerate_render_devices;
use crate::files::config::{self, Config, Scope};
use crate::logging::log_both;
use crate::state::CaptureCtl;

const AUTOSTART_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_NAME: &str = "iphone-bridge";

pub fn run_tray(
    ctl: Arc<CaptureCtl>,
    iphone_url: String,
    files_config: Arc<RwLock<Config>>,
) -> Result<()> {
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

    // --- Folder access submenu (Files feature) ---
    let folder_menu = Submenu::new("Folder access", true);

    let cur_scope = files_config.read().unwrap().scope;
    let scope_roots = CheckMenuItem::new(
        "Chosen roots",
        true,
        cur_scope == Scope::Roots,
        None,
    );
    let scope_profile = CheckMenuItem::new(
        "Whole user profile",
        true,
        cur_scope == Scope::Profile,
        None,
    );
    let scope_drives = CheckMenuItem::new("All drives", true, cur_scope == Scope::Drives, None);
    folder_menu.append(&scope_roots).ok();
    folder_menu.append(&scope_profile).ok();
    folder_menu.append(&scope_drives).ok();
    folder_menu.append(&PredefinedMenuItem::separator()).ok();

    let add_root = MenuItem::new("Add root...", true, None);
    folder_menu.append(&add_root).ok();

    // Submenu listing removable roots, rebuilt whenever roots change.
    let remove_menu = Submenu::new("Remove root", true);
    let mut remove_items: Vec<MenuItem> = Vec::new();
    rebuild_remove_items(&files_config, &remove_menu, &mut remove_items);
    folder_menu.append(&remove_menu).ok();

    folder_menu.append(&PredefinedMenuItem::separator()).ok();
    let show_pin = MenuItem::new("Show PIN", true, None);
    folder_menu.append(&show_pin).ok();

    menu.append(&folder_menu).ok();
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

    let scope_roots_id = scope_roots.id().clone();
    let scope_profile_id = scope_profile.id().clone();
    let scope_drives_id = scope_drives.id().clone();
    let add_root_id = add_root.id().clone();
    let show_pin_id = show_pin.id().clone();

    // A background folder-picker (blocking rfd dialog) must not run on the tray
    // message-loop thread, or it would stall menu processing. The worker sends
    // the picked path back here; the actual menu mutation happens on-tick.
    let (picked_tx, picked_rx) = std_mpsc::channel::<std::path::PathBuf>();

    let receiver = MenuEvent::receiver();

    pump_messages(|| {
        // Apply any folder the async picker returned since the last tick.
        while let Ok(path) = picked_rx.try_recv() {
            add_root_path(&files_config, path);
            rebuild_remove_items(&files_config, &remove_menu, &mut remove_items);
        }

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
            } else if id == &scope_roots_id {
                set_scope(&files_config, Scope::Roots);
                scope_roots.set_checked(true);
                scope_profile.set_checked(false);
                scope_drives.set_checked(false);
            } else if id == &scope_profile_id {
                set_scope(&files_config, Scope::Profile);
                scope_roots.set_checked(false);
                scope_profile.set_checked(true);
                scope_drives.set_checked(false);
            } else if id == &scope_drives_id {
                set_scope(&files_config, Scope::Drives);
                scope_roots.set_checked(false);
                scope_profile.set_checked(false);
                scope_drives.set_checked(true);
            } else if id == &add_root_id {
                let tx = picked_tx.clone();
                // Blocking native folder picker on its own thread.
                std::thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Add a folder root")
                        .pick_folder()
                    {
                        let _ = tx.send(path);
                    }
                });
            } else if id == &show_pin_id {
                let pin = files_config.read().unwrap().pin.clone();
                std::thread::spawn(move || {
                    rfd::MessageDialog::new()
                        .set_title("iPhone Bridge PIN")
                        .set_description(format!("Files PIN: {pin}"))
                        .show();
                });
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
            } else if let Some(idx) = remove_items.iter().position(|it| it.id() == id) {
                remove_root_at(&files_config, idx);
                rebuild_remove_items(&files_config, &remove_menu, &mut remove_items);
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

/// Persist a new scope selection immediately (live via the RwLock).
fn set_scope(cfg: &Arc<RwLock<Config>>, scope: Scope) {
    {
        let mut c = cfg.write().unwrap();
        c.scope = scope;
    }
    save_config(cfg);
    log_both(&format!("[files] scope -> {scope:?}"));
}

/// Append a folder root (if not already present) and persist.
fn add_root_path(cfg: &Arc<RwLock<Config>>, path: std::path::PathBuf) {
    let s = path.to_string_lossy().into_owned();
    {
        let mut c = cfg.write().unwrap();
        if c.roots.iter().any(|r| r.eq_ignore_ascii_case(&s)) {
            return;
        }
        c.roots.push(s.clone());
    }
    save_config(cfg);
    log_both(&format!("[files] added root {s}"));
}

/// Remove the configured root at `idx` and persist.
fn remove_root_at(cfg: &Arc<RwLock<Config>>, idx: usize) {
    let removed = {
        let mut c = cfg.write().unwrap();
        if idx < c.roots.len() {
            Some(c.roots.remove(idx))
        } else {
            None
        }
    };
    if let Some(r) = removed {
        save_config(cfg);
        log_both(&format!("[files] removed root {r}"));
    }
}

fn save_config(cfg: &Arc<RwLock<Config>>) {
    let snapshot = cfg.read().unwrap().clone();
    config::save(&snapshot);
}

/// Clear and repopulate the "Remove root" submenu to match current roots.
fn rebuild_remove_items(
    cfg: &Arc<RwLock<Config>>,
    remove_menu: &Submenu,
    remove_items: &mut Vec<MenuItem>,
) {
    for it in remove_items.drain(..) {
        let _ = remove_menu.remove(&it);
    }
    let roots = cfg.read().unwrap().roots.clone();
    if roots.is_empty() {
        let placeholder = MenuItem::new("(no roots)", false, None);
        remove_menu.append(&placeholder).ok();
        remove_items.push(placeholder);
        return;
    }
    for r in roots {
        let item = MenuItem::new(format!("Remove: {r}"), true, None);
        remove_menu.append(&item).ok();
        remove_items.push(item);
    }
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
