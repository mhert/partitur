//! The global settings layer the configuration file sets: its top-level `worktree_path` and
//! `link_opener`, each validated as the core's settings layer does, an invalid value stopping
//! the start with an error naming the file.

use std::path::Path;

use partitur_core::{Level, SettingsLayer};

use super::{Config, ConfigError};

/// The global settings layer `config` sets: its `worktree_path` and `link_opener`, each when
/// it has one. `path` is the config file, named in the error.
///
/// # Errors
///
/// Returns [`ConfigError::Setting`] when `worktree_path` is not a valid pattern or
/// `link_opener` names no program.
pub fn global_settings(config: &Config, path: &Path) -> Result<SettingsLayer, ConfigError> {
    let mut layer = SettingsLayer::default();
    let keys = [
        ("worktree_path", &config.worktree_path),
        ("link_opener", &config.link_opener),
    ];
    for (key, value) in keys {
        if let Some(value) = value {
            layer
                .set(Level::Global, key, value)
                .map_err(|source| ConfigError::Setting {
                    path: path.to_path_buf(),
                    key,
                    source,
                })?;
        }
    }
    Ok(layer)
}
