//! Persisted user settings.
//!
//! Stored as JSON in the OS config dir rather than via a plugin, so the Rust side
//! stays the single source of truth — the session `Manager` lives here too, and
//! settings changes have to be applied to a running session immediately.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager as _};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Hold the display assertion as well as the system one. On macOS this is what
    /// stops the screen blanking and locking.
    pub keep_display: bool,

    /// What the tray "Turn On" item starts. `None` means indefinite.
    pub default_duration_secs: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        // Keeping the display on is what most people mean by "keep my Mac awake", so
        // it is the default even though it is the more aggressive of the two.
        Self {
            keep_display: true,
            default_duration_secs: None,
        }
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

impl Settings {
    /// Read from disk, falling back to defaults.
    ///
    /// A corrupt or unreadable file is not an error worth surfacing: the app must
    /// still start, and defaults are always safe. The bad file is left in place
    /// rather than clobbered, so it can be inspected.
    pub fn load(app: &AppHandle) -> Self {
        let Some(p) = path(app) else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return Self::default();
        };

        match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("no-afk: ignoring unreadable {}: {err}", p.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, app: &AppHandle) -> Result<(), String> {
        let p = path(app).ok_or("could not resolve the config directory")?;

        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&p, json).map_err(|e| format!("{}: {e}", p.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_the_display_on_indefinitely() {
        let s = Settings::default();
        assert!(s.keep_display);
        assert_eq!(s.default_duration_secs, None);
    }

    #[test]
    fn round_trips_through_json() {
        let s = Settings {
            keep_display: false,
            default_duration_secs: Some(1800),
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    /// A file written by an older version, missing newer fields, must still load.
    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());

        let s: Settings = serde_json::from_str(r#"{"keep_display":false}"#).unwrap();
        assert!(!s.keep_display);
        assert_eq!(s.default_duration_secs, None);
    }

    #[test]
    fn indefinite_is_representable_as_null() {
        let s: Settings = serde_json::from_str(r#"{"default_duration_secs":null}"#).unwrap();
        assert_eq!(s.default_duration_secs, None);
    }
}
