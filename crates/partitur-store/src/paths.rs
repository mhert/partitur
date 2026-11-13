//! Where the store keeps its files.

use std::path::PathBuf;

use partitur_core::{ProjectId, SessionId};

const PROJECTS_FILE: &str = "projects.json";
const PROJECTS_DIR: &str = "projects";
const HISTORY_SUFFIX: &str = "_history.jsonl";
const META_SUFFIX: &str = "_meta.json";
const SETTINGS_FILE: &str = "settings.toml";

/// The file layout under one data directory.
///
/// ```text
/// <dir>/projects.json
/// <dir>/projects/<project-id>/settings.toml
/// <dir>/projects/<project-id>/<session-id>/<session-id>_history.jsonl
/// <dir>/projects/<project-id>/<session-id>/<session-id>_meta.json
/// ```
#[derive(Debug, Clone)]
pub struct StatePaths {
    data_dir: PathBuf,
}

impl StatePaths {
    /// Lays the store out below `data_dir`. Nothing is created until the first write.
    pub fn under(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// The file listing the open projects.
    pub fn projects_file(&self) -> PathBuf {
        self.data_dir.join(PROJECTS_FILE)
    }

    /// The file holding the settings of `project`.
    pub fn settings_file(&self, project: &ProjectId) -> PathBuf {
        self.project_dir(project).join(SETTINGS_FILE)
    }

    /// The folder holding everything stored for `session`.
    pub fn session_dir(&self, project: &ProjectId, session: &SessionId) -> PathBuf {
        self.project_dir(project).join(session.to_string())
    }

    /// The JSON-lines file holding the updates of `session`.
    pub fn history_file(&self, project: &ProjectId, session: &SessionId) -> PathBuf {
        self.session_dir(project, session)
            .join(format!("{session}{HISTORY_SUFFIX}"))
    }

    /// The JSON file holding the session record of `session`.
    pub fn meta_file(&self, project: &ProjectId, session: &SessionId) -> PathBuf {
        self.session_dir(project, session)
            .join(format!("{session}{META_SUFFIX}"))
    }

    /// The folder that holds one sub-folder per project.
    pub(crate) fn projects_dir(&self) -> PathBuf {
        self.data_dir.join(PROJECTS_DIR)
    }

    /// The folder holding the settings and the session folders of `project`.
    pub(crate) fn project_dir(&self, project: &ProjectId) -> PathBuf {
        self.projects_dir().join(project.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn every_file_lives_at_its_documented_place() {
        let paths = StatePaths::under("/data");
        let project = ProjectId::new();
        let session = SessionId::new();

        assert_eq!(paths.projects_file(), Path::new("/data/projects.json"));
        assert_eq!(
            paths.settings_file(&project),
            Path::new(&format!("/data/projects/{project}/settings.toml"))
        );
        assert_eq!(
            paths.session_dir(&project, &session),
            Path::new(&format!("/data/projects/{project}/{session}"))
        );
        assert_eq!(
            paths.history_file(&project, &session),
            Path::new(&format!(
                "/data/projects/{project}/{session}/{session}_history.jsonl"
            ))
        );
        assert_eq!(
            paths.meta_file(&project, &session),
            Path::new(&format!(
                "/data/projects/{project}/{session}/{session}_meta.json"
            ))
        );
    }
}
