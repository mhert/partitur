//! Settings that can be set at several levels, where the most specific level wins.
//!
//! A level is global (`config.toml`), project, repository, or a folder inside a repository.
//! Each level holds a [`SettingsLayer`]; [`ProjectSettings::layers`] orders the layers that
//! apply to a folder from most to least specific so a setting is read from the first layer
//! that has it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::ids::RepositoryId;
use crate::link_opener::{LinkOpener, LinkOpenerError};
use crate::worktree_path::{PatternError, WorktreePattern};

/// Where a settings layer sits in the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The user's `config.toml`.
    Global,
    /// The whole project.
    Project,
    /// One repository of the project.
    Repository,
    /// A folder inside a repository.
    Folder,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Repository => "repository",
            Self::Folder => "folder",
        })
    }
}

/// Why a setting could not be applied to a layer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingError {
    /// No setting has this key.
    #[error("unknown setting {0}")]
    UnknownKey(String),
    /// The setting exists but may not be set at this level.
    #[error("{key} cannot be set for a {level}")]
    NotAtLevel {
        /// The setting's key.
        key: String,
        /// The level that rejected it.
        level: Level,
    },
    /// The value of `worktree_path` is not a valid pattern.
    #[error(transparent)]
    Pattern(#[from] PatternError),
    /// The value of `link_opener` is not a usable command.
    #[error(transparent)]
    LinkOpener(#[from] LinkOpenerError),
}

/// Stores a setting's value in a layer.
type Setter = fn(&mut SettingsLayer, &str) -> Result<(), SettingError>;

/// Every setting key, the levels it may be set at, and how it is stored.
const KEYS: &[(&str, &[Level], Setter)] = &[
    (
        "worktree_path",
        &[Level::Global, Level::Project, Level::Repository],
        |layer, value| {
            layer.worktree_path = Some(WorktreePattern::parse(value)?);
            Ok(())
        },
    ),
    (
        "link_opener",
        &[Level::Global, Level::Project, Level::Repository],
        |layer, value| {
            layer.link_opener = Some(LinkOpener::parse(value)?);
            Ok(())
        },
    ),
];

/// The settings one level sets; a setting it leaves unset is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsLayer {
    worktree_path: Option<WorktreePattern>,
    link_opener: Option<LinkOpener>,
}

impl SettingsLayer {
    /// Sets the setting `key` to `value` for a layer at `level`.
    ///
    /// # Errors
    ///
    /// Returns [`SettingError::UnknownKey`] for a key no setting has,
    /// [`SettingError::NotAtLevel`] when the setting is not allowed at `level`, and
    /// [`SettingError::Pattern`] or [`SettingError::LinkOpener`] when `value` is not valid for
    /// the setting.
    pub fn set(&mut self, level: Level, key: &str, value: &str) -> Result<(), SettingError> {
        let (_, levels, setter) = KEYS
            .iter()
            .find(|(name, _, _)| *name == key)
            .ok_or_else(|| SettingError::UnknownKey(key.to_owned()))?;
        if !levels.contains(&level) {
            return Err(SettingError::NotAtLevel {
                key: key.to_owned(),
                level,
            });
        }
        setter(self, value)
    }

    /// The worktree path pattern this layer sets, if any.
    pub fn worktree_path(&self) -> Option<&WorktreePattern> {
        self.worktree_path.as_ref()
    }

    /// The command links are opened with that this layer sets, if any.
    pub fn link_opener(&self) -> Option<&LinkOpener> {
        self.link_opener.as_ref()
    }
}

/// The settings of one repository: its own layer and one layer per folder inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositorySettings {
    /// The repository-level layer.
    pub layer: SettingsLayer,
    /// Folder layers, keyed by the folder's path relative to the repository.
    pub folders: BTreeMap<String, SettingsLayer>,
}

/// The settings of one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    /// The project-level layer.
    pub project: SettingsLayer,
    /// Per-repository settings.
    pub repositories: BTreeMap<RepositoryId, RepositorySettings>,
}

impl ProjectSettings {
    /// The layers that apply to `folder` of `repository`, most specific first: the matching
    /// folder layers (deepest first), the repository, the project, then `global`.
    ///
    /// A folder layer matches when its key is `folder` or a parent of it, compared by path
    /// component, so `services` matches `services/auth` but not `servicesx`.
    pub fn layers<'a>(
        &'a self,
        global: &'a SettingsLayer,
        repository: &RepositoryId,
        folder: &str,
    ) -> Vec<&'a SettingsLayer> {
        let mut layers = Vec::new();
        let repo_settings = self.repositories.get(repository);
        if let Some(settings) = repo_settings {
            let folder = std::path::Path::new(folder);
            let mut matching: Vec<_> = settings
                .folders
                .iter()
                .filter(|(key, _)| folder.starts_with(key))
                .collect();
            matching.sort_by_key(|(key, _)| {
                std::cmp::Reverse(std::path::Path::new(key).components().count())
            });
            layers.extend(matching.into_iter().map(|(_, layer)| layer));
            layers.push(&settings.layer);
        }
        layers.push(&self.project);
        layers.push(global);
        layers
    }

    /// The worktree path pattern at `repository`'s own level, falling back to the default.
    pub fn worktree_path(
        &self,
        global: &SettingsLayer,
        repository: &RepositoryId,
    ) -> WorktreePattern {
        self.layers(global, repository, "")
            .into_iter()
            .find_map(SettingsLayer::worktree_path)
            .cloned()
            .unwrap_or_default()
    }

    /// The command links are opened with for a session at `repository`'s level, or at the
    /// project's when `repository` is `None`, falling back to `global` and then to the default.
    pub fn link_opener(
        &self,
        global: &SettingsLayer,
        repository: Option<&RepositoryId>,
    ) -> LinkOpener {
        let repository = repository.and_then(|id| self.repositories.get(id));
        [
            repository.map(|settings| &settings.layer),
            Some(&self.project),
            Some(global),
        ]
        .into_iter()
        .flatten()
        .find_map(SettingsLayer::link_opener)
        .cloned()
        .unwrap_or_default()
    }
}

/// The user's global settings from `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSettings {
    /// The global layer.
    pub layer: SettingsLayer,
    /// The user's home folder, used to expand a leading `~` in a worktree path.
    pub home: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn layer_with(pattern: &str) -> SettingsLayer {
        let mut layer = SettingsLayer::default();
        layer.set(Level::Project, "worktree_path", pattern).unwrap();
        layer
    }

    fn id(name: &str) -> RepositoryId {
        RepositoryId::try_from(name).unwrap()
    }

    #[test]
    fn worktree_path_is_allowed_everywhere_but_in_a_folder() {
        let mut layer = SettingsLayer::default();
        for level in [Level::Global, Level::Project, Level::Repository] {
            assert!(layer.set(level, "worktree_path", "{repo}").is_ok());
        }
        assert_eq!(
            layer.set(Level::Folder, "worktree_path", "{repo}"),
            Err(SettingError::NotAtLevel {
                key: "worktree_path".into(),
                level: Level::Folder
            })
        );
        assert_eq!(
            layer.set(Level::Project, "colour", "x"),
            Err(SettingError::UnknownKey("colour".into()))
        );
        assert_eq!(
            layer.set(Level::Project, "worktree_path", "{nope}"),
            Err(SettingError::Pattern(PatternError::UnknownPlaceholder(
                "nope".into()
            )))
        );
    }

    fn opener_with(value: &str) -> SettingsLayer {
        let mut layer = SettingsLayer::default();
        layer.set(Level::Project, "link_opener", value).unwrap();
        layer
    }

    #[test]
    fn link_opener_is_allowed_everywhere_but_in_a_folder_and_must_name_a_program() {
        let mut layer = SettingsLayer::default();
        for level in [Level::Global, Level::Project, Level::Repository] {
            assert!(layer.set(level, "link_opener", "firefox").is_ok());
        }
        assert_eq!(
            layer.set(Level::Folder, "link_opener", "firefox"),
            Err(SettingError::NotAtLevel {
                key: "link_opener".into(),
                level: Level::Folder
            })
        );
        assert_eq!(
            layer.set(Level::Project, "link_opener", " "),
            Err(SettingError::LinkOpener(LinkOpenerError::Empty))
        );
        assert_eq!(layer.link_opener().unwrap().to_string(), "firefox");
        assert!(layer.worktree_path().is_none());
        assert!(SettingsLayer::default().link_opener().is_none());
    }

    #[test]
    fn the_link_opener_comes_from_the_repository_then_the_project_then_global() {
        let global = opener_with("g");
        let mut settings = ProjectSettings {
            project: opener_with("p"),
            ..ProjectSettings::default()
        };
        settings.repositories.insert(
            id("api"),
            RepositorySettings {
                layer: opener_with("r"),
                folders: BTreeMap::new(),
            },
        );
        settings
            .repositories
            .insert(id("web"), RepositorySettings::default());
        let opener = |settings: &ProjectSettings, global, repository: Option<&str>| {
            let repository = repository.map(id);
            settings
                .link_opener(global, repository.as_ref())
                .to_string()
        };

        assert_eq!(opener(&settings, &global, Some("api")), "r");
        assert_eq!(opener(&settings, &global, Some("web")), "p");
        assert_eq!(opener(&settings, &global, Some("docs")), "p");
        assert_eq!(opener(&settings, &global, None), "p");
        let unset = ProjectSettings::default();
        assert_eq!(opener(&unset, &global, Some("api")), "g");
        assert_eq!(opener(&unset, &SettingsLayer::default(), None), "xdg-open");
    }

    #[test]
    fn a_rejected_value_leaves_the_layer_unchanged() {
        let mut layer = layer_with("/a/{branch}");
        layer.set(Level::Project, "worktree_path", "{").unwrap_err();
        assert_eq!(layer.worktree_path().unwrap().as_str(), "/a/{branch}");
        assert!(SettingsLayer::default().worktree_path().is_none());
    }

    #[test]
    fn errors_and_levels_read_as_sentences() {
        assert_eq!(
            SettingError::UnknownKey("colour".into()).to_string(),
            "unknown setting colour"
        );
        assert_eq!(
            SettingError::NotAtLevel {
                key: "worktree_path".into(),
                level: Level::Folder
            }
            .to_string(),
            "worktree_path cannot be set for a folder"
        );
        let names: Vec<_> = [
            Level::Global,
            Level::Project,
            Level::Repository,
            Level::Folder,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(names, ["global", "project", "repository", "folder"]);
    }

    #[test]
    fn the_most_specific_layer_wins() {
        let global = layer_with("/g/{branch}");
        let mut settings = ProjectSettings {
            project: layer_with("/p/{branch}"),
            ..ProjectSettings::default()
        };
        settings.repositories.insert(
            id("api"),
            RepositorySettings {
                layer: layer_with("/r/{branch}"),
                folders: BTreeMap::new(),
            },
        );
        assert_eq!(
            settings.worktree_path(&global, &id("api")).as_str(),
            "/r/{branch}"
        );
        assert_eq!(
            settings.worktree_path(&global, &id("web")).as_str(),
            "/p/{branch}"
        );
        assert_eq!(
            ProjectSettings::default()
                .worktree_path(&global, &id("web"))
                .as_str(),
            "/g/{branch}"
        );
        assert_eq!(
            ProjectSettings::default().worktree_path(&SettingsLayer::default(), &id("web")),
            WorktreePattern::default()
        );
    }

    #[test]
    fn a_repository_without_a_pattern_falls_through_to_the_project() {
        let mut settings = ProjectSettings {
            project: layer_with("/p/{branch}"),
            ..ProjectSettings::default()
        };
        settings
            .repositories
            .insert(id("api"), RepositorySettings::default());
        assert_eq!(
            settings
                .worktree_path(&SettingsLayer::default(), &id("api"))
                .as_str(),
            "/p/{branch}"
        );
    }

    #[test]
    fn a_deeper_folder_comes_before_a_shallower_one() {
        let global = SettingsLayer::default();
        let services = layer_with("/s");
        let auth = layer_with("/a");
        let repo = layer_with("/r");
        let project = layer_with("/p");
        let mut folders = BTreeMap::new();
        folders.insert("services".to_owned(), services.clone());
        folders.insert("services/auth".to_owned(), auth.clone());
        let mut settings = ProjectSettings {
            project: project.clone(),
            ..ProjectSettings::default()
        };
        settings.repositories.insert(
            id("api"),
            RepositorySettings {
                layer: repo.clone(),
                folders,
            },
        );

        let layers = settings.layers(&global, &id("api"), "services/auth/db");
        assert_eq!(layers, vec![&auth, &services, &repo, &project, &global]);
        let layers = settings.layers(&global, &id("api"), "services");
        assert_eq!(layers, vec![&services, &repo, &project, &global]);
        let layers = settings.layers(&global, &id("api"), "servicesx");
        assert_eq!(layers, vec![&repo, &project, &global]);
        let layers = settings.layers(&global, &id("web"), "services");
        assert_eq!(layers, vec![&project, &global]);
    }
}
