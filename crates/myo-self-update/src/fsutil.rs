//! Small filesystem helpers.

use std::path::Path;

/// Recursive size of a directory tree in bytes. Errors collapse to 0 — this
/// backs a "you can reclaim X" hint for staged-update leftovers, not anything
/// that needs to be exact.
pub fn dir_size_bytes(path: &Path) -> u64 {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path()));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_sums_nested_files() {
        let dir = std::env::temp_dir().join(format!("myo-fsutil-{}", std::process::id()));
        let nested = dir.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("a/one.bin"), [0u8; 10]).unwrap();
        std::fs::write(nested.join("two.bin"), [0u8; 25]).unwrap();
        assert_eq!(dir_size_bytes(&dir), 35);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_size_of_missing_path_is_zero() {
        assert_eq!(dir_size_bytes(Path::new("/no/such/myo/path/xyz")), 0);
    }
}
