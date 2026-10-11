# fastframe-file

Replaces a file on disk without ever leaving it half written.

Settings, sessions and credentials are rewritten whole. Writing over the file
in place leaves it truncated if the app is killed or the power goes halfway
through, and the app then starts from defaults. `write` writes a temporary
file beside the target instead, flushes it to the disk, and moves it over the
target in one step, so a reader sees the old contents or the new ones, never
part of either.

## Usage

```rust
fastframe_file::write(&dirs.settings_file(), serde_json::to_vec_pretty(&settings)?)?;

// A secret only the user may read (0600 on Unix):
fastframe_file::write_private(&dirs.token_file(), token.as_bytes())?;

// Or stream into the file, from a serializer say:
fastframe_file::write_with(&path, fastframe_file::Access::Default, |file| {
    serde_json::to_writer(file, &session).map_err(std::io::Error::other)
})?;
```

## What it does

1. Creates the file's directory if it is missing.
2. Writes into `.<name>.<pid>.<n>.tmp` beside the file, a name no other write
   uses at the same time, created fresh (with mode 0600 for a private file).
3. Flushes it to the disk, so a crash right after the move cannot leave the
   name pointing at nothing.
4. Moves it over the file: `rename` on Unix, and on Windows `MoveFileExW`
   with write-through, so the move itself is on the disk when it returns.
5. On Unix, syncs the directory, where the rename lives.

If writing or any later step fails, the file keeps its old contents and the
temporary file is removed.

It came from Spotifast's saves (the Windows write-through move), Solco's
licence file (the flush, the directory sync and the private mode), and
ZapFast's settings, which wrote and renamed without flushing.
