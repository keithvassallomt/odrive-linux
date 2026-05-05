use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CONFIG_REL: &str = ".config/odrive-linux/config.toml";

/// Recognised tray-icon colour names. Must match the `odrive-tray-<name>`
/// icon set installed by `odrive-cli install-handlers`. Order is the order
/// the Settings combobox renders.
pub const TRAY_ICON_COLORS: &[&str] = &["pink", "white", "black", "darkgrey", "grey"];
pub const DEFAULT_TRAY_ICON_COLOR: &str = "pink";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdriveConfig {
    /// Directory containing the upstream `odrive` and `odriveagent` bins.
    /// Defaults to `~/.odrive-agent/bin` when the file is absent or the key
    /// is missing.
    #[serde(default = "default_agent_bin_dir")]
    pub agent_bin_dir: String,

    /// Colour variant for the panel-indicator icon. One of `TRAY_ICON_COLORS`.
    /// Unknown values fall back to `DEFAULT_TRAY_ICON_COLOR` at read time
    /// (the indicator just substitutes the default rather than refusing to
    /// render).
    #[serde(default = "default_tray_icon_color")]
    pub tray_icon_color: String,

    /// Whether the Nautilus extension paints the `odrive-synced` emblem
    /// on files / folders covered by a sync rule. Toggle in Preferences →
    /// Appearance. Default true (matches the prior always-on behaviour).
    #[serde(default = "default_true")]
    pub nautilus_synced_emblem: bool,

    /// Whether the Nautilus extension paints the `odrive-syncing` emblem
    /// on entries currently being synced (rows in the
    /// `sync_in_progress` table). Default true.
    #[serde(default = "default_true")]
    pub nautilus_syncing_emblem: bool,
}

fn home() -> String {
    std::env::var("HOME").expect("HOME environment variable must be set")
}

fn default_agent_bin_dir() -> String {
    format!("{}/.odrive-agent/bin", home())
}

fn default_tray_icon_color() -> String {
    DEFAULT_TRAY_ICON_COLOR.to_string()
}

fn default_true() -> bool {
    true
}

impl Default for OdriveConfig {
    fn default() -> Self {
        Self {
            agent_bin_dir: default_agent_bin_dir(),
            tray_icon_color: default_tray_icon_color(),
            nautilus_synced_emblem: true,
            nautilus_syncing_emblem: true,
        }
    }
}

impl OdriveConfig {
    pub fn path() -> PathBuf {
        PathBuf::from(format!("{}/{}", home(), CONFIG_REL))
    }

    /// Load the config from `~/.config/odrive-linux/config.toml`, falling
    /// back to defaults on missing file or unreadable/malformed content.
    /// A fresh-system run is the common case, not an error.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(s) => match toml::from_str::<Self>(&s) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!(
                        "config: {} is unparseable ({}); using defaults",
                        path.display(),
                        e,
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!(
                    "config: failed to read {} ({}); using defaults",
                    path.display(),
                    e,
                );
                Self::default()
            }
        }
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = OdriveConfig::load_from(&path);
        // The default still relies on $HOME — assert it ends with the
        // canonical relative segment rather than pinning the full prefix.
        assert!(cfg.agent_bin_dir.ends_with("/.odrive-agent/bin"));
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        let original = OdriveConfig {
            agent_bin_dir: "/opt/odrive/bin".to_string(),
            tray_icon_color: "white".to_string(),
            nautilus_synced_emblem: false,
            nautilus_syncing_emblem: true,
        };
        original.save_to(&path).expect("save");
        let loaded = OdriveConfig::load_from(&path);
        assert_eq!(loaded.agent_bin_dir, "/opt/odrive/bin");
        assert_eq!(loaded.tray_icon_color, "white");
        assert!(!loaded.nautilus_synced_emblem);
        assert!(loaded.nautilus_syncing_emblem);
    }

    #[test]
    fn missing_nautilus_emblem_keys_default_to_true() {
        // Older configs predating the emblem toggles must keep the prior
        // always-on behaviour rather than silently turning emblems off.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "agent_bin_dir = \"/opt/odrive/bin\"\n").unwrap();
        let cfg = OdriveConfig::load_from(&path);
        assert!(cfg.nautilus_synced_emblem);
        assert!(cfg.nautilus_syncing_emblem);
    }

    #[test]
    fn missing_tray_icon_color_uses_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Write a file that has only the agent_bin_dir key — exercises the
        // serde(default) on tray_icon_color so older configs don't break
        // when this field is added.
        std::fs::write(&path, "agent_bin_dir = \"/opt/odrive/bin\"\n").unwrap();
        let cfg = OdriveConfig::load_from(&path);
        assert_eq!(cfg.tray_icon_color, DEFAULT_TRAY_ICON_COLOR);
    }

    #[test]
    fn unparseable_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not = valid toml [[[").unwrap();
        let cfg = OdriveConfig::load_from(&path);
        assert!(cfg.agent_bin_dir.ends_with("/.odrive-agent/bin"));
    }
}
