//! The GNOME implementation of [`DesktopIntegration`]: a D-Bus activatable
//! desktop entry, an XDG autostart entry, and a GNOME custom keybinding
//! registered through `gsettings`.

use std::fs;
use std::path::PathBuf;


use super::{DesktopIntegration, IntegrationError, InstallOptions, Manifest};
use crate::state::Paths;

pub const DESKTOP_FILE_NAME: &str = "dev.dougcooper.opencode-spotlight.desktop";
pub const KEYBINDING_PATH: &str = "/dev/dougcooper/opencode-spotlight/";
const MEDIA_KEYS_LIST_KEY: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings";
const MEDIA_KEYS_SCHEMA_BASE: &str = "/org/gnome/settings-daemon/plugins/media-keys/";

const TOGGLE_COMMAND: &str = "opencode-spotlight --toggle";

pub struct Gnome;

fn desktop_entry_contents(binary_path: &str) -> String {
    format!(
        "[Desktop Entry]
Type=Application
Name=OpenCode Spotlight
Comment=Summon a quick OpenCode chat with a hotkey
Exec={binary_path}
Icon=utilities-terminal
Terminal=false
Categories=Utility;Development;
StartupNotify=true
DBusActivatable=true
"
    )
}

fn autostart_entry_contents(desktop_text: &str) -> String {
    format!(
        "{desktop_text}\nX-GNOME-Autostart-enabled=true\nHidden=false\n"
    )
}

/// Write both desktop entries; pure filesystem, unit-testable. They belong in
/// the REAL XDG locations (menu scans `~/.local/share/applications`, autostart
/// reads `~/.config/autostart`), not inside the app's private data dir — so
/// callers pass the XDG data/config roots (BaseDirs on the live path).
pub(crate) fn write_entries(
    data_root: &std::path::Path,
    config_root: &std::path::Path,
    binary_path: &std::path::Path,
) -> Result<(PathBuf, PathBuf), IntegrationError> {
    let desktop_text = desktop_entry_contents(&binary_path.to_string_lossy());
    let autostart_text = autostart_entry_contents(&desktop_text);

    let apps_dir = data_root.join("applications");
    fs::create_dir_all(&apps_dir)?;
    let desktop_entry = apps_dir.join(DESKTOP_FILE_NAME);
    fs::write(&desktop_entry, desktop_text)?;

    let autostart_dir = config_root.join("autostart");
    fs::create_dir_all(&autostart_dir)?;
    let autostart_entry = autostart_dir.join(DESKTOP_FILE_NAME);
    fs::write(&autostart_entry, autostart_text)?;

    Ok((desktop_entry, autostart_entry))
}

impl DesktopIntegration for Gnome {
    fn id(&self) -> &'static str {
        "gnome"
    }

    fn install(
        &self,
        _paths: &Paths,
        opts: &InstallOptions,
    ) -> Result<Manifest, IntegrationError> {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| IntegrationError::Other("XDG base dirs unresolvable".into()))?;
        let (desktop_entry, autostart_entry) =
            write_entries(base.data_dir(), base.config_dir(), opts.binary_path.as_path())?;

        // Best-effort: requires the GNOME schemas; entries above always land.
        let keybinding_path = match install_keybinding(&opts.binding) {
            Ok(path) => Some(path),
            Err(e) => {
                eprintln!("opencode-spotlight: keybinding registration skipped: {e}");
                None
            }
        };

        Ok(Manifest {
            desktop_entry,
            autostart_entry,
            keybinding_path,
            binding: opts.binding.clone(),
        })
    }

    fn uninstall(&self, manifest: &Manifest) -> Result<(), IntegrationError> {
        if manifest.desktop_entry.exists() {
            fs::remove_file(&manifest.desktop_entry)?;
        }
        if manifest.autostart_entry.exists() {
            fs::remove_file(&manifest.autostart_entry)?;
        }
        if let Some(path) = &manifest.keybinding_path {
            remove_keybinding(path);
        }
        Ok(())
    }
}

/// Reads the current GNOME custom keybindings via the `dconf` CLI.
///
/// Everything here intentionally avoids schema sources: AppImage-inherited
/// `XDG_DATA_DIRS` can poison glib's schema source for the whole process,
/// while the dconf CLI is unaffected.
fn existing_keybindings() -> Result<Vec<String>, IntegrationError> {
    let output = std::process::Command::new("dconf")
        .arg("read")
        .arg(MEDIA_KEYS_LIST_KEY)
        .output()
        .map_err(|e| IntegrationError::Settings(format!("dconf: {e}")))?;
    if !output.status.success() {
        return Err(IntegrationError::Settings(format!(
            "dconf read: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let unprefixed = |p: &str| {
        p.strip_prefix(MEDIA_KEYS_SCHEMA_BASE)
            .unwrap_or(p)
            .to_string()
    };
    if let Some(inner) = raw.strip_prefix("[").and_then(|s| s.strip_suffix("]")) {
        if inner.trim().is_empty() {
            return Ok(Vec::new());
        }
        return Ok(inner
            .split("', '")
            .map(|part| part.trim_matches('\'').to_string())
            .map(|p| unprefixed(&p))
            .collect());
    }
    Ok(Vec::new())
}

fn dconf_write(key: &str, value: &str) -> Result<(), IntegrationError> {
    let output = std::process::Command::new("dconf")
        .arg("write")
        .arg(key)
        .arg(value)
        .output()
        .map_err(|e| IntegrationError::Settings(format!("dconf: {e}")))?;
    if !output.status.success() {
        return Err(IntegrationError::Settings(format!(
            "dconf write {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn dconf_reset_dir(path: &str) -> Result<(), IntegrationError> {
    let output = std::process::Command::new("dconf")
        .arg("reset")
        .arg("-f")
        .arg(path)
        .output()
        .map_err(|e| IntegrationError::Settings(format!("dconf: {e}")))?;
    if !output.status.success() {
        return Err(IntegrationError::Settings(format!(
            "dconf reset {path}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn variant_str(value: &str) -> String {
    format!("'{value}'")
}

fn variant_str_list(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|v| variant_str(v)).collect();
    format!("[{}]", items.join(", "))
}

/// Insert our relocatable-schema path into the GNOME custom keybindings list
/// and write the binding.
fn install_keybinding(binding: &str) -> Result<String, IntegrationError> {
    let existing = existing_keybindings()?;
    let updated: Vec<String> = existing
        .iter()
        .filter(|p| p.as_str() != KEYBINDING_PATH)
        .cloned()
        .chain(std::iter::once(KEYBINDING_PATH.to_string()))
        .collect();

    dconf_write(MEDIA_KEYS_LIST_KEY, &variant_str_list(&updated))?;
    dconf_write(
        &format!("{KEYBINDING_PATH}name"),
        &variant_str("OpenCode Spotlight"),
    )?;
    dconf_write(
        &format!("{KEYBINDING_PATH}binding"),
        &variant_str(binding),
    )?;
    dconf_write(
        &format!("{KEYBINDING_PATH}command"),
        &variant_str(TOGGLE_COMMAND),
    )?;

    // Verify: the list must now contain our path.
    let check = existing_keybindings()?;
    if !check.iter().any(|p| p == KEYBINDING_PATH) {
        return Err(IntegrationError::Settings(
            "custom keybindings list did not update".into(),
        ));
    }

    Ok(KEYBINDING_PATH.to_string())
}

/// Remove our entry from the custom keybindings list, leaving everything
/// else exactly as the user had it.
fn remove_keybinding(path: &str) {
    if let Ok(existing) = existing_keybindings() {
        let updated: Vec<String> = existing.into_iter().filter(|p| p != path).collect();
        let _ = dconf_write(MEDIA_KEYS_LIST_KEY, &variant_str_list(&updated));
    }
    let _ = dconf_reset_dir(path);
}



#[cfg(test)]
#[path = "gnome_tests.rs"]
mod tests;

