# fastframe-dirs

An app's config, state and cache folders: the platform's per-user folders, or,
for a portable copy on Windows, a folder next to the executable.

A portable copy is an executable unpacked from the Windows `.zip` download,
next to its `<slug>-portable.txt` marker (the file `fastframe-update` reads to
decide that the copy may replace itself). It keeps everything in
`<slug>-data` beside the executable:

```text
Spotifast\
  spotifast.exe
  spotifast-portable.txt
  spotifast-data\
    config\   settings, and files the user adds
    state\    sessions, history, logs
    cache\    disposable caches
```

An installed copy on the same machine keeps the per-user folders, and the two
never share files. Updates replace only the executable, so the data folder
survives them, and their rollback, untouched.

## Usage

```rust
use fastframe_dirs::{App, Mode};

const DIRS: App = App {
    // Left out of the first copy: a lock a running installed copy holds
    // (Windows refuses to read it), and the logs of its runs.
    not_copied: &["instance", "spotifast.log", "panic.log"],
    ..App::new("spotifast", "me", "paolino", "spotifast")
};

// Before claiming the single-instance slot: cheap, writes nothing that stays.
let scope = DIRS.portable().instance_scope();

// Once this process is the running copy, before the window opens:
let found = DIRS.discover(|copy, from| {
    // Adjust the first copy before it is put in place, if the app must.
    Ok(())
});
let dirs = found.dirs.unwrap_or_else(fallback);
// After logging starts, log `found.mode`.
```

## Behaviour

- **Which folders.** On Windows, a valid `<slug>-portable.txt` beside the
  running executable, and no `<slug>-installer.txt` (which the updater takes
  first), makes it portable: `Dirs::inside(<exe folder>\<slug>-data)`.
  Otherwise, and always on Linux and macOS, `Dirs::platform`: the config,
  state (local data on Windows and macOS) and cache folders of
  `directories::ProjectDirs`.
- **The first portable start copies, never moves.** When `<slug>-data` does
  not exist yet, the per-user config and state folders are copied into it,
  except the state entries named in `not_copied`. Caches are not copied. The
  copy is made in `<slug>-data.partial` and renamed to `<slug>-data` once it
  is complete; a copy that was interrupted is removed and made again at the
  next start. The per-user folders are only read.
- **`adapt`** runs on the finished copy before the rename, with the folders
  being filled and the folders they were copied from, for an app that ties
  something to its folder's path (Spotifast records the credential profile
  its sign-in is stored under).
- **Unwritable folders.** A portable copy that cannot create a file in its
  data folder, or beside it before it exists (Program Files, read-only
  media), uses the per-user folders, as does one whose first copy failed.
  `Mode::Fallback` carries the folder and the error, for the log; the copy is
  tried again at the next start.
- **One running copy per folder.** `Portable::instance_scope` names a
  single-instance slot for the portable folder, stable across versions, so a
  portable copy never hands its launches to an installed copy or to a
  portable copy in another folder, which keep different data. Keep the slot
  itself in the user's profile (`fastframe_instance::Slot::scoped`), where
  other users cannot read the token that guards it, rather than in the
  portable folder, which may be on a drive without permissions.
- **Linux stays put.** The Linux `.tar.gz` carries the portable marker too,
  but only so the updater may replace the executable. People unpack it into
  their home and expect the XDG folders, which `XDG_CONFIG_HOME`,
  `XDG_STATE_HOME` and `XDG_CACHE_HOME` already relocate, so it keeps them.

`marker` is the marker check itself, which `fastframe-update` shares, so a
copy the updater treats as portable is the copy that keeps its data beside
it.

## What the app keeps

- Whatever is not a file in these folders. Windows Credential Manager, the
  macOS Keychain and the Secret Service are per user, not per folder: a
  portable copy's secrets stay on the machine where they were saved.
- Its own fallback when the user has no home folder (`Discovery::dirs` is
  `None`).
