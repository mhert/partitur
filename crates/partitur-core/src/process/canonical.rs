//! Canonical paths in the form `git` prints them. On Windows the standard library resolves a path
//! to a verbatim `\\?\C:\…` path, which never equals the `C:\…` path git reports for the same
//! folder; `canonical` returns the plain form instead, and behaves like `std::fs::canonicalize`
//! everywhere else.

use std::io;
use std::path::{Path, PathBuf};

/// The canonical, absolute form of `path` with every symlink and dot segment resolved, without
/// the `\\?\` verbatim prefix on Windows.
///
/// # Errors
///
/// Returns the I/O error when `path` does not exist or cannot be resolved.
pub fn canonical(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::canonical;

    #[test]
    fn canonical_resolves_a_relative_path_and_dot_segments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let real = canonical(dir.path()).unwrap();

        assert_eq!(
            canonical(real.join("a/../a")).unwrap(),
            canonical(real.join("a")).unwrap()
        );
        assert!(canonical(real.join("missing")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn canonical_has_no_verbatim_prefix() {
        let resolved = canonical(std::env::temp_dir()).unwrap();
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
    }
}
