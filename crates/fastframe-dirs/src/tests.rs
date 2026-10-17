use super::*;

const SLUG: &str = "sample";

/// A portable copy unpacked into `root/app`, with its marker, and per-user
/// folders under `root/user` holding settings, state and a cache.
struct Fixture {
    _root: tempfile::TempDir,
    executable: PathBuf,
    installed: Dirs,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("app");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("sample-portable.txt"), "sample-portable-v1\n").unwrap();
        let installed = Dirs::inside(&root.path().join("user"));
        fs::create_dir_all(installed.config.join("themes")).unwrap();
        fs::write(installed.config.join("settings.json"), "{\"volume\":3}").unwrap();
        fs::write(installed.config.join("themes").join("dusk.json"), "{}").unwrap();
        fs::create_dir_all(installed.state.join("instance")).unwrap();
        fs::write(installed.state.join("instance").join("instance.lock"), "").unwrap();
        fs::write(installed.state.join("session.json"), "signed in").unwrap();
        fs::write(installed.state.join("sample.log"), "old run").unwrap();
        fs::create_dir_all(&installed.cache).unwrap();
        fs::write(installed.cache.join("art.bin"), "rebuildable").unwrap();
        Self {
            executable: app.join("sample.exe"),
            installed,
            _root: root,
        }
    }

    fn app(&self) -> &Path {
        self.executable.parent().unwrap()
    }

    fn folder(&self) -> PathBuf {
        self.app().join("sample-data")
    }

    fn portable(&self) -> Portable {
        portable_for(SLUG, &self.executable, |path| fs::read_to_string(path).ok())
    }

    fn discover(&self) -> Discovery {
        settle(
            Some(self.installed.clone()),
            self.portable(),
            &["instance", "sample.log"],
            |_, _| Ok(()),
        )
    }

    /// Every file under `root`, relative, with its contents.
    fn files(root: &Path) -> Vec<(String, String)> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let name = path.strip_prefix(root).unwrap().to_string_lossy();
                    files.push((name.replace('\\', "/"), fs::read_to_string(&path).unwrap()));
                }
            }
        }
        files.sort();
        files
    }
}

fn read_from(files: &[(&str, &str)]) -> impl Fn(&Path) -> Option<String> {
    let files: Vec<(PathBuf, String)> = files
        .iter()
        .map(|(path, contents)| (PathBuf::from(path), (*contents).to_owned()))
        .collect();
    move |path| {
        files
            .iter()
            .find(|(file, _)| file == path)
            .map(|(_, contents)| contents.clone())
    }
}

#[test]
fn a_copy_is_portable_only_with_a_valid_portable_marker() {
    let executable = Path::new("/apps/sample/sample.exe");
    let marker = "/apps/sample/sample-portable.txt";
    let folder = Some(PathBuf::from("/apps/sample/sample-data"));
    assert_eq!(portable_folder(SLUG, executable, read_from(&[])), None);
    for contents in [
        "sample-portable-v1",
        "sample-portable-v1\r\n",
        " sample-portable-v1 \n",
    ] {
        assert_eq!(
            portable_folder(SLUG, executable, read_from(&[(marker, contents)])),
            folder,
            "{contents:?}"
        );
    }
    for contents in [
        "",
        "sample-portable-v2",
        "other-portable-v1",
        "sample-installer-v1",
    ] {
        assert_eq!(
            portable_folder(SLUG, executable, read_from(&[(marker, contents)])),
            None,
            "{contents:?}"
        );
    }
    // Another app's marker beside it does not count.
    assert_eq!(
        portable_folder(
            SLUG,
            executable,
            read_from(&[("/apps/sample/other-portable.txt", "other-portable-v1")])
        ),
        None
    );
}

#[test]
fn an_installer_marker_wins_as_it_does_for_the_updater() {
    let executable = Path::new("/apps/sample/sample.exe");
    let read = read_from(&[
        ("/apps/sample/sample-portable.txt", "sample-portable-v1"),
        ("/apps/sample/sample-installer.txt", "sample-installer-v1"),
    ]);
    assert_eq!(portable_folder(SLUG, executable, read), None);
}

#[test]
fn markers_name_the_app_and_the_kind() {
    let read = read_from(&[("/d/app-installer.txt", "app-installer-v1")]);
    assert!(marker(Path::new("/d"), "app", "installer", &read));
    assert!(!marker(Path::new("/d"), "app", "portable", &read));
    assert!(!marker(Path::new("/e"), "app", "installer", &read));
}

#[test]
fn an_installed_copy_uses_the_per_user_folders() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.app().join("sample-portable.txt")).unwrap();
    let found = fixture.discover();
    assert!(matches!(found.mode, Mode::Installed));
    assert_eq!(found.dirs, Some(fixture.installed.clone()));
    assert!(!fixture.folder().exists());
}

#[test]
fn a_first_portable_start_copies_settings_and_state_but_not_caches() {
    let fixture = Fixture::new();
    let before = Fixture::files(fixture.installed.config.parent().unwrap());
    let found = fixture.discover();
    let folder = fixture.folder();
    let Mode::Portable {
        folder: used,
        copied_from,
    } = &found.mode
    else {
        panic!("{:?}", found.mode);
    };
    assert_eq!(used, &folder);
    assert_eq!(copied_from.as_ref(), Some(&fixture.installed));
    assert_eq!(found.dirs, Some(Dirs::inside(&folder)));
    assert_eq!(
        Fixture::files(&folder),
        [
            (
                "config/settings.json".to_owned(),
                "{\"volume\":3}".to_owned()
            ),
            ("config/themes/dusk.json".to_owned(), "{}".to_owned()),
            ("state/session.json".to_owned(), "signed in".to_owned()),
        ]
    );
    assert!(folder.join("cache").is_dir());
    // The installed copy keeps its own, untouched.
    assert_eq!(
        Fixture::files(fixture.installed.config.parent().unwrap()),
        before
    );
    assert!(!fixture.app().join("sample-data.partial").exists());
}

#[test]
fn a_later_start_keeps_the_portable_folder_as_it_is() {
    let fixture = Fixture::new();
    fixture.discover();
    let settings = fixture.folder().join("config").join("settings.json");
    fs::write(&settings, "{\"volume\":9}").unwrap();
    fs::write(fixture.installed.config.join("settings.json"), "changed").unwrap();
    let found = fixture.discover();
    assert!(matches!(
        found.mode,
        Mode::Portable {
            copied_from: None,
            ..
        }
    ));
    assert_eq!(fs::read_to_string(settings).unwrap(), "{\"volume\":9}");
}

#[test]
fn a_portable_copy_without_per_user_folders_starts_empty() {
    let fixture = Fixture::new();
    let found = settle(None, fixture.portable(), &[], |_, _| Ok(()));
    assert!(matches!(
        found.mode,
        Mode::Portable {
            copied_from: None,
            ..
        }
    ));
    assert!(Fixture::files(&fixture.folder()).is_empty());
    assert!(fixture.folder().join("state").is_dir());
}

#[test]
fn an_interrupted_copy_is_made_again() {
    let fixture = Fixture::new();
    let partial = fixture.app().join("sample-data.partial");
    fs::create_dir_all(partial.join("state")).unwrap();
    fs::write(partial.join("state").join("half.json"), "{").unwrap();
    let found = fixture.discover();
    assert!(matches!(
        found.mode,
        Mode::Portable {
            copied_from: Some(_),
            ..
        }
    ));
    assert!(!fixture.folder().join("state").join("half.json").exists());
    assert!(
        fixture
            .folder()
            .join("state")
            .join("session.json")
            .is_file()
    );
    assert!(!partial.exists());
}

#[test]
fn a_failed_copy_falls_back_and_is_tried_again() {
    let fixture = Fixture::new();
    let found = settle(
        Some(fixture.installed.clone()),
        fixture.portable(),
        &[],
        |_, _| Err(io::Error::other("no room")),
    );
    let Mode::Fallback { folder, error } = &found.mode else {
        panic!("{:?}", found.mode);
    };
    assert_eq!(folder, &fixture.folder());
    assert_eq!(error.to_string(), "no room");
    assert_eq!(found.dirs, Some(fixture.installed.clone()));
    assert!(!fixture.folder().exists());
    assert!(!fixture.app().join("sample-data.partial").exists());
    assert!(matches!(
        fixture.discover().mode,
        Mode::Portable {
            copied_from: Some(_),
            ..
        }
    ));
}

#[test]
fn adapt_sees_the_copy_and_its_source_before_it_is_in_place() {
    let fixture = Fixture::new();
    let folder = fixture.folder();
    settle(
        Some(fixture.installed.clone()),
        fixture.portable(),
        &[],
        |copy, from| {
            assert!(!folder.exists());
            assert_eq!(from, &fixture.installed);
            assert!(copy.state.join("session.json").is_file());
            fs::write(copy.state.join("profile"), "kept")
        },
    );
    assert_eq!(
        fs::read_to_string(folder.join("state").join("profile")).unwrap(),
        "kept"
    );
}

#[test]
fn an_unwritable_folder_falls_back_to_the_per_user_folders() {
    let fixture = Fixture::new();
    // Where the data folder would go is a file, so nothing can be made there
    // on any platform.
    let blocked = fixture.app().join("blocked");
    fs::write(&blocked, "").unwrap();
    let executable = blocked.join("sample.exe");
    let read = |path: &Path| {
        (path == blocked.join("sample-portable.txt")).then(|| "sample-portable-v1".to_owned())
    };
    let portable = portable_for(SLUG, &executable, read);
    assert!(
        matches!(portable, Portable::Unwritable { .. }),
        "{portable:?}"
    );
    assert_eq!(portable.instance_scope(), None);
    let found = settle(
        Some(fixture.installed.clone()),
        portable,
        &[],
        |_, _| Ok(()),
    );
    assert!(matches!(found.mode, Mode::Fallback { .. }));
    assert_eq!(found.dirs, Some(fixture.installed.clone()));
}

#[test]
fn a_file_named_like_the_data_folder_is_not_used() {
    let fixture = Fixture::new();
    fs::write(fixture.folder(), "").unwrap();
    assert!(matches!(fixture.portable(), Portable::Unwritable { .. }));
}

#[test]
fn the_write_test_leaves_nothing_behind() {
    let fixture = Fixture::new();
    let before = Fixture::files(fixture.app());
    assert!(matches!(fixture.portable(), Portable::Folder(_)));
    assert_eq!(Fixture::files(fixture.app()), before);
    fixture.discover();
    assert!(matches!(fixture.portable(), Portable::Folder(_)));
    assert!(fs::read_dir(fixture.folder()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("write-test")
    }));
}

#[test]
fn a_portable_copy_inside_its_per_user_folders_is_not_filled_from_them() {
    let fixture = Fixture::new();
    let app = fixture.installed.state.join("unpacked");
    fs::create_dir(&app).unwrap();
    fs::write(app.join("sample-portable.txt"), "sample-portable-v1").unwrap();
    let portable = portable_for(SLUG, &app.join("sample.exe"), |path| {
        fs::read_to_string(path).ok()
    });
    let found = settle(
        Some(fixture.installed.clone()),
        portable,
        &[],
        |_, _| Ok(()),
    );
    assert!(
        matches!(found.mode, Mode::Fallback { .. }),
        "{:?}",
        found.mode
    );
    assert!(!app.join("sample-data").exists());
    assert!(!app.join("sample-data.partial").exists());
}

#[test]
fn each_portable_folder_has_a_stable_instance_scope_of_its_own() {
    let scope = |path: &str| Portable::Folder(PathBuf::from(path)).instance_scope();
    let usb = scope(r"E:\Spotifast\spotifast-data").unwrap();
    assert_eq!(usb, scope(r"e:\spotifast\SPOTIFAST-DATA").unwrap());
    assert_ne!(usb, scope(r"D:\Spotifast\spotifast-data").unwrap());
    // Fixed forever: a newer version must reach an older one running there.
    assert_eq!(
        scope("C:/portable/sample-data").unwrap(),
        format!("portable-{:016x}", fnv1a("c:/portable/sample-data"))
    );
    assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    assert!(usb.starts_with("portable-") && usb.len() == "portable-".len() + 16);
    assert_eq!(Portable::No.instance_scope(), None);
}

#[test]
fn another_spelling_of_the_same_folder_finds_the_same_slot() {
    let fixture = Fixture::new();
    let scope = |folder: PathBuf| Portable::Folder(folder).instance_scope().unwrap();
    let direct = scope(fixture.folder());
    let roundabout = fixture.app().join("..").join("app").join("sample-data");
    assert_eq!(scope(roundabout), direct);
    #[cfg(unix)]
    {
        let link = fixture.app().with_file_name("link");
        std::os::unix::fs::symlink(fixture.app(), &link).unwrap();
        assert_eq!(scope(link.join("sample-data")), direct);
    }
}

#[test]
fn portable_folders_are_never_used_outside_windows() {
    let app = App::new(SLUG, "rocks", "sample", "Sample");
    if !cfg!(windows) {
        assert!(matches!(app.portable(), Portable::No));
    }
}

#[test]
fn a_portable_data_folder_holds_config_state_and_cache() {
    let dirs = Dirs::inside(Path::new("/p/sample-data"));
    assert_eq!(dirs.config, Path::new("/p/sample-data/config"));
    assert_eq!(dirs.state, Path::new("/p/sample-data/state"));
    assert_eq!(dirs.cache, Path::new("/p/sample-data/cache"));
}
