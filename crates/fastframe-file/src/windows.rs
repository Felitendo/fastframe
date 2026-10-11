//! Windows: the move, with write-through.

// MoveFileExW is FFI.
#![allow(unsafe_code)]

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

/// Moves `temporary` over `path`, returning only once the move is on the
/// disk (`MOVEFILE_WRITE_THROUGH`), as Spotifast's saves did.
pub(crate) fn replace(temporary: &Path, path: &Path) -> io::Result<()> {
    let wide =
        |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (from, to) = (wide(temporary), wide(path));
    // SAFETY: both names are NUL-terminated UTF-16 strings that outlive the
    // call.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
