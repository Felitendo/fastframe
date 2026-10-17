//! XDG autostart, and the background portal inside a Flatpak.

use std::io;
use std::path::{Path, PathBuf};

use crate::{Autostart, Start, State, desktop};

pub(crate) fn state(app: &Autostart) -> State {
    match entry_path(app, config_home().as_deref()) {
        Some(path) => read(&path, app),
        None => State::Unsupported,
    }
}

pub(crate) fn set(app: &Autostart, start: Option<Start>) -> io::Result<State> {
    let path = entry_path(app, config_home().as_deref())
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no configuration folder"))?;
    let executable = executable()?;
    let command = start.map(|start| app.command(executable, start));
    if std::env::var_os("FLATPAK_ID").is_some() {
        // The host's autostart folder is out of reach: the portal writes
        // the entry there. The sandbox's own copy below is the record the
        // state is read from, since the portal cannot be asked.
        portal::request(app, command.as_deref())?;
    }
    write(&path, app, command.as_deref())?;
    Ok(read(&path, app))
}

/// `$XDG_CONFIG_HOME`, or `~/.config` when it is unset or relative. Inside a
/// Flatpak this is the sandbox's own configuration folder.
fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".config"))
        })
}

fn entry_path(app: &Autostart, config: Option<&Path>) -> Option<PathBuf> {
    Some(
        config?
            .join("autostart")
            .join(format!("{}.desktop", app.id)),
    )
}

/// The file to run. An AppImage runs from a mount that changes each time,
/// so its own path is the one to start; inside a Flatpak the portal runs
/// the sandbox's executable.
fn executable() -> io::Result<String> {
    let path = match std::env::var_os("APPIMAGE") {
        Some(image) => PathBuf::from(image),
        None => std::env::current_exe()?,
    };
    Ok(path.to_string_lossy().into_owned())
}

fn read(path: &Path, app: &Autostart) -> State {
    match std::fs::read_to_string(path) {
        Ok(text) => desktop::state(&text, &app.hidden_argument),
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                log::debug!("cannot read {}: {error}", path.display());
            }
            State::Off
        }
    }
}

/// Writes the entry that runs `command`, atomically, or removes it.
fn write(path: &Path, app: &Autostart, command: Option<&[String]>) -> io::Result<()> {
    match command {
        Some(command) => {
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder)?;
            }
            fastframe_file::write(path, desktop::entry(app, command))
        }
        None => match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

mod portal {
    //! `org.freedesktop.portal.Background.RequestBackground`.

    use std::collections::HashMap;
    use std::io;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    use crate::Autostart;

    const DESKTOP: &str = "org.freedesktop.portal.Desktop";
    const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";

    /// Asks the portal to start the sandbox's `command` at login, or not
    /// with `None`, and waits for its answer, which may follow a question to
    /// the person.
    pub(super) fn request(app: &Autostart, command: Option<&[String]>) -> io::Result<()> {
        let connection = Connection::session().map_err(io::Error::other)?;
        let token = format!("fastframe_autostart_{}", std::process::id());
        let sender = connection
            .unique_name()
            .map(|name| crate::desktop::request_sender(name.as_str()))
            .ok_or_else(|| io::Error::other("no unique bus name"))?;
        // Subscribe before asking, so a quick answer is not missed.
        let path = format!("{DESKTOP_PATH}/request/{sender}/{token}");
        let request = Proxy::new(
            &connection,
            DESKTOP,
            path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .map_err(io::Error::other)?;
        let mut responses = request
            .receive_signal("Response")
            .map_err(io::Error::other)?;
        let background = Proxy::new(
            &connection,
            DESKTOP,
            DESKTOP_PATH,
            "org.freedesktop.portal.Background",
        )
        .map_err(io::Error::other)?;
        let reason = format!("Start {} when you log in", app.name);
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("reason", Value::from(reason.as_str()));
        options.insert("autostart", Value::from(command.is_some()));
        if let Some(command) = command {
            options.insert("commandline", Value::from(command.to_vec()));
        }
        options.insert("dbus-activatable", Value::from(false));
        let _handle: OwnedObjectPath = background
            .call("RequestBackground", &("", options))
            .map_err(io::Error::other)?;
        let message = responses
            .next()
            .ok_or_else(|| io::Error::other("the portal did not answer"))?;
        let (code, results): (u32, HashMap<String, OwnedValue>) =
            message.body().deserialize().map_err(io::Error::other)?;
        let granted = results
            .get("autostart")
            .and_then(|value| bool::try_from(value).ok());
        crate::desktop::portal_answer(code, granted, command.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spotifast() -> Autostart {
        Autostart::new("spotifast", "Spotifast")
    }

    #[test]
    fn the_entry_lives_in_the_autostart_folder_under_the_apps_id() {
        assert_eq!(
            entry_path(&spotifast(), Some(Path::new("/home/a/.config"))),
            Some(PathBuf::from("/home/a/.config/autostart/spotifast.desktop"))
        );
        assert_eq!(entry_path(&spotifast(), None), None);
    }

    #[test]
    fn writing_reading_and_removing_the_entry_in_a_folder() -> io::Result<()> {
        let config = tempfile::tempdir()?;
        let app = spotifast();
        let path = entry_path(&app, Some(config.path())).expect("a path");
        assert_eq!(read(&path, &app), State::Off, "nothing there yet");
        let hidden = app.command("/usr/bin/spotifast".into(), Start::Hidden);
        write(&path, &app, Some(&hidden))?;
        assert_eq!(read(&path, &app), State::On(Start::Hidden));
        let window = app.command("/usr/bin/spotifast".into(), Start::Window);
        write(&path, &app, Some(&window))?;
        assert_eq!(read(&path, &app), State::On(Start::Window));
        write(&path, &app, None)?;
        assert!(!path.exists());
        assert_eq!(read(&path, &app), State::Off);
        write(&path, &app, None)?;
        Ok(())
    }
}
