//! `~/.myo/config.json` → the `shell` section: the Myo shell's persisted state.
//!
//! Just two things survive restarts: the four capability toggles and the
//! incognito switch. Everything else (sessions, turns, live engine handles) is
//! ephemeral. We share the updater's config document — loading the whole thing,
//! touching only `shell`, and writing it back — so `auto_update` is preserved.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::capabilities::Capabilities;
use crate::paths::config_path;

/// The persisted half of the shell's state — the `shell` key in `config.json`.
///
/// `Default` is derived: it composes `Capabilities::default()` (the PLAN's
/// "Web on, everything else off" posture) with `incognito: false`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellSettings {
    /// The four tier-a capability toggles (Web / Files / Code / Reach-out).
    pub capabilities: Capabilities,
    /// When on, memory is paused: turns run with Odysseus incognito so nothing
    /// is persisted. A privacy switch the user (or the agent) can flip.
    pub incognito: bool,
}

impl ShellSettings {
    /// Load the `shell` section from `~/.myo/config.json`, defaulting cleanly
    /// when the file or the section is absent.
    pub fn load() -> Result<Self> {
        Self::load_at(&config_path()?)
    }

    /// Persist this `shell` section into `~/.myo/config.json`, preserving every
    /// other top-level key (notably the updater's `auto_update`).
    pub fn save(&self) -> Result<()> {
        self.save_at(&config_path()?)
    }

    /// Path-injectable core of [`load`](Self::load) — tests drive it with a temp
    /// file so they never touch the real `~/.myo`.
    pub fn load_at(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path)?)
            .map_err(|e| anyhow!("invalid config.json: {e}"))?;
        match doc.get("shell") {
            // A present-but-malformed `shell` shouldn't wedge the whole shell;
            // fall back to defaults rather than propagating a parse error.
            Some(section) => Ok(serde_json::from_value(section.clone()).unwrap_or_default()),
            None => Ok(Self::default()),
        }
    }

    /// Path-injectable core of [`save`](Self::save). Reads the existing document
    /// (so concurrent `auto_update` keys survive), replaces `shell`, rewrites.
    pub fn save_at(&self, path: &Path) -> Result<()> {
        let mut doc: Value = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(path)?).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        if !doc.is_object() {
            doc = serde_json::json!({});
        }
        doc["shell"] = serde_json::to_value(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&doc)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("myo-core-cfg-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    #[test]
    fn defaults_when_absent() {
        let s = ShellSettings::load_at(Path::new("/no/such/myo/config.json")).unwrap();
        assert_eq!(s, ShellSettings::default());
        assert!(s.capabilities.web);
        assert!(!s.capabilities.code);
        assert!(!s.incognito);
    }

    #[test]
    fn round_trips() {
        let path = temp_path("rt");
        let mut s = ShellSettings::default();
        s.capabilities.code = true;
        s.incognito = true;
        s.save_at(&path).unwrap();
        let back = ShellSettings::load_at(&path).unwrap();
        assert_eq!(back, s);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_preserves_foreign_keys() {
        // Simulate the updater having written `auto_update` first.
        let path = temp_path("coexist");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({ "auto_update": { "enabled": false } })).unwrap(),
        )
        .unwrap();

        ShellSettings::default().save_at(&path).unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Our section landed…
        assert_eq!(doc["shell"]["capabilities"]["web"], json!(true));
        // …and the updater's section is untouched.
        assert_eq!(doc["auto_update"]["enabled"], json!(false));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn malformed_shell_section_falls_back_to_default() {
        let path = temp_path("malformed");
        std::fs::write(&path, r#"{ "shell": "not-an-object" }"#).unwrap();
        let s = ShellSettings::load_at(&path).unwrap();
        assert_eq!(s, ShellSettings::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
