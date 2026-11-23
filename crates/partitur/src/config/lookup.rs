//! Where a bare adapter command is looked up: the folders of a process environment's `PATH`
//! and, on Windows, the extensions of its `PATHEXT`, with this process's variables as the
//! fallback.
//!
//! On Windows environment names are compared ignoring case, a name without an extension is
//! tried only with each listed extension, and a found command runs by its full path.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where a bare adapter command is looked up and how a found command runs.
///
/// Off Windows `extensions` is empty and `full_path` false: a bare name is a file of a folder
/// and runs as configured. On Windows a name without an extension is tried only as `name + ext`
/// for each of `extensions` in order, never as written, so `npx` finds `npx.cmd` and not Node's
/// sh script `npx` beside it, and a found command runs by its full path. An extension is tried
/// in lowercase and then as listed: the list is uppercase by default, and the file system there
/// ignores case, so trying `.cmd` first names the found file `npx.cmd`, as npm writes it, where
/// `.CMD` would name it `npx.CMD`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lookup {
    /// The folders searched, in order.
    pub folders: Vec<PathBuf>,
    /// The extensions a name without one is tried with, in order; empty off Windows.
    pub extensions: Vec<String>,
    /// Whether a found command runs by the found file's path instead of as configured.
    pub full_path: bool,
}

/// The extensions tried when no `PATHEXT` is set.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Whether the environment variable names `a` and `b` are the same, ignoring case when
/// `fold_case` (Windows names are case-insensitive, others are not).
pub fn env_name_eq(a: &str, b: &str, fold_case: bool) -> bool {
    if fold_case {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

impl Lookup {
    /// The lookup for the environment `base`: the folders of its last `PATH` (the one a child
    /// process gets), else those of `process_path`; on Windows (`windows`) the names are
    /// compared ignoring case, the extensions come from the last `PATHEXT` of `base`, else
    /// `process_pathext`, else `.COM;.EXE;.BAT;.CMD`, split on `;` without empty entries, and
    /// found commands run by their full path.
    pub fn from_env(
        base: &[(String, String)],
        process_path: Option<OsString>,
        process_pathext: Option<OsString>,
        windows: bool,
    ) -> Self {
        let last = |wanted: &str| {
            base.iter()
                .rev()
                .find(|(name, _)| env_name_eq(name, wanted, windows))
                .map(|(_, value)| OsString::from(value))
        };
        let folders = last("PATH")
            .or(process_path)
            .map(|path| env::split_paths(&path).collect())
            .unwrap_or_default();
        let split = |list: &str| -> Vec<String> {
            list.split(';')
                .filter(|ext| !ext.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let extensions = if windows {
            let listed = last("PATHEXT")
                .or(process_pathext)
                .map(|ext| split(&ext.to_string_lossy()))
                .unwrap_or_default();
            // A list with nothing in it counts as none, so a bare name is never tried as written.
            if listed.is_empty() {
                split(DEFAULT_PATHEXT)
            } else {
                listed
            }
        } else {
            Vec::new()
        };
        Self {
            folders,
            extensions,
            full_path: windows,
        }
    }

    /// The file `command` names in one of the folders, the first folder's first match.
    pub(crate) fn find(&self, command: &Path) -> Option<PathBuf> {
        self.folders
            .iter()
            .find_map(|folder| self.find_in(folder, command))
    }

    /// The file `command` names in `folder`, if any.
    fn find_in(&self, folder: &Path, command: &Path) -> Option<PathBuf> {
        if self.extensions.is_empty() || command.extension().is_some() {
            return Some(folder.join(command)).filter(|file| file.is_file());
        }
        self.extensions
            .iter()
            .flat_map(|ext| [ext.to_lowercase(), ext.clone()])
            .map(|ext| {
                let mut name = command.as_os_str().to_owned();
                name.push(ext);
                folder.join(name)
            })
            .find(|file| file.is_file())
    }
}

/// The lookup for `base` with this process's `PATH` and `PATHEXT` as the fallback.
pub(super) fn process_lookup(base: &[(String, String)]) -> Lookup {
    Lookup::from_env(
        base,
        env::var_os("PATH"),
        env::var_os("PATHEXT"),
        cfg!(windows),
    )
}
