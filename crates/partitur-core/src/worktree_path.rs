//! The pattern that decides where a branch's worktree lives on disk.
//!
//! A pattern is plain text with the placeholders `{repo_path}`, `{repo}`, `{project}` and
//! `{branch}`. Resolving it substitutes the placeholders, expands a leading `~` to the home
//! folder, puts a path that is still relative under the repository's folder, and normalises
//! `.` and `..` components lexically, without touching the file system. The result is
//! always an absolute path, so it names the same folder for git, which runs in the
//! repository, and for Partitur, which runs wherever it was started.

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// The pattern used when no settings layer sets one: next to the repository, grouped by name.
pub const DEFAULT_WORKTREE_PATH: &str = "{repo_path}/../{repo}-wt/{branch}";

/// Why a worktree path pattern could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PatternError {
    /// A `{name}` that is not one of the four known placeholders.
    #[error(
        "unknown placeholder {{{0}}} in worktree_path; use {{repo_path}}, {{repo}}, {{project}} or {{branch}}"
    )]
    UnknownPlaceholder(String),
    /// A `{` without a closing `}`.
    #[error("unclosed {{ in worktree_path")]
    Unclosed,
    /// A leading `~` while no home folder is known.
    #[error("worktree_path starts with ~, but there is no home folder to put there")]
    NoHome,
}

/// The values a pattern's placeholders stand for.
#[derive(Debug, Clone, Copy)]
pub struct PatternValues<'a> {
    /// The repository's folder.
    pub repo_path: &'a Path,
    /// The repository's name.
    pub repo: &'a str,
    /// The project's name.
    pub project: &'a str,
    /// The branch the worktree checks out.
    pub branch: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    RepoPath,
    Repo,
    Project,
    Branch,
}

/// A parsed worktree path pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePattern {
    text: String,
    parts: Vec<Part>,
}

impl Default for WorktreePattern {
    fn default() -> Self {
        Self {
            text: DEFAULT_WORKTREE_PATH.to_owned(),
            parts: vec![
                Part::RepoPath,
                Part::Text("/../".to_owned()),
                Part::Repo,
                Part::Text("-wt/".to_owned()),
                Part::Branch,
            ],
        }
    }
}

impl WorktreePattern {
    /// Parses `text` into literal text and placeholders.
    ///
    /// # Errors
    ///
    /// Returns [`PatternError::UnknownPlaceholder`] for a `{name}` that is not a placeholder
    /// and [`PatternError::Unclosed`] for a `{` without a closing `}`.
    pub fn parse(text: &str) -> Result<Self, PatternError> {
        let mut parts = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            if open > 0 {
                parts.push(Part::Text(rest.get(..open).unwrap_or_default().to_owned()));
            }
            let after = rest.get(open + 1..).unwrap_or_default();
            let close = after.find('}').ok_or(PatternError::Unclosed)?;
            let name = after.get(..close).unwrap_or_default();
            parts.push(match name {
                "repo_path" => Part::RepoPath,
                "repo" => Part::Repo,
                "project" => Part::Project,
                "branch" => Part::Branch,
                other => return Err(PatternError::UnknownPlaceholder(other.to_owned())),
            });
            rest = after.get(close + 1..).unwrap_or_default();
        }
        if !rest.is_empty() {
            parts.push(Part::Text(rest.to_owned()));
        }
        Ok(Self {
            text: text.to_owned(),
            parts,
        })
    }

    /// The pattern as the user wrote it.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The worktree folder for `values`, an absolute path when `values.repo_path` is one.
    ///
    /// A leading `~` (alone or followed by `/`) expands to `home`; any other `~` stays
    /// literal. A path that is relative after that is taken relative to `values.repo_path`.
    /// `.` and `..` components are resolved lexically, and a `..` at the root is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`PatternError::NoHome`] for a leading `~` when `home` is `None`.
    pub fn resolve(
        &self,
        values: &PatternValues<'_>,
        home: Option<&Path>,
    ) -> Result<PathBuf, PatternError> {
        let mut text = String::new();
        for part in &self.parts {
            match part {
                Part::Text(literal) => text.push_str(literal),
                Part::RepoPath => text.push_str(&values.repo_path.to_string_lossy()),
                Part::Repo => text.push_str(values.repo),
                Part::Project => text.push_str(values.project),
                Part::Branch => text.push_str(values.branch),
            }
        }
        let expanded = expand_home(&text, home)?;
        Ok(normalise(&values.repo_path.join(expanded)))
    }
}

/// `text` with a leading `~` (alone or followed by `/`) replaced by `home`.
///
/// # Errors
///
/// Returns [`PatternError::NoHome`] for such a `~` when `home` is `None`.
fn expand_home(text: &str, home: Option<&Path>) -> Result<PathBuf, PatternError> {
    let tail = text
        .strip_prefix('~')
        .filter(|t| t.is_empty() || t.starts_with('/'));
    match (home, tail) {
        (Some(home), Some(tail)) => Ok(home.join(tail.trim_start_matches('/'))),
        (None, Some(_)) => Err(PatternError::NoHome),
        (_, None) => Ok(PathBuf::from(text)),
    }
}

fn normalise(path: &Path) -> PathBuf {
    let mut kept: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match kept.last() {
                Some(Component::Normal(_)) => {
                    kept.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => kept.push(component),
            },
            other => kept.push(other),
        }
    }
    kept.iter().collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn values() -> PatternValues<'static> {
        PatternValues {
            repo_path: Path::new("/code/api"),
            repo: "api",
            project: "shop",
            branch: "feat/login-retry",
        }
    }

    #[test]
    fn the_default_pattern_puts_worktrees_next_to_the_repository() {
        assert_eq!(
            WorktreePattern::default().resolve(&values(), None).unwrap(),
            PathBuf::from("/code/api-wt/feat/login-retry")
        );
        assert_eq!(
            WorktreePattern::default(),
            WorktreePattern::parse(DEFAULT_WORKTREE_PATH).unwrap()
        );
        assert_eq!(WorktreePattern::default().as_str(), DEFAULT_WORKTREE_PATH);
    }

    #[test]
    fn every_placeholder_is_substituted() {
        let p = WorktreePattern::parse("/w/{project}/{repo}/{branch}{repo_path}").unwrap();
        assert_eq!(
            p.resolve(&values(), None).unwrap(),
            PathBuf::from("/w/shop/api/feat/login-retry/code/api")
        );
    }

    #[test]
    fn a_leading_tilde_expands_and_nothing_else_does() {
        let values = values();
        let home = Path::new("/home/me");
        let resolve = |text: &str, home| {
            WorktreePattern::parse(text)
                .unwrap()
                .resolve(&values, home)
                .unwrap()
        };
        assert_eq!(
            resolve("~/wt/{project}/{repo}/{branch}", Some(home)),
            PathBuf::from("/home/me/wt/shop/api/feat/login-retry")
        );
        assert_eq!(resolve("~", Some(home)), PathBuf::from("/home/me"));
        assert_eq!(
            resolve("/x/~/{branch}", Some(home)),
            PathBuf::from("/x/~/feat/login-retry")
        );
        assert_eq!(
            resolve("~user/{branch}", Some(home)),
            PathBuf::from("/code/api/~user/feat/login-retry")
        );
    }

    #[test]
    fn a_leading_tilde_without_a_home_folder_is_an_error() {
        let values = values();
        for text in ["~", "~/a/{branch}"] {
            assert_eq!(
                WorktreePattern::parse(text).unwrap().resolve(&values, None),
                Err(PatternError::NoHome)
            );
        }
        assert_eq!(
            PatternError::NoHome.to_string(),
            "worktree_path starts with ~, but there is no home folder to put there"
        );
    }

    #[test]
    fn a_relative_pattern_is_resolved_against_the_repository() {
        let values = values();
        let resolve = |text: &str| {
            WorktreePattern::parse(text)
                .unwrap()
                .resolve(&values, None)
                .unwrap()
        };
        assert_eq!(
            resolve("../wt/{branch}"),
            PathBuf::from("/code/wt/feat/login-retry")
        );
        assert_eq!(
            resolve("trees/{branch}"),
            PathBuf::from("/code/api/trees/feat/login-retry")
        );
        assert_eq!(resolve("."), PathBuf::from("/code/api"));
    }

    #[test]
    fn unknown_and_unclosed_placeholders_are_errors() {
        assert_eq!(
            WorktreePattern::parse("{repo}/{title}"),
            Err(PatternError::UnknownPlaceholder("title".into()))
        );
        assert_eq!(WorktreePattern::parse("{repo"), Err(PatternError::Unclosed));
        assert_eq!(
            PatternError::UnknownPlaceholder("title".into()).to_string(),
            "unknown placeholder {title} in worktree_path; use {repo_path}, {repo}, {project} or {branch}"
        );
        assert_eq!(
            PatternError::Unclosed.to_string(),
            "unclosed { in worktree_path"
        );
    }

    #[test]
    fn the_text_is_kept_as_written() {
        assert_eq!(
            WorktreePattern::parse("/a/{repo}").unwrap().as_str(),
            "/a/{repo}"
        );
    }

    #[test]
    fn dot_dot_and_dot_are_resolved_lexically() {
        let values = PatternValues {
            branch: "x",
            ..values()
        };
        let resolve = |text: &str| {
            WorktreePattern::parse(text)
                .unwrap()
                .resolve(&values, None)
                .unwrap()
        };
        assert_eq!(resolve("/a/b/../c/./{branch}"), PathBuf::from("/a/c/x"));
        assert_eq!(resolve("/../x"), PathBuf::from("/x"));
        assert_eq!(resolve("/a/../../x"), PathBuf::from("/x"));
        let relative = PatternValues {
            repo_path: Path::new("./api"),
            ..values
        };
        let pattern = WorktreePattern::parse("../../{branch}").unwrap();
        assert_eq!(
            pattern.resolve(&relative, None).unwrap(),
            PathBuf::from("../x")
        );
    }
}
