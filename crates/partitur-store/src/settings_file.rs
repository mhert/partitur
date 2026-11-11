//! A project's `settings.toml`: its template and how it is read.
//!
//! The file has the sections `[project]`, `[repository.<name>]` and
//! `[repository.<name>.folder."<path>"]`. Every setting value is a string. Reading reports the
//! first mistake with the section it is in, so the user can find it in the file.

use partitur_core::{Level, ProjectSettings, RepositoryId, RepositorySettings, SettingsLayer};
use toml::{Table, Value};

/// What a new `settings.toml` contains: comments only, so every setting stays at its default.
pub(crate) const TEMPLATE: &str = r#"# Settings for this project. They override config.toml and are overridden by
# more specific sections below. Remove the leading # of a line to use it.
#
# Three kinds of section exist, from least to most specific:
#
#   [project]                                    the whole project
#   [repository.<name>]                          one repository of the project
#   [repository.<name>.folder."<path>"]          a folder inside a repository
#
# worktree_path decides where the worktree of a branch is created. It may use the
# placeholders {repo_path} (the repository's folder), {repo} (its name), {project}
# (the project's name) and {branch} (the branch name). When no section sets it, the
# pattern is {repo_path}/../{repo}-wt/{branch}. Only a leading ~ or ~/ stands for the
# home folder. worktree_path cannot be set for a folder.
#
# [project]
# worktree_path = "~/worktrees/{project}/{repo}/{branch}"
#
# [repository.api]
# worktree_path = "{repo_path}/../api-trees/{branch}"
"#;

/// Reads the text of a `settings.toml`.
///
/// The error names the section the mistake is in, for example
/// `[repository.api] worktree_path cannot be set for a folder`.
pub(crate) fn parse(text: &str) -> Result<ProjectSettings, String> {
    let table: Table = text
        .parse()
        .map_err(|error: toml::de::Error| error.to_string())?;
    let mut settings = ProjectSettings::default();
    for (name, value) in &table {
        match name.as_str() {
            "project" => {
                let section = section_table("[project]", value)?;
                fill(&mut settings.project, Level::Project, "[project]", section)?;
            }
            "repository" => {
                for (repo, value) in section_table("[repository]", value)? {
                    let path = format!("repository.{repo}");
                    let label = format!("[{path}]");
                    let id = RepositoryId::try_from(repo.as_str())
                        .map_err(|error| format!("{label} {error}"))?;
                    let section = section_table(&label, value)?;
                    settings
                        .repositories
                        .insert(id, repository(&path, section)?);
                }
            }
            other => return Err(format!("unknown section [{other}]")),
        }
    }
    Ok(settings)
}

/// Reads the section `[path]` of a repository and the folder sections below it.
fn repository(path: &str, section: &Table) -> Result<RepositorySettings, String> {
    let mut settings = RepositorySettings::default();
    fill(
        &mut settings.layer,
        Level::Repository,
        &format!("[{path}]"),
        section,
    )?;
    if let Some(folders) = section.get("folder") {
        let folders = section_table(&format!("[{path}.folder]"), folders)?;
        for (folder, value) in folders {
            let label = format!("[{path}.folder.\"{folder}\"]");
            let mut layer = SettingsLayer::default();
            fill(
                &mut layer,
                Level::Folder,
                &label,
                section_table(&label, value)?,
            )?;
            settings.folders.insert(folder.clone(), layer);
        }
    }
    Ok(settings)
}

/// Sets every setting of `section` in `layer`; a repository's `folder` table is not a setting.
fn fill(
    layer: &mut SettingsLayer,
    level: Level,
    label: &str,
    section: &Table,
) -> Result<(), String> {
    for (key, value) in section {
        if level == Level::Repository && key == "folder" {
            continue;
        }
        let Value::String(value) = value else {
            return Err(format!("{label} {key} must be a string"));
        };
        layer
            .set(level, key, value)
            .map_err(|error| format!("{label} {error}"))?;
    }
    Ok(())
}

fn section_table<'a>(label: &str, value: &'a Value) -> Result<&'a Table, String> {
    value
        .as_table()
        .ok_or_else(|| format!("{label} must be a section"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use partitur_core::WorktreePattern;

    use super::*;

    #[test]
    fn every_level_of_the_file_is_read() {
        let s = parse(
            "[project]\nworktree_path = \"~/wt/{project}/{repo}/{branch}\"\n\
             [repository.api]\nworktree_path = \"{repo_path}/../api-trees/{branch}\"\n\
             [repository.api.folder.\"services/auth\"]\n\
             [repository.web]\nworktree_path = \"/trees/{branch}\"\n",
        )
        .unwrap();
        assert_eq!(
            s.project.worktree_path().map(WorktreePattern::as_str),
            Some("~/wt/{project}/{repo}/{branch}")
        );
        let api = &s.repositories[&RepositoryId::try_from("api").unwrap()];
        assert_eq!(
            api.layer.worktree_path().map(WorktreePattern::as_str),
            Some("{repo_path}/../api-trees/{branch}")
        );
        assert!(api.folders.contains_key("services/auth"));
        let web = &s.repositories[&RepositoryId::try_from("web").unwrap()];
        assert_eq!(
            web.layer.worktree_path().map(WorktreePattern::as_str),
            Some("/trees/{branch}")
        );
        assert!(web.folders.is_empty());
    }

    #[test]
    fn a_bad_file_says_where() {
        for (text, detail) in [
            ("[projekt]\n", "unknown section [projekt]"),
            (
                "[project]\nworktree_path = 3\n",
                "[project] worktree_path must be a string",
            ),
            (
                "[project]\nnope = \"x\"\n",
                "[project] unknown setting nope",
            ),
            (
                "[repository.api.folder.\"a\"]\nworktree_path = \"x\"\n",
                "[repository.api.folder.\"a\"] worktree_path cannot be set for a folder",
            ),
            (
                "[repository.api]\nworktree_path = 3\n",
                "[repository.api] worktree_path must be a string",
            ),
            (
                "[project]\nworktree_path = \"{title}\"\n",
                "[project] unknown placeholder {title}",
            ),
            ("project = 3\n", "[project] must be a section"),
            ("repository = 3\n", "[repository] must be a section"),
            (
                "[repository]\napi = 3\n",
                "[repository.api] must be a section",
            ),
            (
                "[repository.api]\nfolder = 3\n",
                "[repository.api.folder] must be a section",
            ),
            (
                "[repository.api.folder]\na = 3\n",
                "[repository.api.folder.\"a\"] must be a section",
            ),
            (
                "[repository.\"a/b\"]\n",
                "[repository.a/b] repository id must be",
            ),
            ("[project\n", ""),
        ] {
            let error = parse(text).unwrap_err();
            assert!(error.starts_with(detail), "{text}: {error}");
            assert!(!detail.is_empty() || !error.is_empty(), "{text}");
        }
    }

    #[test]
    fn the_template_parses_to_nothing() {
        assert_eq!(parse(TEMPLATE).unwrap(), ProjectSettings::default());
        assert!(
            TEMPLATE
                .lines()
                .all(|line| line.is_empty() || line.starts_with('#'))
        );
    }
}
