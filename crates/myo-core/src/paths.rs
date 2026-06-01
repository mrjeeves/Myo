//! Where the Myo shell keeps its on-disk state.
//!
//! Intentionally a 3-line mirror of `myo-self-update`'s `paths::myo_dir` rather
//! than a cross-crate dependency: both crates root everything at `~/.myo`, but
//! `myo-core` (engine orchestration) and `myo-self-update` (the updater) stay
//! decoupled — they only ever meet inside the `myo` binary.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// `~/.myo` — the per-user Myo state directory.
pub fn myo_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    Ok(home.join(".myo"))
}

/// `~/.myo/config.json` — the single config document shared with the updater.
/// `myo-core` owns the `shell` section; `myo-self-update` owns `auto_update`.
/// Each loads the whole document, mutates only its own subtree, and writes it
/// back, so the two coexist without clobbering one another.
pub fn config_path() -> Result<PathBuf> {
    Ok(myo_dir()?.join("config.json"))
}
