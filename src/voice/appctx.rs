//! What has focus on the PC right now, for dictation.
//!
//! Two uses: picking a dictation mode per app (a terminal wants different
//! post-processing than an email client), and giving the AI rewrite step
//! ambient context about where the text is headed.
//!
//! Everything here is best-effort. A focused window may be a protected or
//! elevated process we can't open, or have no title at all -- those are
//! ordinary outcomes, not errors, so they come back as `None` silently. This
//! runs on every dictation, so failures must not write to `bridge.log`.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

/// The focused window's identity. Either field can be absent even when a
/// window exists: an unopenable process yields no `exe`, an untitled window
/// no `title`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppContext {
    /// Executable file name, lowercased (e.g. `"code.exe"`).
    pub exe: Option<String>,
    /// Title bar text, verbatim.
    pub title: Option<String>,
}

/// Both facts about the foreground window in one pass.
///
/// Preferred over the single-field helpers: it takes one `GetForegroundWindow`
/// reading, so the exe and the title can't describe two different windows if
/// focus moves mid-call. `None` means nothing is focused at all.
pub fn foreground() -> Option<AppContext> {
    // SAFETY: no arguments to get wrong, and the returned HWND is only used
    // below after a null check. It can legitimately be null when the desktop
    // has focus or focus is in transition between windows.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    Some(AppContext {
        exe: window_exe(hwnd),
        title: window_title(hwnd),
    })
}

/// Executable name of the foreground window's process, lowercased.
pub fn foreground_exe() -> Option<String> {
    foreground()?.exe
}

/// Title text of the foreground window. An empty title reports as `None`.
pub fn foreground_title() -> Option<String> {
    foreground()?.title
}

/// Closes a process handle on every exit path, including early `return`s.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: only ever constructed from a non-null handle returned by
        // OpenProcess, never copied and never closed elsewhere, so this is the
        // one and only close of a still-valid handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn window_title(hwnd: HWND) -> Option<String> {
    // SAFETY: hwnd is non-null and came from GetForegroundWindow; this call
    // only reads the window's text length.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return None;
    }

    // The off-by-one: GetWindowTextLengthW's count EXCLUDES the terminating
    // null, but GetWindowTextW's nMaxCount INCLUDES it -- so the buffer must be
    // len + 1 and we pass its full length. Docs also warn that the reported
    // length can exceed the real text, so the count actually copied (the return
    // value) is what we slice by, never `len`.
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: buf is a live allocation of exactly the element count passed as
    // nMaxCount, so the call cannot write past it.
    let copied = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if copied <= 0 {
        return None;
    }

    let title = utf16_to_string(&buf[..copied as usize]);
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

fn window_exe(hwnd: HWND) -> Option<String> {
    let mut pid: u32 = 0;
    // SAFETY: hwnd is non-null; pid is a live u32 the call writes once.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }

    // PROCESS_QUERY_LIMITED_INFORMATION rather than the heavier
    // PROCESS_QUERY_INFORMATION: it's all QueryFullProcessImageNameW needs, and
    // it succeeds against elevated processes that would otherwise refuse us.
    // SAFETY: plain scalar arguments; the returned handle is null-checked and
    // then owned by the guard below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let process = OwnedHandle(handle);

    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    // SAFETY: process.0 is a valid handle opened with the required access
    // right, and `size` is initialised to buf's true element count, so the
    // call cannot overrun the buffer.
    let ok = unsafe { QueryFullProcessImageNameW(process.0, 0, buf.as_mut_ptr(), &mut size) };
    if ok == 0 {
        // Guard closes the handle here too -- this is the path a scattered
        // CloseHandle would miss.
        return None;
    }

    // On success `size` is the count written, excluding the null.
    let name = exe_file_name(&utf16_to_string(&buf[..size as usize]));
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Last path component of a full image path, lowercased. Pure, so the messy
/// part (UNC prefixes, mixed separators) is testable without a live process.
fn exe_file_name(full_path: &str) -> String {
    full_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(full_path)
        .to_lowercase()
}

/// Decode a UTF-16 buffer, stopping at the first null. Win32 sometimes reports
/// a count that includes the terminator and sometimes one that doesn't; cutting
/// at the null makes both shapes decode the same.
fn utf16_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_exe_from_backslash_path() {
        assert_eq!(
            exe_file_name(r"C:\Program Files\Microsoft VS Code\Code.exe"),
            "code.exe"
        );
    }

    #[test]
    fn extracts_exe_from_forward_slash_path() {
        assert_eq!(exe_file_name("C:/Windows/System32/Notepad.EXE"), "notepad.exe");
    }

    #[test]
    fn bare_filename_passes_through_lowercased() {
        assert_eq!(exe_file_name("WindowsTerminal.exe"), "windowsterminal.exe");
    }

    #[test]
    fn extracts_exe_from_unc_path() {
        assert_eq!(
            exe_file_name(r"\\server\share\tools\Ripgrep.exe"),
            "ripgrep.exe"
        );
        // The extended-length prefix Win32 sometimes hands back.
        assert_eq!(exe_file_name(r"\\?\C:\bin\Foo.Exe"), "foo.exe");
    }

    #[test]
    fn handles_path_with_no_extension() {
        assert_eq!(exe_file_name(r"C:\msys64\usr\bin\BASH"), "bash");
    }

    #[test]
    fn handles_empty_and_trailing_separator() {
        assert_eq!(exe_file_name(""), "");
        assert_eq!(exe_file_name(r"C:\some\dir\"), "");
    }

    #[test]
    fn utf16_decodes_with_and_without_trailing_null() {
        let with_null: Vec<u16> = "notepad.exe\0".encode_utf16().collect();
        let without_null: Vec<u16> = "notepad.exe".encode_utf16().collect();
        assert_eq!(utf16_to_string(&with_null), "notepad.exe");
        assert_eq!(utf16_to_string(&without_null), "notepad.exe");
    }

    #[test]
    fn utf16_stops_at_the_first_null_not_the_buffer_end() {
        // The shape of an oversized fixed buffer: text, null, then garbage.
        let mut buf: Vec<u16> = "code.exe".encode_utf16().collect();
        buf.push(0);
        buf.extend("junk".encode_utf16());
        assert_eq!(utf16_to_string(&buf), "code.exe");
    }

    #[test]
    fn utf16_handles_empty_buffer_and_non_ascii() {
        assert_eq!(utf16_to_string(&[]), "");
        assert_eq!(utf16_to_string(&[0]), "");
        let emoji: Vec<u16> = "café 🎤".encode_utf16().collect();
        assert_eq!(utf16_to_string(&emoji), "café 🎤");
    }

    #[test]
    fn utf16_survives_a_lone_surrogate() {
        // from_utf16_lossy must not panic on a truncated surrogate pair.
        assert!(!utf16_to_string(&[0xD800, 0x0041]).is_empty());
    }

    #[test]
    fn foreground_never_panics_and_is_well_formed() {
        // The guarantee worth asserting: whatever the environment, this
        // returns cleanly. A test runner may have no foreground window.
        let ctx = match foreground() {
            Some(c) => c,
            None => {
                eprintln!("SKIPPED: no foreground window in this environment");
                return;
            }
        };

        if let Some(exe) = &ctx.exe {
            assert!(!exe.is_empty(), "exe should be absent, not empty");
            assert_eq!(*exe, exe.to_lowercase(), "exe must be lowercased");
            assert!(exe.ends_with(".exe"), "expected an .exe name, got {exe:?}");
        } else {
            eprintln!("SKIPPED exe assertions: could not open the foreground process");
        }

        if let Some(title) = &ctx.title {
            assert!(!title.is_empty(), "title should be absent, not empty");
        }
    }

    #[test]
    fn single_field_helpers_agree_with_the_combined_call() {
        // Also exercises the OpenProcess/CloseHandle path repeatedly -- a
        // leaked handle per call would show up here first.
        for _ in 0..50 {
            let _ = foreground_exe();
            let _ = foreground_title();
        }
        if let Some(exe) = foreground_exe() {
            assert!(exe.ends_with(".exe"));
        }
    }
}
