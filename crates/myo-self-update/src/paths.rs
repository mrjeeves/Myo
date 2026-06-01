//! Where Myo keeps its on-disk state.

use anyhow::{anyhow, Result};
use std::path::PathBuf;

/// `~/.myo` — the per-user Myo state directory (config, cache, staged updates).
///
/// Mirrors MyOwnLLM's `~/.myownllm`: a single home-rooted dir keeps the
/// updater's staging area on (almost always) the same filesystem as the
/// installed binary, so the atomic rename in `update::atomic_replace` can take
/// the fast path.
pub fn myo_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    Ok(home.join(".myo"))
}
