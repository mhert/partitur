//! Where the store keeps its files.

use std::path::PathBuf;

use partitur_core::{ProjectId, SessionId};

const PROJECTS_FILE: &str = "projects.json";
const PROJECTS_DIR: &str = "projects";
const SESSIONS_FILE: &str = "sessions.json";

/// The file layout under one data directory.
///
/// ```text
/// <dir>/projects.json
/// <dir>/projects/<project-id>/sessions.json
/// <dir>/projects/<project-id>/<session-id>.jsonl
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

    /// The file listing the sessions of `project`.
    pub fn sessions_file(&self, project: &ProjectId) -> PathBuf {
        self.project_dir(project).join(SESSIONS_FILE)
    }

    /// The JSON-lines file holding the updates of `session`.
    pub fn transcript_file(&self, project: &ProjectId, session: &SessionId) -> PathBuf {
        self.project_dir(project).join(format!("{session}.jsonl"))
    }

    /// The folder that holds one sub-folder per project.
    pub(crate) fn projects_dir(&self) -> PathBuf {
        self.data_dir.join(PROJECTS_DIR)
    }

    fn project_dir(&self, project: &ProjectId) -> PathBuf {
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
            paths.sessions_file(&project),
            Path::new(&format!("/data/projects/{project}/sessions.json"))
        );
        assert_eq!(
            paths.transcript_file(&project, &session),
            Path::new(&format!("/data/projects/{project}/{session}.jsonl"))
        );
    }
}
