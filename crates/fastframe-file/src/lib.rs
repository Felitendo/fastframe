//! Replaces a file on disk without ever leaving it half written.
//!
//! Settings, sessions and credentials are rewritten whole. Writing over the
//! file in place leaves it truncated if the app is killed or the power goes
//! halfway through, and the app then starts from defaults. [`write()`] writes a
//! temporary file beside the target instead, flushes it to the disk, and moves
//! it over the target in one step, so a reader sees the old contents or the
//! new ones, never part of either.
//!
//! Flushing to the disk can take tens of milliseconds, and far longer on a
//! busy or network drive. Save from a worker thread, or after the frame,
//! rather than in the middle of drawing one.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! let path = std::path::Path::new("/home/ada/.config/example/settings.json");
//! fastframe_file::write(path, br#"{"theme":"dark"}"#)?;
//! // A secret only the user may read (0600 on Unix):
//! fastframe_file::write_private(&path.with_file_name("token"), b"...")?;
//! // Or stream into the file, from a serializer say:
//! fastframe_file::write_with(path, fastframe_file::Access::Default, |file| {
//!     use std::io::Write as _;
//!     file.write_all(b"{}")
//! })?;
//! # Ok(())
//! # }
//! ```

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(windows)]
mod windows;

/// Who may read the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Access {
    /// The system's default for a new file.
    #[default]
    Default,
    /// Only the user (mode 0600 on Unix). Windows keeps a user's profile
    /// private already, so there it is the same as `Default`.
    Private,
}

/// Replaces `path` with `contents`, creating its directory if needed.
pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    write_with(path, Access::Default, |file| {
        file.write_all(contents.as_ref())
    })
}

/// Replaces `path` with `contents` that only the user may read.
pub fn write_private(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    write_with(path, Access::Private, |file| {
        file.write_all(contents.as_ref())
    })
}

/// Replaces `path` with what `fill` writes into a fresh file. If `fill` or
/// anything after it fails, `path` keeps its old contents and the temporary
/// file is removed.
pub fn write_with(
    path: &Path,
    access: Access,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let temporary = temporary_path(path);
    let result = (|| {
        let mut file = create(&temporary, access)?;
        fill(&mut file)?;
        file.flush()?;
        // The contents reach the disk before the move, or a crash just after
        // it could leave the new name pointing at nothing.
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// A name beside `path` that no other write, in this process or another,
/// uses at the same time: `.<name>.<pid>.<n>.tmp`. Hidden, so a directory
/// listing does not show it while it lives.
fn temporary_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

fn create(path: &Path, access: Access) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if access == Access::Private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = access;
    options.open(path)
}

/// Moves `temporary` over `path` in one step.
#[cfg(not(windows))]
fn replace(temporary: &Path, path: &Path) -> io::Result<()> {
    std::fs::rename(temporary, path)
}

#[cfg(windows)]
fn replace(temporary: &Path, path: &Path) -> io::Result<()> {
    windows::replace(temporary, path)
}

/// Makes the move itself durable: on Unix a rename lives in the directory,
/// which has to reach the disk too. Windows' write-through move covers it.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn a_file_is_written_and_then_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        write(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        write(&path, "second, longer").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second, longer");
        write(&path, "3").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "3", "no tail left");
        assert!(leftovers(path.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_failed_write_keeps_the_old_contents_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        write(&path, "kept").unwrap();
        let failed = write_with(&path, Access::Default, |file| {
            file.write_all(b"half")?;
            Err(io::Error::other("the serializer gave up"))
        });
        assert_eq!(failed.unwrap_err().to_string(), "the serializer gave up");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn writes_at_the_same_time_never_share_a_temporary_file() {
        let path = Path::new("/x/settings.json");
        let (a, b) = (temporary_path(path), temporary_path(path));
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".settings.json.") && name.ends_with(".tmp"));
        assert_eq!(a.parent(), path.parent(), "beside the file, on its disk");
    }

    #[test]
    fn a_bare_file_name_is_written_in_the_current_directory() {
        assert_eq!(temporary_path(Path::new("x")).parent(), Some(Path::new("")));
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_readable_by_the_user_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        write_private(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
