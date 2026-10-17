//! An app's config, state and cache folders.
//!
//! An installed app keeps them in the platform's per-user folders. A portable
//! copy on Windows (an executable unpacked from the `.zip` download, next to
//! a `<slug>-portable.txt` marker) keeps them in `<slug>-data` next to the
//! executable instead, so the folder carries the app and everything it
//! remembers, and an installed copy on the same machine keeps its own.
//!
//! The first time a portable copy starts without that folder, it gets a copy
//! (never a move) of the settings and state the per-user folders hold, so a
//! user who ran the portable copy before, when it still used them, keeps
//! their settings and sign-in. Caches are not copied: the app rebuilds them.
//! The copy is made in `<slug>-data.partial` and renamed into place when it
//! is complete, so a copy that was interrupted is made again at the next
//! start rather than used half done.
//!
//! A portable copy whose folder cannot be written (unpacked into Program
//! Files, say, or onto read-only media) uses the per-user folders, and
//! [`Mode::Fallback`] says why.
//!
//! ```no_run
//! use fastframe_dirs::{App, Mode};
//!
//! const DIRS: App = App {
//!     // Entries of the state folder a new portable folder does not copy:
//!     // the single-instance lock of a copy that may be running, the logs.
//!     not_copied: &["instance", "example.log"],
//!     ..App::new("example", "rocks", "example", "Example")
//! };
//!
//! // After this process became the running copy, before its window opens:
//! let found = DIRS.discover(|_copy, _from| Ok(()));
//! let dirs = found.dirs.expect("a home folder");
//! // Once logging is set up, say which folders and why.
//! match &found.mode {
//!     Mode::Installed => {}
//!     Mode::Portable { folder, copied_from } => { /* folder, and what it copied */ }
//!     Mode::Fallback { folder, error } => { /* why `folder` is not used */ }
//! }
//! ```
//!
//! Linux and macOS always use the per-user folders. The Linux `.tar.gz`
//! carries the portable marker too, but only so the updater may replace the
//! executable: people unpack it into their home and expect the XDG folders,
//! which `XDG_CONFIG_HOME` and its siblings already relocate.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where an app keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dirs {
    /// Settings, and files the user adds, such as themes.
    pub config: PathBuf,
    /// What the app remembers that is not configuration: sessions, history,
    /// logs.
    pub state: PathBuf,
    /// Disposable caches the app can rebuild.
    pub cache: PathBuf,
}

impl Dirs {
    /// The platform's per-user folders for an app, as
    /// [`directories::ProjectDirs`] names them: the config folder, the state
    /// folder (the local data folder where the platform has none, as on
    /// Windows and macOS), and the cache folder. On Windows these are
    /// `%APPDATA%\<organization>\<application>\config`,
    /// `%LOCALAPPDATA%\<organization>\<application>\data` and
    /// `%LOCALAPPDATA%\<organization>\<application>\cache`. `None` when the
    /// user has no home folder.
    #[must_use]
    pub fn platform(qualifier: &str, organization: &str, application: &str) -> Option<Self> {
        let project = directories::ProjectDirs::from(qualifier, organization, application)?;
        Some(Self {
            config: project.config_dir().to_path_buf(),
            state: project
                .state_dir()
                .unwrap_or_else(|| project.data_local_dir())
                .to_path_buf(),
            cache: project.cache_dir().to_path_buf(),
        })
    }

    /// `config`, `state` and `cache` inside `root`: the layout of a portable
    /// data folder.
    #[must_use]
    pub fn inside(root: &Path) -> Self {
        Self {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        }
    }
}

/// An app whose folders follow a portable copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct App {
    /// The name of the marker files and the data folder: `<slug>-portable.txt`
    /// and `<slug>-data`, as `fastframe-update` names its markers.
    pub slug: &'static str,
    /// The reverse-domain qualifier [`Dirs::platform`] takes (used on macOS).
    pub qualifier: &'static str,
    /// The organization [`Dirs::platform`] takes.
    pub organization: &'static str,
    /// The application name [`Dirs::platform`] takes.
    pub application: &'static str,
    /// Names directly inside the state folder that a new portable folder
    /// does not copy, such as the single-instance lock of a copy that may be
    /// running (Windows refuses to read a locked file) and the logs of past
    /// runs.
    pub not_copied: &'static [&'static str],
}

impl App {
    /// An app that copies all of its state into a new portable folder.
    #[must_use]
    pub const fn new(
        slug: &'static str,
        qualifier: &'static str,
        organization: &'static str,
        application: &'static str,
    ) -> Self {
        Self {
            slug,
            qualifier,
            organization,
            application,
            not_copied: &[],
        }
    }

    /// The per-user folders, which an installed copy uses, and where a new
    /// portable folder copies from.
    #[must_use]
    pub fn installed(&self) -> Option<Dirs> {
        Dirs::platform(self.qualifier, self.organization, self.application)
    }

    /// Whether the running executable is a portable copy, and whether its
    /// data folder can be written. Cheap, and changes nothing on disk, so
    /// the app can call it before it knows it is the running copy (to pick
    /// a single-instance slot, say). Always [`Portable::No`] outside Windows.
    #[must_use]
    pub fn portable(&self) -> Portable {
        if !cfg!(windows) {
            return Portable::No;
        }
        match std::env::current_exe() {
            Ok(executable) => {
                portable_for(self.slug, &executable, |path| fs::read_to_string(path).ok())
            }
            Err(_) => Portable::No,
        }
    }

    /// The folders this copy uses. A portable copy's data folder is created
    /// here the first time, with a copy of the per-user settings and state.
    ///
    /// `adapt` runs on that copy before it is put in place, with the folders
    /// being filled (inside `<slug>-data.partial`, so never record their
    /// paths) and the per-user folders it was copied from. An error there
    /// abandons the copy like any other, and the app uses the per-user
    /// folders this time.
    ///
    /// Call it once the process is the app's running copy, so two launches
    /// of one portable copy never fill the folder at once, and before the
    /// window opens: the copy waits for the disk.
    pub fn discover(&self, adapt: impl FnOnce(&Dirs, &Dirs) -> io::Result<()>) -> Discovery {
        settle(self.installed(), self.portable(), self.not_copied, adapt)
    }
}

/// What [`App::portable`] found.
#[derive(Debug)]
pub enum Portable {
    /// Not a portable copy: an installed one, or not on Windows.
    No,
    /// A portable copy whose data folder, `<slug>-data` next to the
    /// executable, can be written. It may not exist yet.
    Folder(PathBuf),
    /// A portable copy that cannot write its data folder.
    Unwritable {
        /// The data folder it would use.
        folder: PathBuf,
        /// Why it cannot.
        error: io::Error,
    },
}

impl Portable {
    /// The data folder of a usable portable copy.
    #[must_use]
    pub fn folder(&self) -> Option<&Path> {
        match self {
            Self::Folder(folder) => Some(folder),
            Self::No | Self::Unwritable { .. } => None,
        }
    }

    /// A name for a single-instance slot of this portable copy's own, such
    /// as `fastframe_instance::Slot::scoped` takes, so that it never hands
    /// its launches to an installed copy or to a portable copy in another
    /// folder, which keep other data. The same folder always gives the same
    /// name, in any version. `None` when the copy uses the per-user
    /// folders.
    #[must_use]
    pub fn instance_scope(&self) -> Option<String> {
        self.folder()
            .map(|folder| format!("portable-{:016x}", fnv1a(&folder_key(folder))))
    }
}

/// What [`App::discover`] decided.
#[derive(Debug)]
pub struct Discovery {
    /// The folders to use. `None` only when this is not a usable portable
    /// copy and the user has no home folder either.
    pub dirs: Option<Dirs>,
    /// Why these folders, for the log.
    pub mode: Mode,
}

/// Which folders a copy uses.
#[derive(Debug)]
pub enum Mode {
    /// The per-user folders.
    Installed,
    /// The portable data folder.
    Portable {
        /// The data folder.
        folder: PathBuf,
        /// The per-user folders it was filled from just now, the first time.
        copied_from: Option<Dirs>,
    },
    /// A portable copy that uses the per-user folders this time, because
    /// its own could not be written or filled. A copy that failed is tried
    /// again at the next start.
    Fallback {
        /// The data folder it would have used.
        folder: PathBuf,
        /// Why it did not.
        error: io::Error,
    },
}

/// Whether `<name>-<kind>.txt` in `directory` reads `<name>-<kind>-v1`: the
/// marker files the download page's copies carry (`portable` in the
/// archives, `installer` beside an installed copy). `read` returns a file's
/// contents, or `None` when it cannot be read.
pub fn marker(
    directory: &Path,
    name: &str,
    kind: &str,
    read: impl Fn(&Path) -> Option<String>,
) -> bool {
    read(&directory.join(format!("{name}-{kind}.txt")))
        .is_some_and(|value| value.trim() == format!("{name}-{kind}-v1"))
}

/// The data folder `executable` would use, when it is a portable copy: a
/// valid portable marker beside it, and no installer marker, which the
/// updater takes first.
fn portable_folder(
    slug: &str,
    executable: &Path,
    read: impl Fn(&Path) -> Option<String>,
) -> Option<PathBuf> {
    let directory = executable.parent()?;
    (!marker(directory, slug, "installer", &read) && marker(directory, slug, "portable", &read))
        .then(|| directory.join(format!("{slug}-data")))
}

fn portable_for(slug: &str, executable: &Path, read: impl Fn(&Path) -> Option<String>) -> Portable {
    let Some(folder) = portable_folder(slug, executable, read) else {
        return Portable::No;
    };
    match writable(&folder, slug) {
        Ok(()) => Portable::Folder(folder),
        Err(error) => Portable::Unwritable { folder, error },
    }
}

/// Whether a file can be made in `folder`, or beside it while it does not
/// exist yet. Leaves nothing behind.
fn writable(folder: &Path, slug: &str) -> io::Result<()> {
    let directory = match fs::metadata(folder) {
        Ok(metadata) if metadata.is_dir() => folder,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a file has the data folder's name",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            folder.parent().ok_or(io::ErrorKind::InvalidInput)?
        }
        Err(error) => return Err(error),
    };
    let probe = directory.join(format!(".{slug}-write-test-{}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {}
        // Left by an earlier process with this id.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    fs::remove_file(&probe)
}

fn settle(
    installed: Option<Dirs>,
    portable: Portable,
    not_copied: &[&str],
    adapt: impl FnOnce(&Dirs, &Dirs) -> io::Result<()>,
) -> Discovery {
    match portable {
        Portable::No => Discovery {
            dirs: installed,
            mode: Mode::Installed,
        },
        Portable::Unwritable { folder, error } => Discovery {
            dirs: installed,
            mode: Mode::Fallback { folder, error },
        },
        Portable::Folder(folder) => match prepare(&folder, installed.as_ref(), not_copied, adapt) {
            Ok(copied_from) => Discovery {
                dirs: Some(Dirs::inside(&folder)),
                mode: Mode::Portable {
                    folder,
                    copied_from,
                },
            },
            Err(error) => Discovery {
                dirs: installed,
                mode: Mode::Fallback { folder, error },
            },
        },
    }
}

/// Makes the portable data folder when it does not exist yet, filled from
/// `from`, and returns `from` when it copied anything.
fn prepare(
    folder: &Path,
    from: Option<&Dirs>,
    not_copied: &[&str],
    adapt: impl FnOnce(&Dirs, &Dirs) -> io::Result<()>,
) -> io::Result<Option<Dirs>> {
    if folder.is_dir() {
        return Ok(None);
    }
    let mut partial = folder.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    // What an interrupted copy left.
    match fs::remove_dir_all(&partial) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let staged = Dirs::inside(&partial);
    let from = from.filter(|from| from.config.is_dir() || from.state.is_dir());
    if let Some(from) = from
        && [&from.config, &from.state]
            .iter()
            .any(|tree| partial.starts_with(tree))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the portable copy is inside its own per-user folders",
        ));
    }
    for directory in [&staged.config, &staged.state, &staged.cache] {
        fs::create_dir_all(directory)?;
    }
    if let Some(from) = from {
        let copied = copy_tree(&from.config, &staged.config, &[])
            .and_then(|()| copy_tree(&from.state, &staged.state, not_copied))
            .and_then(|()| adapt(&staged, from));
        if let Err(error) = copied {
            let _ = fs::remove_dir_all(&partial);
            return Err(error);
        }
    }
    if let Err(error) = fs::rename(&partial, folder) {
        let _ = fs::remove_dir_all(&partial);
        // Another launch finished first.
        if folder.is_dir() {
            return Ok(None);
        }
        return Err(error);
    }
    Ok(from.cloned())
}

/// Copies the files and folders in `from` into `to`, which exists, leaving
/// out `skip` at the top. Links are not followed or copied. A missing `from`
/// copies nothing.
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> io::Result<()> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if skip.iter().any(|skipped| name == *skipped) {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            fs::create_dir(&target)?;
            copy_tree(&entry.path(), &target, &[])?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The folder as Windows compares paths: without case.
fn folder_key(folder: &Path) -> String {
    folder.to_string_lossy().to_lowercase()
}

/// 64-bit FNV-1a: small, and the same in every build, which a slot name
/// shared by versions needs (`DefaultHasher` promises neither).
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests;
