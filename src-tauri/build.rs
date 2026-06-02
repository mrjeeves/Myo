//! Build-time bundling of the `myownllm` model engine as a Tauri sidecar.
//!
//! Myo's voice loop talks to a running `myownllm serve` on `:1473` for the LLM
//! *and* — since the open-mic listening landed — for transcription
//! (`POST /v1/audio/transcriptions`). End-users shouldn't have to install
//! MyOwnLLM separately, and Myo must run a version **new enough to have the
//! transcription route** (a stale `myownllm` 404s it). So, exactly the way
//! MyOwnLLM bundles a pinned `myownmesh` daemon, this script stages a pinned
//! `myownllm` into `binaries/myownllm-<target-triple>` and Tauri's bundler ships
//! it next to the Myo executable as an `externalBin`.
//!
//! The pinned version lives in `.myownllm-rev` (a MyOwnLLM release tag, bare
//! semver like `0.2.22` — its tags aren't `v`-prefixed), kept in lockstep with
//! the MyOwnLLM release that carries the routes Myo needs.
//!
//! Resolution order at build time:
//!   1. **Override** — `MYO_MYOWNLLM_BIN` / `MYOWNLLM_BIN` pointing at a
//!      pre-built engine (release CI brings its own signed binary).
//!   2. **Sibling checkout** (dev) — a MyOwnLLM checkout next to this repo with
//!      a built `src-tauri/target/<profile>/myownllm`. Lets a dev who built the
//!      engine locally have Myo pick it up immediately (this is the path that
//!      unblocks `just dev` against unreleased engine changes).
//!   3. **Release asset** — download `myownllm-<platform>.{tar.gz,zip}` from
//!      MyOwnLLM's GitHub Releases for the pinned tag.
//!
//! On failure a zero-byte stub is written so `tauri_build`'s `externalBin`
//! existence check still passes; the runtime then falls back to
//! `MYO_MYOWNLLM_BIN` / `PATH` (see `supervisor::resolve_myownllm`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Bundle the sidecar BEFORE tauri_build so its `externalBin` validation in
    // tauri.conf.json sees the produced file.
    if let Err(e) = bundle_myownllm_sidecar() {
        println!(
            "cargo:warning=myownllm sidecar bundle failed: {e:#} — continuing without a \
             bundled engine; runtime falls back to MYO_MYOWNLLM_BIN / PATH discovery"
        );
        if let Err(stub_err) = write_sidecar_stub() {
            println!("cargo:warning=could not write sidecar stub: {stub_err:#}");
        }
    }

    tauri_build::build();
}

/// The pinned MyOwnLLM release tag from `.myownllm-rev`, if present.
fn pinned_rev(crate_dir: &Path) -> Option<String> {
    let rev_file = crate_dir.parent().unwrap().join(".myownllm-rev");
    println!("cargo:rerun-if-changed={}", rev_file.display());
    let rev = fs::read_to_string(&rev_file)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty());
    if let Some(r) = &rev {
        // Stamp the pin into the runtime binary so it can self-heal a stale
        // local engine up to *this exact version* at launch — the runtime twin
        // of the bundling below (see `src/engine_update.rs`). Read with
        // `option_env!`, so an absent pin just disables that path.
        println!("cargo:rustc-env=MYOWNLLM_PINNED_REV={r}");
    }
    rev
}

fn write_sidecar_stub() -> std::io::Result<()> {
    let target_triple = env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    let exe_suffix = if target_triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bin_dir = crate_dir.join("binaries");
    fs::create_dir_all(&bin_dir)?;
    let p = bin_dir.join(format!("myownllm-{target_triple}{exe_suffix}"));
    if !p.exists() {
        fs::write(&p, b"")?;
        make_executable(&p).ok();
    }
    Ok(())
}

fn bundle_myownllm_sidecar() -> Result<(), Box<dyn std::error::Error>> {
    let target_triple = env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    // The runtime uses this to look for `myownllm-<triple>{.exe}` (dev staging,
    // where Tauri keeps the suffix) and `myownllm{.exe}` (production bundle,
    // where Tauri strips it) next to the Myo executable.
    println!("cargo:rustc-env=MYOWNLLM_SIDECAR_TRIPLE={target_triple}");
    let exe_suffix = if target_triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let bin_dir = crate_dir.join("binaries");
    fs::create_dir_all(&bin_dir)?;
    let sidecar_path = bin_dir.join(format!("myownllm-{target_triple}{exe_suffix}"));

    println!("cargo:rerun-if-env-changed=MYO_MYOWNLLM_BIN");
    println!("cargo:rerun-if-env-changed=MYOWNLLM_BIN");
    println!("cargo:rerun-if-env-changed=MYO_SKIP_MYOWNLLM_SIDECAR");

    let want_rev = pinned_rev(&crate_dir);

    // Where we record which engine is currently staged. Idempotency is applied
    // per-resolution-path below: override/sibling re-bundle when their source
    // changes (so a rebuilt engine at the *same* version is picked up — the dev
    // iteration case), while the immutable release download is skipped once its
    // tag is already staged. The per-path freshness checks also avoid os error 32
    // from a parallel `tauri dev` re-copying a file it's holding open.
    let bundled_rev_sentinel = bin_dir.join(".bundled-rev");

    // Escape hatch for offline / sandboxed builds.
    if env::var_os("MYO_SKIP_MYOWNLLM_SIDECAR").is_some() {
        println!("cargo:warning=MYO_SKIP_MYOWNLLM_SIDECAR set — skipping engine sidecar bundle");
        return Err("skipped via MYO_SKIP_MYOWNLLM_SIDECAR".into());
    }

    // 1. Explicit override — release CI ships a pre-built/signed binary.
    for var in ["MYO_MYOWNLLM_BIN", "MYOWNLLM_BIN"] {
        if let Ok(p) = env::var(var) {
            let p = PathBuf::from(p);
            println!("cargo:rerun-if-changed={}", p.display());
            if p.exists() {
                if up_to_date(&p, &sidecar_path) {
                    return Ok(());
                }
                println!(
                    "cargo:warning=bundling myownllm from {} (via {var})",
                    p.display()
                );
                write_sidecar_with_retry(&p, &sidecar_path)?;
                make_executable(&sidecar_path)?;
                if let Some(want) = &want_rev {
                    fs::write(&bundled_rev_sentinel, want)?;
                }
                return Ok(());
            }
        }
    }

    // 2. Sibling MyOwnLLM checkout (dev). Myo is a local sidecar host, not a
    //    cross-device peer, so — unlike MyOwnLLM's strict version gate on the
    //    MyOwnMesh sibling — we trust a present sibling and just log its version.
    //    A dev who built the engine wants Myo to use exactly that build.
    if let Some(p) = find_sibling_engine_binary(&crate_dir, exe_suffix) {
        if up_to_date(&p, &sidecar_path) {
            return Ok(());
        }
        let ver = engine_version(&p).unwrap_or_else(|_| "unknown".into());
        println!(
            "cargo:warning=bundling myownllm from sibling checkout: {} (reports {ver})",
            p.display()
        );
        write_sidecar_with_retry(&p, &sidecar_path)?;
        make_executable(&sidecar_path)?;
        if let Some(want) = &want_rev {
            fs::write(&bundled_rev_sentinel, want)?;
        }
        return Ok(());
    }

    // 3. Prebuilt release asset from MyOwnLLM's GitHub Releases. `.myownllm-rev`
    //    holds the tag (bare semver, e.g. `0.2.22` — MyOwnLLM's tags aren't
    //    `v`-prefixed); the asset is myownllm-<platform>.{tar.gz,zip}.
    let tag = want_rev.as_deref().ok_or_else(|| {
        format!(
            "no pin in .myownllm-rev (got {want_rev:?}); set MYO_MYOWNLLM_BIN, build a sibling \
             MyOwnLLM checkout, or pin a release tag"
        )
    })?;
    // Release assets are immutable per tag — once this tag is staged, skip the
    // re-download on subsequent builds.
    let staged_is_tag = sidecar_path
        .metadata()
        .map(|m| m.len() > 0)
        .unwrap_or(false)
        && fs::read_to_string(&bundled_rev_sentinel)
            .map(|s| s.trim() == tag)
            .unwrap_or(false);
    if staged_is_tag {
        return Ok(());
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let staging = out_dir.join("myownllm-staging");
    fs::create_dir_all(&staging)?;
    let bin = download_release_asset(tag, &target_triple, &staging, exe_suffix)?;
    write_sidecar_with_retry(&bin, &sidecar_path)?;
    make_executable(&sidecar_path)?;
    fs::write(&bundled_rev_sentinel, tag)?;
    println!(
        "cargo:warning=[sidecar] {} ready ({} bytes)",
        sidecar_path.display(),
        fs::metadata(&sidecar_path).map(|m| m.len()).unwrap_or(0)
    );
    Ok(())
}

/// Atomic-rename copy that survives Windows `ERROR_SHARING_VIOLATION` (os error
/// 32) from either a parallel `tauri dev` holding the destination open or
/// Defender scanning a freshly-extracted source. Retries with backoff.
fn write_sidecar_with_retry(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() && validate_executable_magic(dst).is_err() {
        let _ = fs::remove_file(dst);
    }
    let tmp = dst.with_extension("tmp-incoming");
    let _ = fs::remove_file(&tmp);

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..10 {
        match fs::copy(src, &tmp) {
            Ok(_) => match fs::rename(&tmp, dst) {
                Ok(()) => {
                    println!(
                        "cargo:warning=[sidecar] wrote {} ({} bytes)",
                        dst.display(),
                        fs::metadata(dst).map(|m| m.len()).unwrap_or(0)
                    );
                    return Ok(());
                }
                Err(e) if e.raw_os_error() == Some(32) => {
                    let _ = fs::remove_file(&tmp);
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(200 << attempt));
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(e);
                }
            },
            Err(e) if e.raw_os_error() == Some(32) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(200 << attempt));
            }
            Err(e) => return Err(e),
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last_err
        .unwrap_or_else(|| std::io::Error::other("retries exhausted with no recorded error")))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Map a Rust target triple to MyOwnLLM's release platform name (see
/// `MyOwnLLM/.github/workflows/release.yml`).
fn release_platform_name(target_triple: &str) -> Result<&'static str, String> {
    Ok(match target_triple {
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "linux-aarch64",
        "aarch64-apple-darwin" => "macos-aarch64",
        "x86_64-apple-darwin" => "macos-x86_64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        other => return Err(format!("no prebuilt myownllm for target triple '{other}'")),
    })
}

/// Download + extract `myownllm-<platform>.{tar.gz,zip}` for `tag`, returning the
/// extracted binary path. Shells out to `curl` + `tar`/PowerShell (all ship by
/// default) to avoid build-time reqwest/zip deps.
fn download_release_asset(
    tag: &str,
    target_triple: &str,
    staging: &Path,
    exe_suffix: &str,
) -> Result<PathBuf, String> {
    let platform = release_platform_name(target_triple)?;
    let is_windows = exe_suffix == ".exe";
    let archive_name = if is_windows {
        format!("myownllm-{platform}.zip")
    } else {
        format!("myownllm-{platform}.tar.gz")
    };
    let url =
        format!("https://github.com/mrjeeves/MyOwnLLM/releases/download/{tag}/{archive_name}");
    let archive_path = staging.join(&archive_name);
    let _ = fs::remove_file(&archive_path);
    let _ = fs::remove_file(staging.join(format!("myownllm{exe_suffix}")));

    println!("cargo:warning=[download] {url}");
    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "-o"])
        .arg(&archive_path)
        .arg(&url)
        .status()
        .map_err(|e| format!("curl spawn failed: {e} (install curl, then re-run the build)"))?;
    if !status.success() {
        return Err(format!(
            "curl exited with {status} fetching {url} — is tag {tag} released with a \
             {archive_name} asset? (bump .myownllm-rev to a released MyOwnLLM tag, or build a \
             sibling MyOwnLLM checkout / set MYO_MYOWNLLM_BIN for dev)"
        ));
    }
    let archive_size = fs::metadata(&archive_path)
        .map_err(|e| format!("stat {}: {e}", archive_path.display()))?
        .len();
    if archive_size < 1024 {
        return Err(format!(
            "downloaded archive {} is only {archive_size} bytes — likely an error page",
            archive_path.display()
        ));
    }

    println!("cargo:warning=[extract] {}", archive_path.display());
    if is_windows {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                    archive_path.display(),
                    staging.display()
                ),
            ])
            .output()
            .map_err(|e| format!("powershell spawn failed: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Expand-Archive failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    } else {
        let output = Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(staging)
            .output()
            .map_err(|e| format!("tar spawn failed: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "tar failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }

    let bin = staging.join(format!("myownllm{exe_suffix}"));
    if !bin.exists() {
        return Err(format!(
            "extracted archive but `{}` not found",
            bin.display()
        ));
    }
    validate_executable_magic(&bin)?;
    Ok(bin)
}

/// Verify the bytes look like a real executable (ELF / PE / Mach-O, ≥ 1 MiB),
/// catching truncated downloads and HTML error pages renamed to a binary.
fn validate_executable_magic(path: &Path) -> Result<(), String> {
    use std::io::Read;
    let meta = fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
    if meta.len() < 1_048_576 {
        return Err(format!(
            "{} is only {} bytes (< 1 MiB) — too small to be a real myownllm build",
            path.display(),
            meta.len()
        ));
    }
    let mut head = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .map_err(|e| format!("read magic {}: {e}", path.display()))?;
    let looks_pe = head[0..2] == *b"MZ";
    let looks_elf = head == [0x7f, b'E', b'L', b'F'];
    let looks_macho = matches!(
        u32::from_le_bytes(head),
        0xFEED_FACE | 0xFEED_FACF | 0xCAFE_BABE | 0xBEBA_FECA
    ) || matches!(
        u32::from_be_bytes(head),
        0xFEED_FACE | 0xFEED_FACF | 0xCAFE_BABE | 0xBEBA_FECA
    );
    if !(looks_pe || looks_elf || looks_macho) {
        return Err(format!(
            "{} doesn't look like an executable (first 4 bytes: {head:02x?}) — corrupt download",
            path.display()
        ));
    }
    Ok(())
}

/// True when `dst` already mirrors `src` — same non-zero size and `dst` no
/// older than `src`. Lets override/sibling bundling skip a redundant copy of an
/// unchanged engine, while a *rebuilt* source (newer mtime, or different size)
/// still triggers a fresh copy — so iterating on the engine at the same version
/// is actually reflected in what Myo runs.
fn up_to_date(src: &Path, dst: &Path) -> bool {
    let (Ok(s), Ok(d)) = (fs::metadata(src), fs::metadata(dst)) else {
        return false;
    };
    if d.len() == 0 || s.len() != d.len() {
        return false;
    }
    match (s.modified(), d.modified()) {
        (Ok(sm), Ok(dm)) => dm >= sm,
        _ => false,
    }
}

/// Find a built `myownllm` in a sibling MyOwnLLM checkout. The crate lives under
/// `MyOwnLLM/src-tauri`, so its binary is at `MyOwnLLM/src-tauri/target/<profile>/`
/// (with a plain `MyOwnLLM/target/<profile>/` fallback for non-standard layouts).
fn find_sibling_engine_binary(crate_dir: &Path, exe_suffix: &str) -> Option<PathBuf> {
    let bin_name = format!("myownllm{exe_suffix}");
    // crate_dir = .../Myo/src-tauri → parent .../Myo → parent .../ → join MyOwnLLM
    let root = crate_dir.parent()?.parent()?.join("MyOwnLLM");
    let bases = [root.join("src-tauri").join("target"), root.join("target")];
    let mut found = None;
    for base in &bases {
        for profile in ["release", "debug"] {
            let p = base.join(profile).join(&bin_name);
            // Watch each candidate (even when absent now) so a freshly (re)built
            // sibling re-triggers the bundle on the next Myo build — otherwise an
            // engine rebuilt at the same version wouldn't be picked up.
            println!("cargo:rerun-if-changed={}", p.display());
            if found.is_none() && p.exists() {
                found = Some(p);
            }
        }
    }
    found
}

/// Best-effort `<bin> --version` token, for an informational build log only.
fn engine_version(p: &Path) -> Result<String, String> {
    let out = Command::new(p)
        .arg("--version")
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("exit {:?}", out.status.code()));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    Ok(raw
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next_back()
        .unwrap_or("")
        .to_string())
}
