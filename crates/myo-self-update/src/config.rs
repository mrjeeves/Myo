//! `~/.myo/config.json` — read-only views + the one mutation the updater needs
//! (`auto_update.enabled`).
//!
//! Deliberately tiny: Myo's full config grows later. For now the only section
//! that matters is `auto_update`, and `merge_defaults` guarantees it always
//! exists with every key so the rest of the updater can read it without
//! `unwrap_or` scattered everywhere.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::Path;

use crate::paths::myo_dir;

/// The compiled-in defaults. `auto_update` mirrors MyOwnLLM's knobs:
///   - `enabled`              — master switch (also overridable via `MYO_AUTOUPDATE=0`)
///   - `channel`              — "stable" | "beta"
///   - `auto_apply`           — "patch" | "minor" | "all" | "none" (background staging policy)
///   - `check_interval_hours` — how often the background watcher hits the network
///   - `stable_url`/`beta_url`— optional per-machine release-feed overrides
pub fn default_config_value() -> Value {
    serde_json::json!({
        "auto_update": {
            "enabled": true,
            "channel": "stable",
            "auto_apply": "patch",
            "check_interval_hours": 6.0,
        }
    })
}

/// Load `~/.myo/config.json`, returning compiled-in defaults if it's absent.
pub fn load_config_value() -> Result<Value> {
    load_config_value_at(&myo_dir()?.join("config.json"))
}

/// Persist the full config document to `~/.myo/config.json`.
pub fn save_config_value(config: &Value) -> Result<()> {
    save_config_value_at(&myo_dir()?.join("config.json"), config)
}

/// Path-injectable core of [`load_config_value`] — tests drive it with a temp
/// file so they never touch the real `~/.myo`.
pub fn load_config_value_at(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(default_config_value());
    }
    let s = std::fs::read_to_string(path)?;
    let v: Value = serde_json::from_str(&s).map_err(|e| anyhow!("invalid config.json: {e}"))?;
    Ok(merge_defaults(v))
}

/// Path-injectable core of [`save_config_value`].
pub fn save_config_value_at(path: &Path, config: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}

/// Fill in any missing `auto_update` keys from the defaults without clobbering
/// values the user already set. Guarantees `auto_update` is an object so
/// `set_enabled` can mutate it in place.
pub fn merge_defaults(mut v: Value) -> Value {
    let defaults = default_config_value();
    let Some(obj) = v.as_object_mut() else {
        return defaults;
    };
    let au_default = defaults["auto_update"].clone();
    let au = obj
        .entry("auto_update")
        .or_insert_with(|| serde_json::json!({}));
    if !au.is_object() {
        *au = au_default;
    } else if let (Some(au_obj), Some(def_obj)) = (au.as_object_mut(), au_default.as_object()) {
        for (k, dv) in def_obj {
            au_obj.entry(k.clone()).or_insert_with(|| dv.clone());
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_returns_defaults_when_file_absent() {
        let cfg = load_config_value_at(Path::new("/no/such/myo/config.json")).unwrap();
        assert_eq!(cfg["auto_update"]["enabled"], json!(true));
        assert_eq!(cfg["auto_update"]["channel"], json!("stable"));
        assert_eq!(cfg["auto_update"]["auto_apply"], json!("patch"));
        assert_eq!(cfg["auto_update"]["check_interval_hours"], json!(6.0));
    }

    #[test]
    fn merge_defaults_fills_missing_auto_update_keys_only() {
        let user = json!({ "auto_update": { "enabled": false }, "other": 1 });
        let merged = merge_defaults(user);
        // User value preserved…
        assert_eq!(merged["auto_update"]["enabled"], json!(false));
        // …unrelated keys preserved…
        assert_eq!(merged["other"], json!(1));
        // …and the rest of auto_update backfilled.
        assert_eq!(merged["auto_update"]["channel"], json!("stable"));
        assert_eq!(merged["auto_update"]["auto_apply"], json!("patch"));
    }

    #[test]
    fn merge_defaults_replaces_non_object_auto_update() {
        let merged = merge_defaults(json!({ "auto_update": "garbage" }));
        assert!(merged["auto_update"].is_object());
        assert_eq!(merged["auto_update"]["enabled"], json!(true));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("myo-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut cfg = default_config_value();
        cfg["auto_update"]["channel"] = json!("beta");
        save_config_value_at(&path, &cfg).unwrap();
        let back = load_config_value_at(&path).unwrap();
        assert_eq!(back["auto_update"]["channel"], json!("beta"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
