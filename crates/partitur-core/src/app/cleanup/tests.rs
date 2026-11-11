#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::app::{App, AppError, Cleanup};
use crate::git::{GitAnswer, GitEvent, Head, LocalBranch, RepoInfo, WorktreeInfo};
use crate::ids::{BackendId, BackendSessionId, ProjectId, RepositoryId, SessionId};
use crate::project::{Project, discover_project};
use crate::session::{Scope, Session, Status, Workspace};
use crate::settings::GlobalSettings;
use crate::store::Store as _;
use crate::testing::{
    Call, GitCall, GitOp, MemoryGit, MemoryStore, RecordingBackend, repo_info, settle,
    single_claude,
};
use crate::view_model::{BranchCleanup, CleanupView, WorktreeCleanup};

struct Fixture {
    app: App,
    backend: RecordingBackend,
    git: MemoryGit,
    project: ProjectId,
    /// The project folder, kept so it outlives the test.
    _dir: TempDir,
    api: PathBuf,
    web: PathBuf,
}

/// A project with the repositories `api` and `web` whose store holds the sessions `make`
/// returns for it; git knows both repositories with `main` checked out.
fn fixture(make: impl FnOnce(&Project) -> Vec<Session>) -> Fixture {
    let dir = TempDir::new().unwrap();
    for name in ["api", "web"] {
        fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
    }
    let project = discover_project(dir.path()).unwrap();
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id.clone(), &make(&project))
        .unwrap();
    let (backends, default) = single_claude();
    let backend = RecordingBackend::new();
    let git = MemoryGit::new();
    let app = App::load(
        Box::new(backend.clone()),
        Box::new(store),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();
    let (api, web) = (dir.path().join("api"), dir.path().join("web"));
    for repository in [&api, &web] {
        git.add_repository(
            repository.clone(),
            repo_info(repository, "main", &["main"], &[]),
        );
    }
    Fixture {
        app,
        backend,
        git,
        project: project.id,
        _dir: dir,
        api,
        web,
    }
}

/// A stored session of `project` titled `title` in `repository`, working in `workspace`.
fn session(project: &Project, title: &str, repository: &str, workspace: Workspace) -> Session {
    let scope = Scope::Repository(RepositoryId::try_from(repository).unwrap());
    Session {
        id: SessionId::new(),
        project: project.id.clone(),
        scope,
        cwd: project.root.join(repository),
        title: title.into(),
        backend_session: Some(BackendSessionId::new(format!("backend-{title}"))),
        backend: BackendId::try_from("claude").unwrap(),
        model: None,
        effort: None,
        status: Status::Idle,
        parent: None,
        auto_allow: false,
        instructions_injected: false,
        created_at: 1,
        orchestrator: false,
        archived: None,
        workspace,
        child_defaults: None,
    }
}

/// An orchestrator of `project` with no branch of its own.
fn orchestrator(project: &Project) -> Session {
    Session {
        scope: Scope::Project,
        cwd: project.root.clone(),
        orchestrator: true,
        ..session(project, "orchestrator", "api", Workspace::CheckedOut)
    }
}

fn worktree(branch: &str, base: &str, path: &Path) -> Workspace {
    Workspace::Worktree {
        branch: branch.into(),
        base: base.into(),
        fetched: false,
        path: path.to_path_buf(),
    }
}

fn new_branch(branch: &str, base: &str) -> Workspace {
    Workspace::NewBranch {
        branch: branch.into(),
        base: base.into(),
        fetched: false,
    }
}

/// Changes the repository at `path` in git's model with `change`.
fn change(git: &MemoryGit, path: &Path, change: impl FnOnce(&mut RepoInfo)) {
    let mut info = git.repository(path).unwrap();
    change(&mut info);
    git.add_repository(path.to_path_buf(), info);
}

/// Gives the repository at `path` the branch `branch`, checked out in a worktree at
/// `worktree` when there is one.
fn add_branch(git: &MemoryGit, path: &Path, branch: &str, worktree: Option<&Path>) {
    change(git, path, |info| {
        info.branches.push(LocalBranch {
            name: branch.into(),
            upstream_remote: None,
        });
        if let Some(worktree) = worktree {
            info.worktrees.push(WorktreeInfo {
                path: worktree.to_path_buf(),
                head: Head::Branch(branch.into()),
                prunable: false,
            });
        }
    });
}

/// The steps git was asked to run, in order.
fn steps(git: &MemoryGit) -> Vec<GitCall> {
    git.calls()
        .into_iter()
        .filter(|call| {
            matches!(
                call,
                GitCall::RemoveWorktree { .. } | GitCall::DeleteBranch { .. }
            )
        })
        .collect()
}

fn remove_worktree(repository: &Path, path: &Path) -> GitCall {
    GitCall::RemoveWorktree {
        repository: repository.to_path_buf(),
        path: path.to_path_buf(),
    }
}

fn delete_branch(repository: &Path, branch: &str) -> GitCall {
    GitCall::DeleteBranch {
        repository: repository.to_path_buf(),
        branch: branch.into(),
    }
}

fn both(session: &SessionId) -> Cleanup {
    Cleanup {
        session: session.clone(),
        remove_worktree: true,
        delete_branch: true,
    }
}

fn cleaning(app: &App) -> bool {
    app.tree().projects[0].cleaning
}

/// A fixture with one Worktree session `feat/x` from `main` in `api`, its worktree at
/// `/wt/feat-x`; the sheet's reading has answered with `changes` and `merged`.
fn read_worktree_session(changes: usize, merged: bool) -> (Fixture, SessionId, PathBuf) {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", Some(&wt));
    fixture.git.set_changes(&wt, changes);
    fixture.git.set_merged(&fixture.api, "feat/x", merged);
    fixture.app.delete_summary(&id).unwrap();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    (fixture, id, wt)
}

#[test]
fn the_delete_summary_reads_in_the_background() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "dev", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", Some(&wt));
    fixture.git.set_changes(&wt, 3);
    fixture.git.set_merged(&fixture.api, "feat/x", false);

    let summary = fixture.app.delete_summary(&id).unwrap();

    let loading = CleanupView {
        session: id.clone(),
        title: "login".into(),
        repository: "api".into(),
        loading: true,
        busy: false,
        worktree: None,
        branch: None,
    };
    assert_eq!(summary.cleanup, std::slice::from_ref(&loading));
    assert_eq!(fixture.app.cleanup(&id), std::slice::from_ref(&loading));
    assert_eq!(fixture.git.waiting(), 1);
    assert_eq!(
        fixture.git.calls(),
        [
            GitCall::Inspect(fixture.api.clone()),
            GitCall::Changes(wt.clone()),
            GitCall::IsMerged {
                repository: fixture.api.clone(),
                branch: "feat/x".into(),
                base: "dev".into(),
            },
        ]
    );
    assert!(!cleaning(&fixture.app));

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert_eq!(
        fixture.app.cleanup(&id),
        [CleanupView {
            loading: false,
            worktree: Some(WorktreeCleanup {
                path: wt.clone(),
                changes: Some(3),
            }),
            branch: Some(BranchCleanup {
                name: "feat/x".into(),
                merged: false,
                checked_out: None,
                in_own_worktree: true,
            }),
            ..loading
        }]
    );
    assert!(steps(&fixture.git).is_empty());
}

#[test]
fn a_new_branch_session_offers_a_branch_that_cannot_go_while_checked_out() {
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", new_branch("feat/x", "main"));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    let api = fixture.api.clone();
    fixture.git.add_repository(
        api.clone(),
        repo_info(&api, "feat/x", &["main", "feat/x"], &[]),
    );
    fixture.git.set_merged(&api, "feat/x", true);

    fixture.app.delete_summary(&id).unwrap();
    settle(&mut fixture.app, &fixture.git);

    assert_eq!(
        fixture.git.calls(),
        [
            GitCall::Inspect(api.clone()),
            GitCall::IsMerged {
                repository: api.clone(),
                branch: "feat/x".into(),
                base: "main".into(),
            },
        ]
    );
    let line = &fixture.app.cleanup(&id)[0];
    assert_eq!(line.worktree, None);
    assert_eq!(
        line.branch,
        Some(BranchCleanup {
            name: "feat/x".into(),
            merged: true,
            checked_out: Some(api.clone()),
            in_own_worktree: false,
        })
    );

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();

    assert_eq!(fixture.git.waiting(), 0, "nothing to remove is read again");
    assert!(steps(&fixture.git).is_empty());
}

#[test]
fn a_checked_out_session_and_a_removed_branch_offer_nothing() {
    let wt = PathBuf::from("/wt/gone");
    let (mut plain, mut removed) = (None, None);
    let mut fixture = fixture(|project| {
        let checked_out = session(project, "plain", "api", Workspace::CheckedOut);
        let gone = session(project, "gone", "web", worktree("feat/gone", "main", &wt));
        plain = Some(checked_out.id.clone());
        removed = Some(gone.id.clone());
        vec![checked_out, gone]
    });
    let (plain, removed) = (plain.unwrap(), removed.unwrap());

    let summary = fixture.app.delete_summary(&plain).unwrap();

    assert!(summary.cleanup.is_empty());
    assert!(fixture.app.cleanup(&plain).is_empty());
    assert!(fixture.git.calls().is_empty());

    fixture.app.delete_summary(&removed).unwrap();
    settle(&mut fixture.app, &fixture.git);

    let line = &fixture.app.cleanup(&removed)[0];
    assert!(!line.loading);
    assert_eq!((&line.worktree, &line.branch), (&None, &None));
}

#[test]
fn a_failed_reading_offers_what_it_can() {
    let (api_wt, web_wt) = (PathBuf::from("/wt/api"), PathBuf::from("/wt/web"));
    let (mut in_api, mut in_web) = (None, None);
    let mut fixture = fixture(|project| {
        let api = session(project, "api", "api", worktree("feat/a", "main", &api_wt));
        let web = session(project, "web", "web", worktree("feat/w", "main", &web_wt));
        in_api = Some(api.id.clone());
        in_web = Some(web.id.clone());
        vec![api, web]
    });
    let (in_api, in_web) = (in_api.unwrap(), in_web.unwrap());
    add_branch(&fixture.git, &fixture.api, "feat/a", Some(&api_wt));
    add_branch(&fixture.git, &fixture.web, "feat/w", Some(&web_wt));
    fixture.git.set_merged(&fixture.api, "feat/a", true);
    fixture.git.fail_next(GitOp::Changes, &api_wt);
    fixture.git.fail_next(GitOp::IsMerged, &fixture.api);
    fixture.git.fail_next(GitOp::Inspect, &fixture.web);

    fixture.app.delete_summary(&in_api).unwrap();
    fixture.app.delete_summary(&in_web).unwrap();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    let api = &fixture.app.cleanup(&in_api)[0];
    assert_eq!(api.worktree.as_ref().unwrap().changes, None);
    assert!(!api.branch.as_ref().unwrap().merged);
    let web = &fixture.app.cleanup(&in_web)[0];
    assert!(!web.loading);
    assert_eq!((&web.worktree, &web.branch), (&None, &None));
}

#[test]
fn a_summary_git_cannot_queue_offers_nothing() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    fixture.git.refuse_next_request();

    let summary = fixture.app.delete_summary(&id).unwrap();

    assert!(!summary.cleanup[0].loading);
    assert_eq!(summary.cleanup[0].worktree, None);
    assert_eq!(summary.cleanup[0].branch, None);
    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    assert_eq!(fixture.git.waiting(), 0);
}

#[test]
fn an_orchestrator_lists_one_line_per_child() {
    let (wt_a, wt_c) = (PathBuf::from("/wt/a"), PathBuf::from("/wt/c"));
    let mut top = None;
    let mut children = Vec::new();
    let mut fixture = fixture(|project| {
        let parent = orchestrator(project);
        let child = |title: &str, repository: &str, workspace: Workspace| Session {
            parent: Some(parent.id.clone()),
            ..session(project, title, repository, workspace)
        };
        let a = child("a", "api", worktree("feat/a", "main", &wt_a));
        let b = child("b", "web", new_branch("feat/b", "main"));
        let plain = child("plain", "api", Workspace::CheckedOut);
        let c = child("c", "api", worktree("feat/c", "main", &wt_c));
        top = Some(parent.id.clone());
        children = vec![a.id.clone(), b.id.clone(), c.id.clone()];
        vec![parent, a, b, plain, c]
    });
    let top = top.unwrap();

    let summary = fixture.app.delete_summary(&top).unwrap();

    let lines: Vec<(SessionId, String, String)> = summary
        .cleanup
        .iter()
        .map(|line| {
            (
                line.session.clone(),
                line.title.clone(),
                line.repository.clone(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            (children[0].clone(), "a".into(), "api".into()),
            (children[1].clone(), "b".into(), "web".into()),
            (children[2].clone(), "c".into(), "api".into()),
        ]
    );
    assert!(summary.cleanup.iter().all(|line| line.loading));
    assert_eq!(fixture.git.waiting(), 1, "one request reads every line");
    settle(&mut fixture.app, &fixture.git);
    assert!(fixture.app.cleanup(&top).iter().all(|line| !line.loading));
    assert!(fixture.app.cleanup(&children[0]).is_empty());
}

#[test]
fn a_project_session_in_a_repository_root_has_a_line() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let project = discover_project(dir.path()).unwrap();
    let made = Session {
        scope: Scope::Project,
        ..session(&project, "root", "api", new_branch("feat/r", "main"))
    };
    let store = MemoryStore::new();
    store.save_projects(std::slice::from_ref(&project)).unwrap();
    store
        .save_sessions(project.id.clone(), std::slice::from_ref(&made))
        .unwrap();
    let (backends, default) = single_claude();
    let git = MemoryGit::new();
    let mut app = App::load(
        Box::new(RecordingBackend::new()),
        Box::new(store),
        Box::new(git.clone()),
        backends,
        default,
        GlobalSettings::default(),
    )
    .unwrap();

    let summary = app.delete_summary(&made.id).unwrap();

    let root = project.repositories[0].path.clone();
    assert_eq!(summary.cleanup[0].repository, project.repositories[0].name);
    assert_eq!(git.calls()[0], GitCall::Inspect(root));
}

#[test]
fn a_worktree_under_a_symlinked_folder_is_still_found() {
    let folders = TempDir::new().unwrap();
    let real = folders.path().join("real");
    fs::create_dir_all(real.join("wt")).unwrap();
    let symlink = folders.path().join("link");
    std::os::unix::fs::symlink(&real, &symlink).unwrap();
    let recorded = symlink.join("wt");
    let listed = fs::canonicalize(real.join("wt")).unwrap();
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(
            project,
            "login",
            "api",
            worktree("feat/x", "main", &recorded),
        );
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", Some(&listed));

    fixture.app.delete_summary(&id).unwrap();
    settle(&mut fixture.app, &fixture.git);

    let line = &fixture.app.cleanup(&id)[0];
    assert_eq!(line.worktree.as_ref().unwrap().path, listed);
    assert!(line.branch.as_ref().unwrap().in_own_worktree);
    assert_eq!(line.branch.as_ref().unwrap().checked_out, None);

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert_eq!(
        steps(&fixture.git),
        [
            remove_worktree(&fixture.api, &listed),
            delete_branch(&fixture.api, "feat/x"),
        ]
    );
}

#[test]
fn a_prunable_or_main_worktree_is_not_offered() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut ids = Vec::new();
    let mut fixture = fixture(|project| {
        let pruned = session(project, "pruned", "api", worktree("feat/x", "main", &wt));
        let web = project.root.join("web");
        let main = session(project, "main", "web", worktree("feat/m", "main", &web));
        ids = vec![pruned.id.clone(), main.id.clone()];
        vec![pruned, main]
    });
    add_branch(&fixture.git, &fixture.api, "feat/x", None);
    change(&fixture.git, &fixture.api, |info| {
        info.worktrees.push(WorktreeInfo {
            path: wt.clone(),
            head: Head::Branch("feat/x".into()),
            prunable: true,
        });
    });
    let web = fixture.web.clone();
    add_branch(&fixture.git, &web, "feat/m", None);
    change(&fixture.git, &web, |info| {
        info.worktrees[0].head = Head::Branch("feat/m".into());
    });

    for id in &ids {
        fixture.app.delete_summary(id).unwrap();
    }
    settle(&mut fixture.app, &fixture.git);

    let pruned = &fixture.app.cleanup(&ids[0])[0];
    assert_eq!(pruned.worktree, None);
    assert_eq!(pruned.branch.as_ref().unwrap().checked_out, None);
    let main = &fixture.app.cleanup(&ids[1])[0];
    assert_eq!(main.worktree, None);
    assert_eq!(main.branch.as_ref().unwrap().checked_out, Some(web));
    assert!(!main.branch.as_ref().unwrap().in_own_worktree);
}

#[test]
fn an_answer_for_an_older_summary_is_ignored() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", Some(&wt));
    fixture.git.set_changes(&wt, 1);
    fixture.app.delete_summary(&id).unwrap();
    let older = fixture.git.answer();
    fixture.git.set_changes(&wt, 5);

    fixture.app.delete_summary(&id).unwrap();
    let newer = fixture.git.answer();
    for event in newer.into_iter().chain(older) {
        fixture.app.apply_git_event(event).unwrap();
    }

    let changes = fixture.app.cleanup(&id)[0]
        .worktree
        .as_ref()
        .unwrap()
        .changes;
    assert_eq!(changes, Some(5));
}

#[test]
fn deleting_queues_only_what_was_checked_and_by_force() {
    let (mut fixture, id, wt) = read_worktree_session(3, false);
    let checked = Cleanup {
        session: id.clone(),
        remove_worktree: true,
        delete_branch: false,
    };

    fixture.app.delete(id.clone(), &[checked]).unwrap();

    assert!(fixture.app.tree().projects[0].sessions.is_empty());
    assert!(fixture.app.cleanup(&id).is_empty());
    assert!(cleaning(&fixture.app));
    assert!(fixture.app.git_work.is_busy(&fixture.api));
    assert_eq!(
        fixture.git.calls()[3..],
        [
            GitCall::Inspect(fixture.api.clone()),
            GitCall::Changes(wt.clone()),
        ]
    );
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert!(!cleaning(&fixture.app));
    assert!(!fixture.app.git_work.is_busy(&fixture.api));
    assert_eq!(steps(&fixture.git), [remove_worktree(&fixture.api, &wt)]);
    let info = fixture.git.repository(&fixture.api).unwrap();
    assert_eq!(info.worktrees.len(), 1);
    assert!(info.has_branch("feat/x"));

    let (mut fixture, id, wt) = read_worktree_session(3, false);
    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert_eq!(
        steps(&fixture.git),
        [
            remove_worktree(&fixture.api, &wt),
            delete_branch(&fixture.api, "feat/x"),
        ]
    );
    assert!(
        !fixture
            .git
            .repository(&fixture.api)
            .unwrap()
            .has_branch("feat/x")
    );
}

#[test]
fn a_branch_alone_can_go_once_its_worktree_is_gone() {
    let (mut fixture, id, wt) = read_worktree_session(0, true);
    change(&fixture.git, &fixture.api, |info| {
        info.worktrees.truncate(1);
    });
    fixture.app.delete_summary(&id).unwrap();
    settle(&mut fixture.app, &fixture.git);
    let only_branch = Cleanup {
        session: id.clone(),
        remove_worktree: false,
        delete_branch: true,
    };

    fixture.app.delete(id.clone(), &[only_branch]).unwrap();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert_eq!(steps(&fixture.git), [delete_branch(&fixture.api, "feat/x")]);
    assert!(!steps(&fixture.git).contains(&remove_worktree(&fixture.api, &wt)));
}

#[test]
fn a_branch_in_its_own_worktree_goes_only_with_the_worktree() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    let only_branch = Cleanup {
        session: id.clone(),
        remove_worktree: false,
        delete_branch: true,
    };

    fixture.app.delete(id.clone(), &[only_branch]).unwrap();

    assert_eq!(fixture.git.waiting(), 0);
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert!(steps(&fixture.git).is_empty());
}

#[test]
fn a_failed_removal_is_reported_after_the_session_is_gone() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    fixture.git.fail_next(GitOp::RemoveWorktree, &fixture.api);

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();

    assert!(fixture.backend.calls().contains(&Call::CloseSession {
        session: id.clone()
    }));
    let errors = settle(&mut fixture.app, &fixture.git);
    assert!(
        matches!(
            errors.as_slice(),
            [AppError::Cleanup { repository, .. }] if repository == "api"
        ),
        "{errors:?}"
    );
    assert_eq!(
        errors[0].to_string(),
        "could not remove the worktree or branch in api: git worktree remove failed: simulated failure"
    );
    assert!(!cleaning(&fixture.app));
    assert!(
        fixture
            .git
            .repository(&fixture.api)
            .unwrap()
            .has_branch("feat/x")
    );
}

#[test]
fn a_removal_git_cannot_queue_is_reported() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    fixture.git.refuse_next_request();

    let refused = fixture.app.delete(id.clone(), &[both(&id)]);

    assert!(matches!(refused, Err(AppError::Cleanup { repository, .. }) if repository == "api"));
    assert!(fixture.app.tree().projects[0].sessions.is_empty());
    assert!(!cleaning(&fixture.app));

    let (mut fixture, id, _) = read_worktree_session(0, true);
    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    fixture.git.refuse_next_request();
    let errors = settle(&mut fixture.app, &fixture.git);
    assert!(
        matches!(errors.as_slice(), [AppError::Cleanup { repository, .. }] if repository == "api"),
        "{errors:?}"
    );
    assert!(steps(&fixture.git).is_empty());
    assert!(!cleaning(&fixture.app));
    assert!(!fixture.app.git_work.is_busy(&fixture.api));
}

/// The errors and steps of deleting the session of [`read_worktree_session`] (read clean
/// and merged) with both boxes checked, after `then` changed git's model.
fn delete_after(then: impl FnOnce(&Fixture, &Path)) -> (Vec<String>, Vec<GitCall>, Fixture) {
    let (mut fixture, id, wt) = read_worktree_session(0, true);
    then(&fixture, &wt);
    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    let errors = settle(&mut fixture.app, &fixture.git)
        .into_iter()
        .map(|error| match error {
            AppError::CleanupKept { repository, reason } => {
                assert_eq!(repository, "api");
                reason
            }
            other => panic!("{other:?}"),
        })
        .collect();
    let steps = steps(&fixture.git);
    (errors, steps, fixture)
}

#[test]
fn what_changed_after_the_sheet_read_it_is_kept() {
    let wt = PathBuf::from("/wt/feat-x");
    let checked_out_there = "branch feat/x is checked out at /wt/feat-x";

    let (kept, steps, _) = delete_after(|fixture, wt| fixture.git.set_changes(wt, 2));
    assert_eq!(
        kept,
        [format!(
            "worktree /wt/feat-x has uncommitted files; {checked_out_there}"
        )]
    );
    assert!(steps.is_empty());

    let (kept, steps, fixture) =
        delete_after(|fixture, _| fixture.git.set_merged(&fixture.api, "feat/x", false));
    assert_eq!(kept, ["branch feat/x has commits not merged into main"]);
    assert_eq!(steps, [remove_worktree(&fixture.api, &wt)]);

    let (kept, steps, fixture) = delete_after(|fixture, _| {
        change(&fixture.git, &fixture.api, |info| {
            info.worktrees[1].head = Head::Branch("main".into());
        });
    });
    assert_eq!(kept, ["worktree /wt/feat-x has something else checked out"]);
    assert_eq!(steps, [delete_branch(&fixture.api, "feat/x")]);

    let (kept, steps, fixture) = delete_after(|fixture, _| {
        change(&fixture.git, &fixture.api, |info| {
            info.worktrees.truncate(1);
        });
    });
    assert_eq!(kept, ["worktree /wt/feat-x is no longer there"]);
    assert_eq!(steps, [delete_branch(&fixture.api, "feat/x")]);

    let (kept, steps, _) = delete_after(|fixture, _| {
        change(&fixture.git, &fixture.api, |info| {
            info.worktrees[1].prunable = true;
        });
    });
    assert_eq!(
        kept,
        [format!(
            "worktree /wt/feat-x is no longer there; {checked_out_there}"
        )]
    );
    assert!(steps.is_empty());

    let (kept, steps, fixture) = delete_after(|fixture, _| {
        change(&fixture.git, &fixture.api, |info| {
            info.branches.truncate(1);
        });
    });
    assert_eq!(kept, ["branch feat/x is gone"]);
    assert_eq!(steps, [remove_worktree(&fixture.api, &wt)]);

    let (kept, steps, _) = delete_after(|fixture, wt| fixture.git.fail_next(GitOp::Changes, wt));
    assert_eq!(
        kept,
        [format!(
            "worktree /wt/feat-x has uncommitted files; {checked_out_there}"
        )]
    );
    assert!(steps.is_empty());

    let (kept, steps, fixture) =
        delete_after(|fixture, _| fixture.git.fail_next(GitOp::IsMerged, &fixture.api));
    assert_eq!(kept, ["branch feat/x has commits not merged into main"]);
    assert_eq!(steps, [remove_worktree(&fixture.api, &wt)]);

    let (kept, steps, _) =
        delete_after(|fixture, _| fixture.git.fail_next(GitOp::Inspect, &fixture.api));
    assert_eq!(kept, ["git worktree list failed: simulated failure"]);
    assert!(steps.is_empty());
}

#[test]
fn a_branch_checked_out_after_the_sheet_read_it_is_kept() {
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", new_branch("feat/x", "main"));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    let api = fixture.api.clone();
    add_branch(&fixture.git, &api, "feat/x", None);
    fixture.app.delete_summary(&id).unwrap();
    settle(&mut fixture.app, &fixture.git);
    assert_eq!(
        fixture.app.cleanup(&id)[0]
            .branch
            .as_ref()
            .unwrap()
            .checked_out,
        None
    );
    change(&fixture.git, &api, |info| {
        info.worktrees[0].head = Head::Branch("feat/x".into());
    });

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    let errors = settle(&mut fixture.app, &fixture.git);

    let reason = format!("branch feat/x is checked out at {}", api.display());
    assert!(
        matches!(errors.as_slice(), [AppError::CleanupKept { reason: kept, .. }] if *kept == reason),
        "{errors:?}"
    );
    assert_eq!(
        errors[0].to_string(),
        format!("kept the worktree or branch in api: {reason}")
    );
    assert!(steps(&fixture.git).is_empty());
}

#[test]
fn a_fresh_reading_git_does_not_describe_is_kept() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    let events: Vec<GitEvent> = fixture
        .git
        .answer()
        .into_iter()
        .map(|event| GitEvent::Answered {
            request: event.request(),
            answers: vec![Ok(GitAnswer::Merged(true))],
        })
        .collect();

    let errors: Vec<AppError> = events
        .into_iter()
        .filter_map(|event| fixture.app.apply_git_event(event).err())
        .collect();

    assert!(
        matches!(
            errors.as_slice(),
            [AppError::CleanupKept { reason, .. }] if reason == "git did not describe the repository"
        ),
        "{errors:?}"
    );
    assert_eq!(fixture.git.waiting(), 0);
    assert!(!cleaning(&fixture.app));
}

#[test]
fn two_children_in_one_repository_are_removed_by_one_request() {
    let (wt_a, wt_b, wt_c) = (
        PathBuf::from("/wt/a"),
        PathBuf::from("/wt/b"),
        PathBuf::from("/wt/c"),
    );
    let mut top = None;
    let mut children = Vec::new();
    let mut fixture = fixture(|project| {
        let parent = orchestrator(project);
        let child = |title: &str, repository: &str, workspace: Workspace| Session {
            parent: Some(parent.id.clone()),
            ..session(project, title, repository, workspace)
        };
        let a = child("a", "api", worktree("feat/a", "main", &wt_a));
        let c = child("c", "web", worktree("feat/c", "main", &wt_c));
        let b = child("b", "api", worktree("feat/b", "main", &wt_b));
        top = Some(parent.id.clone());
        children = vec![a.id.clone(), c.id.clone(), b.id.clone()];
        vec![parent, a, c, b]
    });
    let top = top.unwrap();
    let (api, web) = (fixture.api.clone(), fixture.web.clone());
    add_branch(&fixture.git, &api, "feat/a", Some(&wt_a));
    add_branch(&fixture.git, &web, "feat/c", Some(&wt_c));
    add_branch(&fixture.git, &api, "feat/b", Some(&wt_b));
    fixture.app.delete_summary(&top).unwrap();
    settle(&mut fixture.app, &fixture.git);
    let boxes: Vec<Cleanup> = children.iter().map(both).collect();

    fixture.app.delete(top, &boxes).unwrap();

    assert_eq!(fixture.git.waiting(), 2, "one fresh reading per repository");
    for event in fixture.git.answer() {
        fixture.app.apply_git_event(event).unwrap();
    }
    assert_eq!(fixture.git.waiting(), 2, "one removal per repository");
    assert_eq!(
        steps(&fixture.git),
        [
            remove_worktree(&api, &wt_a),
            delete_branch(&api, "feat/a"),
            remove_worktree(&api, &wt_b),
            delete_branch(&api, "feat/b"),
            remove_worktree(&web, &wt_c),
            delete_branch(&web, "feat/c"),
        ]
    );
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert_eq!(
        fixture.git.repository(&api).unwrap().branch_names(),
        ["main"]
    );
    assert_eq!(
        fixture.git.repository(&web).unwrap().branch_names(),
        ["main"]
    );
}

#[test]
fn a_busy_repository_offers_no_boxes_and_removes_nothing() {
    let (first_wt, second_wt) = (PathBuf::from("/wt/one"), PathBuf::from("/wt/two"));
    let (mut first, mut second) = (None, None);
    let mut fixture = fixture(|project| {
        let one = session(
            project,
            "one",
            "api",
            worktree("feat/one", "main", &first_wt),
        );
        let two = session(
            project,
            "two",
            "api",
            worktree("feat/two", "main", &second_wt),
        );
        first = Some(one.id.clone());
        second = Some(two.id.clone());
        vec![one, two]
    });
    let (first, second) = (first.unwrap(), second.unwrap());
    let api = fixture.api.clone();
    add_branch(&fixture.git, &api, "feat/one", Some(&first_wt));
    add_branch(&fixture.git, &api, "feat/two", Some(&second_wt));
    fixture.app.delete_summary(&first).unwrap();
    fixture.app.delete_summary(&second).unwrap();
    settle(&mut fixture.app, &fixture.git);
    assert!(!fixture.app.cleanup(&second)[0].busy);

    fixture.app.delete(first.clone(), &[both(&first)]).unwrap();

    assert!(fixture.app.cleanup(&second)[0].busy);
    fixture
        .app
        .delete(second.clone(), &[both(&second)])
        .unwrap();
    assert_eq!(fixture.git.waiting(), 1, "only the first removal is read");
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    let info = fixture.git.repository(&api).unwrap();
    assert!(info.has_branch("feat/two"));
    assert!(!info.has_branch("feat/one"));
    assert_eq!(info.checked_out_at("feat/two"), Some(second_wt.as_path()));
}

#[test]
fn deleting_while_the_summary_loads_removes_nothing() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", Some(&wt));
    fixture.app.delete_summary(&id).unwrap();

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();

    assert_eq!(fixture.git.waiting(), 1, "only the sheet's reading");
    assert!(!cleaning(&fixture.app));
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());
    assert!(steps(&fixture.git).is_empty());
}

#[test]
fn deleting_without_cleanup_touches_no_git() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    let calls = fixture.git.calls().len();
    let unchecked = Cleanup {
        session: id.clone(),
        remove_worktree: false,
        delete_branch: false,
    };
    let other = both(&SessionId::new());

    fixture.app.delete(id.clone(), &[unchecked, other]).unwrap();

    assert_eq!(fixture.git.calls().len(), calls);
    assert_eq!(fixture.git.waiting(), 0);

    let (mut fixture, id, _) = read_worktree_session(0, true);
    fixture.app.delete(id, &[]).unwrap();
    assert_eq!(fixture.git.waiting(), 0);
    assert!(!cleaning(&fixture.app));
}

#[test]
fn deleting_without_a_summary_touches_no_git() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();

    assert!(fixture.git.calls().is_empty());
}

#[test]
fn a_reading_for_a_deleted_sheet_is_ignored() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    fixture.app.delete_summary(&id).unwrap();
    fixture.app.delete(id.clone(), &[]).unwrap();

    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert!(fixture.app.cleanup(&id).is_empty());
}

#[test]
fn the_archive_summary_offers_no_cleanup() {
    let (fixture, id, _) = read_worktree_session(0, true);

    let summary = fixture.app.archive_summary(&id).unwrap();

    assert!(summary.cleanup.is_empty());
    assert_eq!(fixture.git.waiting(), 0);
    assert_eq!(fixture.app.cleanup(&id).len(), 1);
}

#[test]
fn cleaning_is_per_project() {
    let (mut fixture, id, _) = read_worktree_session(0, true);
    let other = TempDir::new().unwrap();
    let other_project = fixture.app.open_project(other.path()).unwrap();

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();

    let tree = fixture.app.tree();
    let cleaning = |project: &ProjectId| {
        tree.projects
            .iter()
            .find(|node| node.id == *project)
            .unwrap()
            .cleaning
    };
    assert!(cleaning(&fixture.project));
    assert!(!cleaning(&other_project));
    for event in fixture.git.answer() {
        fixture.app.apply_git_event(event).unwrap();
    }
    assert!(fixture.app.tree().projects[0].cleaning, "the removal runs");
    settle(&mut fixture.app, &fixture.git);
    assert!(!fixture.app.tree().projects[0].cleaning);
}

#[test]
fn an_answer_of_another_kind_tells_nothing() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", Some(&wt));
    fixture.app.delete_summary(&id).unwrap();
    let info = fixture.git.repository(&fixture.api).unwrap();

    for event in fixture.git.answer() {
        let swapped = GitEvent::Answered {
            request: event.request(),
            answers: vec![
                Ok(GitAnswer::Repo(info.clone())),
                Ok(GitAnswer::Merged(true)),
                Ok(GitAnswer::Changes(0)),
            ],
        };
        fixture.app.apply_git_event(swapped).unwrap();
    }

    let line = &fixture.app.cleanup(&id)[0];
    assert_eq!(line.worktree.as_ref().unwrap().changes, None);
    assert!(!line.branch.as_ref().unwrap().merged);
}

#[test]
fn a_worktree_with_another_branch_when_read_is_still_removed() {
    let wt = PathBuf::from("/wt/feat-x");
    let mut id = None;
    let mut fixture = fixture(|project| {
        let made = session(project, "login", "api", worktree("feat/x", "main", &wt));
        id = Some(made.id.clone());
        vec![made]
    });
    let id = id.unwrap();
    add_branch(&fixture.git, &fixture.api, "feat/x", None);
    add_branch(&fixture.git, &fixture.api, "other", Some(&wt));
    fixture.app.delete_summary(&id).unwrap();
    settle(&mut fixture.app, &fixture.git);
    let line = &fixture.app.cleanup(&id)[0];
    assert_eq!(line.worktree.as_ref().unwrap().path, wt);
    assert!(!line.branch.as_ref().unwrap().in_own_worktree);

    fixture.app.delete(id.clone(), &[both(&id)]).unwrap();
    assert!(settle(&mut fixture.app, &fixture.git).is_empty());

    assert_eq!(
        steps(&fixture.git),
        [
            remove_worktree(&fixture.api, &wt),
            delete_branch(&fixture.api, "feat/x"),
        ]
    );
}
