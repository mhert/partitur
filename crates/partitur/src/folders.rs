//! The folders Partitur keeps its configuration and state in: the user's own, or a throwaway
//! pair inside a fresh temporary directory that is removed when the value drops.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context;
use directories::ProjectDirs;
use tempfile::TempDir;

/// Where the configuration file and the data folder live.
#[derive(Debug)]
pub enum Folders {
    /// The user's folders, as the platform names them. Boxed, because on Windows they are far
    /// larger than the throwaway pair.
    User(Box<ProjectDirs>),
    /// A fresh temporary directory holding `config/config.toml` and `data`; it is removed
    /// when the value drops.
    Throwaway {
        /// The temporary directory.
        root: TempDir,
        /// Its `data` folder.
        data: PathBuf,
    },
}

impl Folders {
    /// The user's folders, or `None` when the platform names no home folder.
    #[must_use]
    pub fn user() -> Option<Self> {
        ProjectDirs::from("", "mhert", "partitur").map(|dirs| Self::User(Box::new(dirs)))
    }

    /// A pair of folders in a fresh temporary directory, which touches none of the user's.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the temporary directory cannot be created.
    pub fn throwaway() -> io::Result<Self> {
        let root = TempDir::with_prefix("partitur-smoke-")?;
        let data = root.path().join("data");
        Ok(Self::Throwaway { root, data })
    }

    /// The folders to work on: throwaway ones for the smoke check, the user's otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when the throwaway directory cannot be created or, for the user's
    /// folders, when the platform names no home folder.
    pub fn open(throwaway: bool) -> anyhow::Result<Self> {
        if throwaway {
            Self::throwaway().context("failed to create the throwaway folders of the smoke check")
        } else {
            Self::user().context("there is no home folder to keep the configuration and state in")
        }
    }

    /// The configuration file; it need not exist.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        match self {
            Self::User(dirs) => dirs.config_dir().join("config.toml"),
            Self::Throwaway { root, .. } => root.path().join("config").join("config.toml"),
        }
    }

    /// The folder projects, sessions and transcripts are kept in.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        match self {
            Self::User(dirs) => dirs.data_dir(),
            Self::Throwaway { data, .. } => data,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    /// The temporary directory holding both folders: the parent of the data folder.
    fn root_of(folders: &Folders) -> PathBuf {
        folders.data_dir().parent().unwrap().to_path_buf()
    }

    #[test]
    fn the_throwaway_folders_lie_in_a_fresh_temporary_directory() {
        let folders = Folders::throwaway().unwrap();
        let root = root_of(&folders);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(root.is_dir());
        assert_eq!(
            folders.config_file(),
            root.join("config").join("config.toml")
        );
        assert_eq!(folders.data_dir(), root.join("data"));
        assert!(folders.config_file().starts_with(std::env::temp_dir()));
    }

    #[test]
    fn the_throwaway_directory_is_gone_after_the_drop() {
        let folders = Folders::throwaway().unwrap();
        let root = root_of(&folders);
        drop(folders);
        assert!(!root.exists());
    }

    #[test]
    fn two_throwaways_differ() {
        let (a, b) = (Folders::throwaway().unwrap(), Folders::throwaway().unwrap());
        assert_ne!(root_of(&a), root_of(&b));
    }

    #[test]
    fn the_smoke_check_gets_throwaway_folders_and_a_run_the_users() {
        assert!(matches!(
            Folders::open(true).unwrap(),
            Folders::Throwaway { .. }
        ));
        assert!(matches!(Folders::open(false).unwrap(), Folders::User(_)));
    }

    #[test]
    fn the_user_folders_are_the_platforms_for_partitur() {
        let dirs = ProjectDirs::from("", "mhert", "partitur");
        let folders = Folders::user();
        assert_eq!(
            folders.as_ref().map(Folders::config_file),
            dirs.as_ref()
                .map(|dirs| dirs.config_dir().join("config.toml"))
        );
        assert_eq!(
            folders
                .as_ref()
                .map(|folders| folders.data_dir().to_path_buf()),
            dirs.as_ref().map(|dirs| dirs.data_dir().to_path_buf())
        );
        #[cfg(target_os = "linux")]
        assert!(
            folders.is_some_and(|folders| folders.config_file().ends_with("partitur/config.toml"))
        );
    }
}
