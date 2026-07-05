//! Recycle-Bin delete via the Win32 Shell `SHFileOperationW`. We deliberately
//! do NOT hard-delete: `FOF_ALLOWUNDO` sends the folder to the Recycle Bin so
//! a misclick from the phone is recoverable.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW,
};

/// Send `path` to the Recycle Bin. Returns `Ok(())` on success, or an error
/// string with the `SHFileOperationW` return code on failure.
pub fn to_recycle_bin(path: &Path) -> Result<(), String> {
    // pFrom must be a double-null-terminated, null-separated list of paths.
    // Build a UTF-16 buffer: <path>\0\0.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);

    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    // fFlags is u16; the FOF_* constants are u32 but all fit in the low 16 bits.
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT) as u16;

    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("SHFileOperationW failed (code {rc})"))
    }
}
