//! Pinned-engine resolution helpers — the Tauri-free, unit-tested logic behind
//! "is this `myownllm` new enough, and where would I fetch the pinned one?".
//!
//! Myo bundles a pinned `myownllm` (see `.myownllm-rev` / `build.rs`), but the
//! *resolved* engine can still be stale — most commonly when the build couldn't
//! download the pinned release (so the sidecar is a stub) and the runtime falls
//! through to an older `myownllm` on `PATH`. Rather than let that surface to the
//! user as a transcription 404, the binary's `engine_update` fetches the pinned
//! release once. This module is the pure half it leans on: version comparison,
//! the platform→asset mapping, and the release URL. The IO (running `--version`,
//! downloading, emitting `myo://engine`) lives in the binary.

/// Reduce a `myownllm --version` line (`"myownllm 0.2.24"`) to its version
/// token (`"0.2.24"`): the last whitespace-separated word of the first line.
/// Mirrors `build.rs`'s `engine_version`.
pub fn parse_version_token(version_output: &str) -> Option<&str> {
    version_output
        .lines()
        .next()?
        .split_whitespace()
        .next_back()
        .filter(|s| !s.is_empty())
}

/// Is `have` at least `want`, comparing dotted numeric components (the shorter
/// padded with zeros)? Anything that doesn't parse as `MAJOR.MINOR.PATCH…` of
/// integers is treated **conservatively** as *not* meeting the pin — so an
/// unrecognizable build triggers a refetch of the known-good pinned engine
/// rather than being trusted.
pub fn version_meets(have: &str, want: &str) -> bool {
    let (Some(h), Some(w)) = (parse_components(have), parse_components(want)) else {
        return false;
    };
    for i in 0..h.len().max(w.len()) {
        let a = h.get(i).copied().unwrap_or(0);
        let b = w.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    true // all components equal
}

fn parse_components(v: &str) -> Option<Vec<u32>> {
    // Tolerate a stray leading `v` even though MyOwnLLM's tags are bare semver.
    let v = v.trim().trim_start_matches('v');
    let parts = v
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    (!parts.is_empty()).then_some(parts)
}

/// MyOwnLLM's release platform name for an OS/arch pair (matching its
/// `release.yml` asset names), or `None` for a target with no prebuilt engine.
pub fn release_platform(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

/// This host's release platform name (from compile-time `OS`/`ARCH`).
pub fn host_platform() -> Option<&'static str> {
    release_platform(std::env::consts::OS, std::env::consts::ARCH)
}

/// The GitHub release download URL for the pinned engine asset. Windows ships a
/// `.zip`; everything else a `.tar.gz`.
pub fn release_asset_url(tag: &str, platform: &str) -> String {
    let ext = if platform.starts_with("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!(
        "https://github.com/mrjeeves/MyOwnLLM/releases/download/{tag}/myownllm-{platform}.{ext}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_version_token() {
        assert_eq!(parse_version_token("myownllm 0.2.24"), Some("0.2.24"));
        assert_eq!(parse_version_token("0.2.24\n"), Some("0.2.24"));
        assert_eq!(parse_version_token(""), None);
    }

    #[test]
    fn version_meets_is_a_semver_at_least() {
        assert!(version_meets("0.2.24", "0.2.24")); // equal meets
        assert!(version_meets("0.2.25", "0.2.24")); // patch newer
        assert!(version_meets("0.3.0", "0.2.24")); // minor newer
        assert!(version_meets("1.0.0", "0.2.24")); // major newer
        assert!(!version_meets("0.2.23", "0.2.24")); // older patch — the 404 case
        assert!(!version_meets("0.1.99", "0.2.24")); // older minor
    }

    #[test]
    fn version_meets_pads_missing_components() {
        assert!(version_meets("0.2", "0.2.0"));
        assert!(!version_meets("0.2", "0.2.1"));
        assert!(version_meets("0.2.24.1", "0.2.24"));
    }

    #[test]
    fn unparseable_version_is_conservatively_not_met() {
        // Better to refetch the known-good pin than trust a mystery build.
        assert!(!version_meets("nightly", "0.2.24"));
        assert!(!version_meets("0.2.24-rc1", "0.2.24"));
    }

    #[test]
    fn release_platform_maps_known_targets_only() {
        assert_eq!(release_platform("macos", "aarch64"), Some("macos-aarch64"));
        assert_eq!(release_platform("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(
            release_platform("windows", "x86_64"),
            Some("windows-x86_64")
        );
        assert_eq!(release_platform("freebsd", "x86_64"), None);
        // host_platform is just release_platform on this build's consts.
        assert_eq!(
            host_platform(),
            release_platform(std::env::consts::OS, std::env::consts::ARCH)
        );
    }

    #[test]
    fn asset_url_picks_the_right_archive_extension() {
        assert_eq!(
            release_asset_url("0.2.24", "macos-aarch64"),
            "https://github.com/mrjeeves/MyOwnLLM/releases/download/0.2.24/myownllm-macos-aarch64.tar.gz"
        );
        assert_eq!(
            release_asset_url("0.2.24", "windows-x86_64"),
            "https://github.com/mrjeeves/MyOwnLLM/releases/download/0.2.24/myownllm-windows-x86_64.zip"
        );
    }
}
