//! Persistent app state: user configuration and the remembered session id.
//!
//! All functions take explicit paths (rather than reading the environment) so
//! they can be tested against temporary directories.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::oc::ModelRef;

/// The XDG locations the app uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    pub fn detect() -> Self {
        let pd = directories::ProjectDirs::from("dev", "dougcooper", "opencode-spotlight")
            .expect("XDG base directories must be resolvable");
        Self {
            config_dir: pd.config_dir().to_path_buf(),
            data_dir: pd.data_dir().to_path_buf(),
            state_dir: pd.state_dir().expect("state dir").to_path_buf(),
        }
    }

    pub fn scratch_dir(&self) -> PathBuf {
        self.data_dir.join("scratch")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    pub fn install_manifest(&self) -> PathBuf {
        self.state_dir.join("install.json")
    }
}

fn default_keybinding() -> String {
    "<Super>space".to_string()
}

/// One of the two models behind the quick toggle chip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCfg {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
}

impl From<&ModelCfg> for ModelRef {
    fn from(m: &ModelCfg) -> Self {
        ModelRef { id: m.id.clone(), provider_id: m.provider_id.clone() }
    }
}

/// The quick/deep pairing for the model toggle chip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick: Option<ModelCfg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep: Option<ModelCfg>,
}

/// User configuration (`~/.config/opencode-spotlight/config.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where ad-hoc overlay chats live (sessions are created here).
    pub scratch_dir: Option<String>,
    /// GNOME keybinding accelerator (also used by the install helper).
    pub keybinding: String,
    /// Command template for the TUI hand-off; `{dir}` is replaced with the
    /// scratch directory. When unset, the app tries known terminals.
    pub handoff_command: Option<String>,
    /// The quick/deep model pair; the chip is shown only when both are set.
    pub models: Option<ModelsCfg>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scratch_dir: None,
            keybinding: default_keybinding(),
            handoff_command: None,
            models: None,
        }
    }
}

const CONFIG_TEMPLATE: &str = r#"# opencode-spotlight configuration
# All fields are optional; the defaults are shown.

# Directory where ad-hoc quick chats live.
#scratch_dir = "~/.local/share/opencode-spotlight/scratch"

# GNOME keybinding registered by `opencode-spotlight install`.
#keybinding = "<Super>space"

# Terminal command used by the TUI hand-off button.
# {dir} is replaced with the scratch directory.
#handoff_command = "ptyxis --directory {dir} opencode"

# The quick/deep model pair for the one-tap toggle chip.
# Quick defaults to your OpenCode default model when unset.
#[models]
#quick = { id = "glm-5.3-flash", providerID = "ollama-cloud" }
#deep = { id = "glm-5.3", providerID = "ollama-cloud" }
"#;

impl Config {
    /// Load the config, writing a commented template on first use.
    pub fn load(paths: &Paths) -> Config {
        let file = paths.config_file();
        match fs::read_to_string(&file) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!(
                        "opencode-spotlight: ignoring invalid {}: {e}",
                        file.display()
                    );
                    Config::default()
                }
            },
            Err(_) => {
                let _ = fs::create_dir_all(&paths.config_dir);
                let _ = fs::write(&file, CONFIG_TEMPLATE);
                Config::default()
            }
        }
    }

    /// The effective scratch directory (config value with `~` expanded).
    pub fn scratch_dir(&self, paths: &Paths) -> PathBuf {
        match &self.scratch_dir {
            Some(dir) => expand_home(dir),
            None => paths.scratch_dir(),
        }
    }
}

fn expand_home(path: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    if path == "~" {
        return PathBuf::from(home);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// Small persisted app state (the last overlay session id).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub last_session_id: Option<String>,
}

impl AppState {
    pub fn load(paths: &Paths) -> AppState {
        fs::read_to_string(paths.state_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, paths: &Paths) {
        if let Ok(text) = serde_json::to_string_pretty(self)
            && fs::create_dir_all(&paths.state_dir).is_ok() {
                let _ = fs::write(paths.state_file(), text);
            }
    }
}

/// Test helpers operating on explicit paths.
#[cfg(test)]
pub(crate) mod test_paths {
    use super::*;
    use std::path::Path;

    /// A temporary `Paths` with all dirs under `root`.
    pub fn temporary(root: &Path) -> Paths {
        Paths {
            config_dir: root.join("config"),
            data_dir: root.join("share"),
            state_dir: root.join("state"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_paths::temporary;
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn config_default_written_and_reread() {
        let dir = tempdir().expect("tempdir");
        let paths = temporary(dir.path());
        // First load writes the template.
        let cfg = Config::load(&paths);
        assert_eq!(cfg, Config::default());
        assert!(paths.config_file().exists());
        // Second load reads the same file back.
        let again = Config::load(&paths);
        assert_eq!(again, Config::default());
    }

    #[test]
    fn config_values_round_trip() {
        let dir = tempdir().expect("tempdir");
        let paths = temporary(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.config_file(),
            r##"
scratch_dir = "~/chats"
keybinding = "<Ctrl><Alt>space"
handoff_command = "kitty --directory {dir} opencode"
[models]
quick = { id = "glm-5.3-flash", providerID = "ollama-cloud" }
deep = { id = "glm-5.3", providerID = "ollama-cloud" }
"##,
        )
        .unwrap();
        let cfg = Config::load(&paths);
        assert_eq!(
            cfg.scratch_dir.as_deref(),
            Some("~/chats"),
            "scratch override"
        );
        assert_eq!(cfg.keybinding, "<Ctrl><Alt>space");
        let models = cfg.models.as_ref().expect("models set");
        let deep = models.deep.as_ref().expect("deep set");
        assert_eq!(deep.id, "glm-5.3");
        let mr: ModelRef = (&models.quick.clone().unwrap()).into();
        assert_eq!(mr.provider_id, "ollama-cloud");
        assert_eq!(cfg.scratch_dir(&paths), home_dir().join("chats"));
    }

    fn home_dir() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap())
    }

    #[test]
    fn app_state_round_trip() {
        let dir = tempdir().expect("tempdir");
        let paths = temporary(dir.path());
        let state = AppState { last_session_id: Some("abc".into()) };
        state.save(&paths);
        assert!(paths.state_file().exists());
        assert_eq!(AppState::load(&paths), state);
    }

    #[test]
    fn expand_home_variants() {
        assert_eq!(
            expand_home("~/x/y"),
            PathBuf::from(std::env::var("HOME").unwrap()).join("x/y")
        );
        assert_eq!(expand_home("/plain"), PathBuf::from("/plain"));
    }
}
