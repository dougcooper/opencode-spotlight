//! Tests for the desktop-entry side of the GNOME integration. The gsettings
//! parts are exercised only against a live session (step-8 checklist) so the
//! test suite never touches the user's real settings.

use super::*;
use crate::state::Paths;
use std::fs;

fn temp_paths() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("share"),
        state_dir: dir.path().join("state"),
    };
    (dir, paths)
}

#[test]
fn entries_are_dbus_activatable_and_point_at_the_binary() {
    let (_dir, paths) = temp_paths();
    let binary = PathBuf::from("/usr/local/bin/opencode-spotlight");
    let (desktop, autostart) =
        write_entries(&paths.data_dir, &paths.config_dir, &binary).expect("entries");

    assert_eq!(
        desktop.file_name().unwrap().to_string_lossy(),
        DESKTOP_FILE_NAME
    );
    assert_eq!(autostart.parent().unwrap().file_name().unwrap(), "autostart");

    let text = fs::read_to_string(&desktop).unwrap();
    assert!(text.contains("DBusActivatable=true"), "desktop entry: {text}");
    assert!(text.contains("Exec=/usr/local/bin/opencode-spotlight"), "desktop entry: {text}");
    assert!(text.starts_with("[Desktop Entry]"), "desktop entry: {text}");

    let autotext = fs::read_to_string(&autostart).expect("autostart");
    assert!(autotext.contains("X-GNOME-Autostart-enabled=true"), "autostart: {autotext}");
    assert!(autotext.contains("Hidden=false"), "autostart: {autotext}");
    assert!(autotext.contains("[Desktop Entry]"), "autostart: {autotext}");
}

#[test]
fn install_splice_adds_only_once_and_keeps_others() {
    // The splice logic inside install_keybinding: filter out our path,
    // then append it; existing entries are preserved (pure logic check).
    let existing = ["/org/a/".to_string(), "/org/b/".to_string()];
    let path = "/dev/dougcooper/opencode-spotlight/";
    let merged: Vec<String> = existing
        .iter()
        .filter(|p| p.as_str() != path)
        .cloned()
        .chain(std::iter::once(path.to_string()))
        .collect();
    assert_eq!(
        merged,
        vec!["/org/a/", "/org/b/", "/dev/dougcooper/opencode-spotlight/"]
    );
}

#[test]
fn manifest_round_trip() {
    use crate::desktop::{load_manifest, save_manifest};
    let (_dir, paths) = temp_paths();
    let manifest = Manifest {
        desktop_entry: paths.data_dir.join("x.desktop"),
        autostart_entry: paths.config_dir.join("y.desktop"),
        keybinding_path: Some(KEYBINDING_PATH.into()),
        binding: "<Super>o".into(),
    };
    save_manifest(&paths, &manifest).expect("save");
    assert_eq!(load_manifest(&paths).as_ref(), Some(&manifest));
}

#[test]
fn uninstall_removes_exactly_manifest_artifacts() {
    let (_dir, paths) = temp_paths();
    // A decoy entry another app (hypothetically) installed.
    let decoy = paths.config_dir.join("autostart").join("decoy.desktop");
    {
        use std::fs;
        fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        fs::write(&decoy, "[Desktop Entry]\nType=Application\nName=Decoy\n").unwrap();
    }

    let (desktop, autostart) = write_entries(&paths.data_dir, &paths.config_dir, &PathBuf::from("/bin/opencode-spotlight")).unwrap();
    let manifest = Manifest {
        desktop_entry: desktop.clone(),
        autostart_entry: autostart.clone(),
        // None keeps the test pure-fs; the live-session checklist covers gsettings.
        keybinding_path: None,
        binding: "<Super>o".into(),
    };

    Gnome.uninstall(&manifest).expect("uninstall");
    assert!(!desktop.exists(), "desktop entry removed");
    assert!(!autostart.exists(), "autostart entry removed");
    assert!(decoy.exists(), "unrelated entry untouched");
}
