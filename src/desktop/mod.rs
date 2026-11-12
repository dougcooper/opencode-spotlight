//! Desktop-environment integrations: everything DE-specific lives behind one
//! trait, so another desktop is supported by implementing it (fact-27).
//!
//! The trait covers the three artifacts the toggle contract needs: a desktop
//! entry (which must be D-Bus activatable so `.desktop` launches route to the
//! single instance), an autostart entry (window-less resident app), and a
//! hotkey bound to `opencode-spotlight --toggle`.

pub mod gnome;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

use crate::state::{Config, Paths};

/// What `install` creates; uninstall removes exactly this (fact-08).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub desktop_entry: PathBuf,
    pub autostart_entry: PathBuf,
    pub keybinding_path: Option<String>,
    pub binding: String,
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings backend: {0}")]
    Settings(String),
    #[error("{0}")]
    Other(String),
}

/// Inputs to `install`.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Absolute path of the installed binary (used in the desktop entries).
    pub binary_path: PathBuf,
    /// The accelerator string (e.g. `<Super>o`).
    pub binding: String,
}

pub trait DesktopIntegration {
    fn id(&self) -> &'static str;

    fn install(
        &self,
        paths: &Paths,
        opts: &InstallOptions,
    ) -> Result<Manifest, IntegrationError>;

    fn uninstall(&self, manifest: &Manifest) -> Result<(), IntegrationError>;
}

// ---- manifest ---------------------------------------------------------------

pub fn load_manifest(paths: &Paths) -> Option<Manifest> {
    fs::read_to_string(paths.install_manifest())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

pub fn save_manifest(paths: &Paths, manifest: &Manifest) -> Result<(), IntegrationError> {
    fs::create_dir_all(&paths.state_dir)?;
    fs::write(
        paths.install_manifest(),
        serde_json::to_string_pretty(manifest)
            .map_err(|e| IntegrationError::Other(e.to_string()))?,
    )?;
    Ok(())
}

pub fn remove_manifest(paths: &Paths) {
    let _ = fs::remove_file(paths.install_manifest());
}

/// `opencode-spotlight install`
pub fn cli_install() -> i32 {
    let paths = Paths::detect();
    let config = Config::load(&paths);
    let binary = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("opencode-spotlight: could not locate own binary: {e}");
            return 1;
        }
    };
    let opts = InstallOptions {
        binary_path: binary,
        binding: config.keybinding.clone(),
    };
    let integration = gnome::Gnome;
    match integration.install(&paths, &opts) {
        Ok(manifest) => {
            if let Err(e) = save_manifest(&paths, &manifest) {
                eprintln!("opencode-spotlight: install succeeded but manifest could not be saved: {e}");
                return 1;
            }
            let _ = fs::create_dir_all(config.scratch_dir(&paths));
            println!("Installed ({}):", integration.id());
            println!("  desktop entry:  {}", manifest.desktop_entry.display());
            println!("  autostart:      {}", manifest.autostart_entry.display());
            if let Some(path) = &manifest.keybinding_path {
                println!("  keybinding:     {} -> {}", manifest.binding, path);
            }
            println!("The setting takes effect immediately; re-summon is instant while the app is resident.");
            0
        }
        Err(e) => {
            eprintln!("opencode-spotlight: install failed: {e}");
            eprintln!("You can always bind `opencode-spotlight --toggle` manually (see README).");
            1
        }
    }
}

/// `opencode-spotlight uninstall`
pub fn cli_uninstall() -> i32 {
    let paths = Paths::detect();
    let manifest = match load_manifest(&paths) {
        Some(m) => m,
        None => {
            eprintln!("opencode-spotlight: no install manifest found; nothing to do.");
            return 0;
        }
    };
    let integration = gnome::Gnome;
    match integration.uninstall(&manifest) {
        Ok(()) => {
            remove_manifest(&paths);
            println!("Removed everything recorded by install.");
            0
        }
        Err(e) => {
            eprintln!("opencode-spotlight: uninstall failed: {e}");
            1
        }
    }
}
