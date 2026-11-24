//! An orchestrator's permission requests decided on what the files port finds on the real
//! disk: the app asks [`DiskFiles`] about the paths a request names, and decides it once the
//! answer arrives.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::mpsc::UnboundedReceiver;
use partitur_core::testing::{
    Call, FixedClock, MemoryGit, MemoryStore, RecordingBackend, single_claude,
};
use partitur_core::{
    AgentUpdate, App, BackendEvent, BackendSessionId, FilesEvent, GlobalSettings, PermissionId,
    PermissionKind, PermissionOption, PermissionOptionId, ProjectId, Scope, SessionId, Status,
    ToolCallId, ToolKind,
};
use partitur_store::{DiskFiles, FileStore, StatePaths};

/// An app over the real disk through [`DiskFiles`], with the backend it records.
struct OnDisk {
    app: App,
    backend: RecordingBackend,
    events: UnboundedReceiver<FilesEvent>,
}

impl OnDisk {
    fn new(data: &Path) -> Self {
        let backend = RecordingBackend::new();
        let store = MemoryStore::new();
        let (files, events) = DiskFiles::start(FileStore::new(StatePaths::under(data)));
        let (backends, default) = single_claude();
        let app = App::load(
            Box::new(backend.clone()),
            Box::new(store.clone()),
            Box::new(store),
            Box::new(MemoryGit::new()),
            Box::new(files),
            backends,
            default,
            GlobalSettings::default(),
            Box::new(FixedClock::default()),
        )
        .unwrap();
        Self {
            app,
            backend,
            events,
        }
    }

    /// Applies what the port answers until `done` holds, failing after ten seconds.
    fn until(&mut self, mut done: impl FnMut(&mut Self, Option<ProjectId>) -> bool) {
        for _ in 0..1000 {
            match self.events.try_recv() {
                Ok(event) => {
                    let opened = self.app.apply_files_event(event).unwrap();
                    if done(self, opened) {
                        return;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the files port never answered what the test waits for");
    }

    fn open(&mut self, root: &Path) -> ProjectId {
        self.app.open_project(root);
        let mut project = None;
        self.until(|_, opened| {
            project = project.take().or(opened);
            project.is_some()
        });
        project.unwrap()
    }

    /// An attached orchestrator of `project`.
    fn orchestrator(&mut self, project: &ProjectId) -> SessionId {
        let session = self
            .app
            .new_session(project.clone(), Scope::Project)
            .unwrap();
        let calls = self.backend.calls();
        let (request, _, _) = calls.last().unwrap().as_new_session().unwrap();
        self.app
            .apply_event(BackendEvent::SessionCreated {
                request,
                session: session.clone(),
                backend_session: BackendSessionId::new("b-1"),
                config: Vec::new(),
            })
            .unwrap();
        session
    }

    /// Sends `session` the request `id` to edit `path`, and applies what the port answers
    /// until it is decided: allowed, cancelled, or waiting for the user.
    fn edit(&mut self, session: &SessionId, id: u64, path: PathBuf) -> (Status, Option<Call>) {
        let permission = PermissionId(id);
        self.app
            .apply_event(BackendEvent::Update {
                session: session.clone(),
                update: AgentUpdate::PermissionRequested {
                    id: permission,
                    tool_call_id: ToolCallId::new(format!("t-{id}")),
                    title: "Write it".into(),
                    kind: ToolKind::Edit,
                    options: vec![PermissionOption {
                        id: PermissionOptionId::new("allow"),
                        name: "Allow".into(),
                        kind: PermissionKind::AllowOnce,
                    }],
                    name: None,
                    locations: vec![path],
                    partitur_tool: false,
                },
            })
            .unwrap();
        let answered = |on_disk: &Self| {
            on_disk.backend.calls().into_iter().find(|call| {
                matches!(call, Call::RespondPermission { permission: answered, .. }
                    if *answered == permission)
            })
        };
        self.until(|on_disk, _| {
            answered(on_disk).is_some() || on_disk.status(session) == Status::WaitingForYou
        });
        (self.status(session), answered(self))
    }

    /// The status the tree shows for the top-level `session`.
    fn status(&self, session: &SessionId) -> Status {
        self.app
            .tree()
            .projects
            .iter()
            .flat_map(|project| &project.sessions)
            .find(|node| node.id == *session)
            .unwrap()
            .status
    }
}

// Windows symlinks need developer mode, which a runner or user may not have.
#[cfg(unix)]
#[test]
fn an_orchestrators_edit_through_a_link_out_of_its_workflow_folder_is_denied() {
    let (data, project, outside) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let root = partitur_core::canonical(project.path()).unwrap();
    for repository in ["api", "web"] {
        fs::create_dir_all(root.join(repository).join(".git")).unwrap();
    }
    fs::create_dir_all(root.join("docs")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("docs/superpowers")).unwrap();
    fs::create_dir_all(root.join("_bmad")).unwrap();
    let mut on_disk = OnDisk::new(data.path());
    let project = on_disk.open(&root);
    let session = on_disk.orchestrator(&project);
    // BMAD's configuration as it is when the requests are made, after the agent started.
    fs::write(
        root.join("_bmad/config.toml"),
        "[core]\noutput_folder = \"out\"\n",
    )
    .unwrap();

    let escape = on_disk.edit(&session, 1, root.join("docs/superpowers/plan.md"));
    let configured = on_disk.edit(&session, 2, root.join("out/prd.md"));

    assert_eq!(
        escape.1,
        Some(Call::RespondPermission {
            session: session.clone(),
            permission: PermissionId(1),
            option: None,
        })
    );
    assert_eq!(configured, (Status::WaitingForYou, None));
}
