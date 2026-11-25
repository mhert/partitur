#![allow(clippy::unwrap_used)]

use super::*;
use crate::archive::Archived;
use crate::git::{LocalBranch, WorktreeInfo};
use crate::ids::ProjectId;
use crate::session::{BranchKind, Folder};
use crate::view_model::ExistingOption;

fn api() -> Repository {
    Repository {
        id: RepositoryId::try_from("api").unwrap(),
        path: PathBuf::from("/p/api"),
        name: "api".into(),
        instruction_files: Vec::new(),
        is_root: false,
    }
}

fn web() -> Repository {
    Repository {
        id: RepositoryId::try_from("web").unwrap(),
        path: PathBuf::from("/p/web"),
        name: "web".into(),
        ..api()
    }
}

fn project() -> Project {
    Project {
        id: ProjectId::new(),
        root: PathBuf::from("/p"),
        name: "p".into(),
        repositories: vec![api(), web()],
        instruction_files: Vec::new(),
        closed: false,
        fold: crate::Fold::Open,
    }
}

fn on(path: &str, head: Head, prunable: bool) -> WorktreeInfo {
    WorktreeInfo {
        path: PathBuf::from(path),
        head,
        prunable,
    }
}

fn branch_on(path: &str, branch: &str) -> WorktreeInfo {
    on(path, Head::Branch(branch.into()), false)
}

/// `api` on `main`, with `feat/x` and `a/z` in worktrees of their own, `gone` only in a
/// prunable one, a detached worktree, and the branches `b`, `x` and `main`.
fn info() -> RepoInfo {
    RepoInfo {
        worktrees: vec![
            branch_on("/p/api", "main"),
            branch_on("/wt/feat-x", "feat/x"),
            on("/p/api-wt/gone", Head::Branch("gone".into()), true),
            on("/wt/detached", Head::Detached("abc1234".into()), false),
            branch_on("/wt/a-z", "a/z"),
        ],
        branches: ["x", "main", "gone", "feat/x", "b", "a/z"]
            .into_iter()
            .map(|name| LocalBranch {
                name: name.into(),
                upstream_remote: None,
            })
            .collect(),
        remote_branches: Vec::new(),
    }
}

fn choice(mode: BranchMode, existing: Option<&str>) -> BranchChoice {
    BranchChoice {
        mode,
        existing: existing.map(str::to_owned),
        ..BranchChoice::default()
    }
}

fn branching_on(choice: BranchChoice, state: RepoState) -> Branching {
    let target = Target::Repository {
        repository: api(),
        state,
    };
    Branching::new(project(), target, choice, Ok(ProjectSettings::default()))
}

fn branching(choice: BranchChoice) -> Branching {
    let mut branching = branching_on(choice, RepoState::Ready(info()));
    branching.choice.base = Some("main".into());
    branching
}

/// What the rules read: the sessions, the busy repositories and the folders that exist.
#[derive(Default)]
struct World {
    sessions: Vec<Session>,
    busy: bool,
    exists: Vec<PathBuf>,
}

impl World {
    fn refresh(&self, branching: &mut Branching) -> BranchView {
        let global = GlobalSettings::default();
        let busy = |_: &Path| self.busy;
        let exists = |folder: &Path| self.exists.iter().any(|known| known == folder);
        branching.refresh(&Context {
            sessions: &self.sessions,
            global: &global,
            busy: &busy,
            exists: &exists,
        });
        branching.view().clone()
    }

    fn working_in(mut self, folder: &str, title: &str) -> Self {
        let mut session = crate::app::test_support::stored_session(&project(), title, 1);
        session.cwd = PathBuf::from(folder);
        self.sessions.push(session);
        self
    }

    fn archived_in(mut self, folder: &str, title: &str) -> Self {
        self = self.working_in(folder, title);
        if let Some(session) = self.sessions.last_mut() {
            session.archived = Some(Archived { at: 2 });
        }
        self
    }
}

fn view_of(choice: BranchChoice) -> BranchView {
    World::default().refresh(&mut branching(choice))
}

fn blocker_of(choice: BranchChoice, world: &World) -> Option<String> {
    world.refresh(&mut branching(choice)).blocker
}

fn planned_of(choice: BranchChoice) -> Result<Option<Planned>, String> {
    let mut branching = branching(choice);
    World::default().refresh(&mut branching);
    branching.planned()
}

fn option(name: &str, worktree: Option<&str>, main: bool, gone: bool) -> ExistingOption {
    ExistingOption {
        name: name.into(),
        worktree: worktree.map(PathBuf::from),
        main,
        gone,
    }
}

#[test]
fn existing_options_list_worktrees_first() {
    let view = view_of(choice(BranchMode::ExistingWorktree, None));

    assert_eq!(
        view.existing_options,
        [
            option("a/z", Some("/wt/a-z"), false, false),
            option("feat/x", Some("/wt/feat-x"), false, false),
            option("b", None, false, false),
            option("gone", None, false, true),
            option("main", None, true, false),
            option("x", None, false, false),
        ]
    );
    assert_eq!(view.existing, None);
}

#[test]
fn nothing_existing_is_offered_while_loading() {
    let mut branching = branching_on(
        choice(BranchMode::ExistingBranch, Some("b")),
        RepoState::Loading(GitRequestId(1)),
    );

    let view = World::default().refresh(&mut branching);

    assert!(view.existing_options.is_empty());
    assert_eq!(view.existing.as_deref(), Some("b"));
    assert_eq!(view.note, None);
    assert_eq!(
        view.blocker.as_deref(),
        Some("reading the branches of api…")
    );
    assert_eq!(
        branching.set_existing("b".into()),
        Err("b is not a branch of api".into())
    );
}

#[test]
fn a_new_name_that_exists_offers_use_existing() {
    for mode in [BranchMode::Worktree, BranchMode::NewBranch] {
        let mut branching = branching(BranchChoice {
            branch: " x ".into(),
            ..choice(mode, None)
        });

        let view = World::default().refresh(&mut branching);

        assert_eq!(
            view.blocker.as_deref(),
            Some("branch x already exists in api")
        );
        assert_eq!(view.use_existing.as_deref(), Some("x"));
    }
    let fresh = BranchChoice {
        branch: "fresh".into(),
        ..choice(BranchMode::Worktree, None)
    };
    assert_eq!(view_of(fresh).use_existing, None);
    let as_is = BranchChoice {
        branch: "x".into(),
        ..choice(BranchMode::CheckedOut, None)
    };
    assert_eq!(view_of(as_is).use_existing, None);
    let existing = BranchChoice {
        branch: "x".into(),
        ..choice(BranchMode::ExistingWorktree, Some("b"))
    };
    assert_eq!(view_of(existing).use_existing, None);
}

#[test]
fn use_existing_switches_to_existing_in_the_same_folder() {
    for (mode, existing) in [
        (BranchMode::Worktree, BranchMode::ExistingWorktree),
        (BranchMode::NewBranch, BranchMode::ExistingBranch),
    ] {
        let mut branching = branching(BranchChoice {
            branch: " x ".into(),
            ..choice(mode, None)
        });
        World::default().refresh(&mut branching);

        branching.use_existing().unwrap();
        let view = World::default().refresh(&mut branching);

        assert_eq!(view.mode, existing);
        assert_eq!(view.existing.as_deref(), Some("x"));
        assert_eq!(view.use_existing, None);
    }
}

#[test]
fn use_existing_needs_an_existing_name() {
    let mut branching = branching(BranchChoice {
        branch: "fresh".into(),
        ..choice(BranchMode::Worktree, None)
    });
    World::default().refresh(&mut branching);

    assert_eq!(
        branching.use_existing(),
        Err("there is no existing branch to use".into())
    );
    assert_eq!(branching.choice().mode, BranchMode::Worktree);
    assert_eq!(branching.choice().existing, None);
}

#[test]
fn reusing_a_worktree_plans_no_git_work() {
    let picked = choice(BranchMode::ExistingWorktree, Some("feat/x"));
    let view = view_of(picked.clone());
    let planned = planned_of(picked).unwrap().unwrap();

    assert_eq!(view.note.as_deref(), Some("uses its existing worktree"));
    assert_eq!(view.worktree, Some(PathBuf::from("/wt/feat-x")));
    assert_eq!(view.blocker, None);
    assert_eq!(
        planned,
        Planned {
            repository: api(),
            branch: "feat/x".into(),
            make: Make::Reuse {
                path: PathBuf::from("/wt/feat-x")
            },
            before: Head::Branch("main".into()),
        }
    );
    assert!(create_steps(&planned).is_empty());
    assert_eq!(
        workspace_of(&planned),
        Workspace::ExistingWorktree {
            branch: "feat/x".into(),
            path: PathBuf::from("/wt/feat-x"),
        }
    );
}

#[test]
fn an_existing_branch_without_worktree_adds_one() {
    let picked = choice(BranchMode::ExistingWorktree, Some("b"));
    let view = view_of(picked.clone());
    let planned = planned_of(picked).unwrap().unwrap();
    let path = PathBuf::from("/p/api-wt/b");

    assert_eq!(view.note.as_deref(), Some("adds a worktree for it"));
    assert_eq!(view.worktree, Some(path.clone()));
    assert_eq!(planned.make, Make::AddWorktree { path: path.clone() });
    assert_eq!(planned.branch, "b");
    assert_eq!(
        create_steps(&planned),
        [GitStep::CheckoutWorktree {
            repository: PathBuf::from("/p/api"),
            path: path.clone(),
            branch: "b".into(),
        }]
    );
    assert_eq!(
        workspace_of(&planned),
        Workspace::WorktreeOnExisting {
            branch: "b".into(),
            path,
        }
    );
}

#[test]
fn switching_the_main_checkout_to_an_existing_branch() {
    let picked = choice(BranchMode::ExistingBranch, Some("b"));
    let view = view_of(picked.clone());
    let planned = planned_of(picked).unwrap().unwrap();

    assert_eq!(view.note.as_deref(), Some("switches api itself to b"));
    assert_eq!(view.worktree, None);
    assert_eq!(planned.make, Make::SwitchTo);
    assert_eq!(planned.branch, "b");
    assert_eq!(
        create_steps(&planned),
        [GitStep::RestoreHead {
            repository: PathBuf::from("/p/api"),
            head: Head::Branch("b".into()),
        }]
    );
    assert_eq!(
        workspace_of(&planned),
        Workspace::ExistingBranch { branch: "b".into() }
    );
}

#[test]
fn the_current_branch_as_existing_is_as_is() {
    let picked = choice(BranchMode::ExistingBranch, Some("main"));
    let world = World {
        busy: true,
        ..World::default()
    }
    .working_in("/p/api", "other");
    let mut branching = branching(picked.clone());

    let view = world.refresh(&mut branching);

    assert_eq!(view.blocker, None);
    assert_eq!(
        view.note.as_deref(),
        Some("works in api as it is now · also used by \"other\"")
    );
    assert_eq!(branching.planned(), Ok(None));
    assert_eq!(
        view_of(picked).note.as_deref(),
        Some("works in api as it is now")
    );
}

#[test]
fn a_detached_main_checkout_switches_to_any_branch() {
    let mut detached = info();
    if let Some(main) = detached.worktrees.first_mut() {
        main.head = Head::Detached("abc1234".into());
    }
    let mut branching = branching_on(
        choice(BranchMode::ExistingBranch, Some("main")),
        RepoState::Ready(detached),
    );

    let view = World::default().refresh(&mut branching);
    let planned = branching.planned().unwrap().unwrap();

    assert_eq!(view.note.as_deref(), Some("switches api itself to main"));
    assert_eq!(planned.make, Make::SwitchTo);
    assert_eq!(planned.before, Head::Detached("abc1234".into()));
}

#[test]
fn existing_asks_to_pick_a_branch() {
    for mode in [BranchMode::ExistingBranch, BranchMode::ExistingWorktree] {
        let world = World {
            busy: true,
            ..World::default()
        };
        let view = world.refresh(&mut branching(choice(mode, None)));

        assert_eq!(view.blocker.as_deref(), Some("pick a branch"));
        assert_eq!(view.note, None);
        assert_eq!(view.worktree, None);
        assert!(planned_of(choice(mode, None)).is_err());
    }
}

#[test]
fn existing_refuses_a_name_that_is_no_branch() {
    for mode in [BranchMode::ExistingBranch, BranchMode::ExistingWorktree] {
        let view = view_of(choice(mode, Some("nope")));

        assert_eq!(view.blocker.as_deref(), Some("nope is not a branch of api"));
        assert_eq!(view.note, None);
    }
}

#[test]
fn a_gone_worktree_asks_for_a_prune() {
    for mode in [BranchMode::ExistingBranch, BranchMode::ExistingWorktree] {
        let world = World::default().working_in("/p/api", "other");

        let view = world.refresh(&mut branching(choice(mode, Some("gone"))));

        assert_eq!(
            view.blocker.as_deref(),
            Some("its worktree at /p/api-wt/gone is gone; run git worktree prune")
        );
        assert_eq!(view.note, None);
        assert!(planned_of(choice(mode, Some("gone"))).is_err());
    }
}

#[test]
fn a_branch_in_another_worktree_cannot_be_switched_to() {
    let world = World {
        busy: true,
        ..World::default()
    }
    .working_in("/p/api", "other");

    assert_eq!(
        blocker_of(choice(BranchMode::ExistingBranch, Some("feat/x")), &world).as_deref(),
        Some("feat/x is checked out in /wt/feat-x — pick Worktree")
    );
}

#[test]
fn switching_a_checkout_in_use_is_refused() {
    let world = World {
        busy: true,
        ..World::default()
    }
    .archived_in("/p/api", "old")
    .working_in("/wt/feat-x", "elsewhere")
    .working_in("/p/api", "other");

    assert_eq!(
        blocker_of(choice(BranchMode::ExistingBranch, Some("b")), &world).as_deref(),
        Some("api is in use by \"other\" — use a worktree")
    );
    let archived = World::default().archived_in("/p/api", "old");
    assert_eq!(
        blocker_of(choice(BranchMode::ExistingBranch, Some("b")), &archived),
        None
    );
}

#[test]
fn the_main_checkouts_branch_cannot_go_to_a_worktree() {
    let world = World {
        busy: true,
        exists: vec![PathBuf::from("/p/api-wt/main")],
        ..World::default()
    };

    assert_eq!(
        blocker_of(choice(BranchMode::ExistingWorktree, Some("main")), &world).as_deref(),
        Some("main is checked out in api's main checkout — pick Main checkout")
    );
}

#[test]
fn a_worktree_for_an_existing_branch_needs_a_free_folder() {
    let world = World {
        busy: true,
        exists: vec![PathBuf::from("/p/api-wt/b")],
        ..World::default()
    };

    assert_eq!(
        blocker_of(choice(BranchMode::ExistingWorktree, Some("b")), &world).as_deref(),
        Some("/p/api-wt/b already exists")
    );
}

#[test]
fn existing_waits_for_a_busy_repository_only_with_git_work() {
    let busy = World {
        busy: true,
        ..World::default()
    };
    let busy_note = Some("api is busy with another branch change".to_owned());

    assert_eq!(
        blocker_of(choice(BranchMode::ExistingBranch, Some("b")), &busy),
        busy_note
    );
    assert_eq!(
        blocker_of(choice(BranchMode::ExistingWorktree, Some("b")), &busy),
        busy_note
    );
    assert_eq!(
        blocker_of(choice(BranchMode::ExistingWorktree, Some("feat/x")), &busy),
        None
    );
    let idle = World::default();
    assert_eq!(
        blocker_of(choice(BranchMode::ExistingBranch, Some("b")), &idle),
        None
    );
    assert_eq!(
        blocker_of(choice(BranchMode::ExistingWorktree, Some("b")), &idle),
        None
    );
}

#[test]
fn reuse_notes_other_sessions_in_the_worktree() {
    let picked = || choice(BranchMode::ExistingWorktree, Some("feat/x"));
    let one = World::default()
        .working_in("/wt/feat-x", "one")
        .working_in("/p/api", "main checkout");
    let three = World::default()
        .working_in("/wt/feat-x", "one")
        .archived_in("/wt/feat-x", "old")
        .working_in("/wt/feat-x", "two");
    let archived = World::default().archived_in("/wt/feat-x", "old");
    let nobody = World::default().working_in("/p/api", "main checkout");

    assert_eq!(
        one.refresh(&mut branching(picked())).note.as_deref(),
        Some("uses its existing worktree · also used by \"one\"")
    );
    assert_eq!(
        three.refresh(&mut branching(picked())).note.as_deref(),
        Some("uses its existing worktree · also used by \"one\" and 2 more")
    );
    assert_eq!(
        archived.refresh(&mut branching(picked())).note.as_deref(),
        Some("uses its existing worktree · also used by \"old\"")
    );
    assert_eq!(
        nobody.refresh(&mut branching(picked())).note.as_deref(),
        Some("uses its existing worktree")
    );
}

#[test]
fn as_is_notes_the_sessions_in_the_main_checkout() {
    let as_is = || choice(BranchMode::CheckedOut, None);
    let shared = World::default()
        .archived_in("/p/api", "old")
        .working_in("/wt/feat-x", "elsewhere")
        .working_in("/p/api", "other");

    assert_eq!(
        view_of(as_is()).note.as_deref(),
        Some("works in api as it is now")
    );
    assert_eq!(
        shared.refresh(&mut branching(as_is())).note.as_deref(),
        Some("works in api as it is now · also used by \"old\" and 1 more")
    );
}

#[test]
fn as_is_is_not_offered_in_a_worktree() {
    let mut branching = branching(choice(BranchMode::Worktree, None));
    World::default().refresh(&mut branching);

    assert_eq!(
        branching.set_kind(BranchKind::AsIs),
        Err("As is is not offered for this session".into())
    );
    assert_eq!(branching.choice().mode, BranchMode::Worktree);
    branching.set_kind(BranchKind::Existing).unwrap();
    assert_eq!(branching.choice().mode, BranchMode::ExistingWorktree);
    branching.set_folder(Folder::MainCheckout).unwrap();
    assert_eq!(branching.choice().mode, BranchMode::ExistingBranch);
    branching.set_kind(BranchKind::AsIs).unwrap();
    assert_eq!(branching.choice().mode, BranchMode::CheckedOut);
    branching.set_folder(Folder::Worktree).unwrap();
    assert_eq!(branching.choice().mode, BranchMode::Worktree);
    branching.set_kind(BranchKind::New).unwrap();
    assert_eq!(branching.choice().mode, BranchMode::Worktree);
}

#[test]
fn an_existing_branch_is_picked_from_the_options() {
    let mut branching = branching(choice(BranchMode::ExistingWorktree, None));
    World::default().refresh(&mut branching);

    assert_eq!(
        branching.set_existing("nope".into()),
        Err("nope is not a branch of api".into())
    );
    assert_eq!(branching.choice().existing, None);
    branching.set_existing("feat/x".into()).unwrap();
    assert_eq!(branching.choice().existing.as_deref(), Some("feat/x"));
    assert_eq!(branching.choice().mode, BranchMode::ExistingWorktree);
}

#[test]
fn an_orchestrator_offers_no_existing() {
    let target = Target::Orchestrator {
        repositories: vec![
            (api(), RepoState::Ready(info())),
            (web(), RepoState::Ready(info())),
        ],
    };
    let mut branching = Branching::new(
        project(),
        target,
        BranchChoice {
            branch: "x".into(),
            ..choice(BranchMode::Worktree, None)
        },
        Ok(ProjectSettings::default()),
    );

    let view = World::default().refresh(&mut branching);

    assert_eq!(view.modes, ORCHESTRATOR_MODES);
    assert!(view.existing_options.is_empty());
    assert_eq!(view.use_existing, None);
    assert_eq!(
        branching.set_kind(BranchKind::Existing),
        Err("Existing worktree is not offered for this session".into())
    );
    assert_eq!(
        branching.set_existing("main".into()),
        Err("Existing is not offered for this session".into())
    );
    assert_eq!(
        branching.use_existing(),
        Err("there is no existing branch to use".into())
    );
    assert_eq!(branching.choice().mode, BranchMode::Worktree);
}

#[test]
fn a_repository_offers_every_mode() {
    let view = view_of(choice(BranchMode::CheckedOut, None));

    assert_eq!(view.modes, ALL_MODES);
}

#[test]
fn checked_out_makes_nothing() {
    let view = view_of(choice(BranchMode::CheckedOut, None));

    assert_eq!(make_of(&view), None);
}

/// The check of a split whose only child in `api` wants `picked`, with `info` read.
fn check_child(picked: &BranchChoice) -> CheckedChild {
    let repository = api();
    let id = repository.id.clone();
    let children = [SplitChild {
        id: &id,
        repository: Some(&repository),
        choice: picked,
        title: "Child".into(),
    }];
    let reads = HashMap::from([(repository.path.clone(), RepoState::Ready(info()))]);
    let global = GlobalSettings::default();
    let context = Context {
        sessions: &[],
        global: &global,
        busy: &|_| false,
        exists: &|_| false,
    };
    let mut checked = check_children(
        &project(),
        &ProjectSettings::default(),
        &children,
        &reads,
        &context,
    );
    assert_eq!(checked.len(), 1);
    checked.remove(0)
}

#[test]
fn a_split_child_plans_existing_work() {
    let added = worktree_at(
        &ProjectSettings::default(),
        &GlobalSettings::default(),
        &project(),
        &api(),
        "b",
    )
    .unwrap()
    .unwrap();
    let planned = |make: Make, branch: &str| Planned {
        repository: api(),
        branch: branch.into(),
        make,
        before: Head::Branch("main".into()),
    };
    let reuse = PathBuf::from("/wt/feat-x");

    let reused = check_child(&choice(BranchMode::ExistingWorktree, Some("feat/x")));
    let add = check_child(&choice(BranchMode::ExistingWorktree, Some("b")));
    let switch = check_child(&choice(BranchMode::ExistingBranch, Some("b")));
    let as_is = check_child(&choice(BranchMode::ExistingBranch, Some("main")));
    let gone = check_child(&choice(BranchMode::ExistingWorktree, Some("gone")));
    let unknown = check_child(&choice(BranchMode::ExistingBranch, Some("nope")));

    assert_eq!(
        reused.planned,
        Some(planned(
            Make::Reuse {
                path: reuse.clone()
            },
            "feat/x"
        ))
    );
    assert_eq!(reused.check.worktree, Some(reuse));
    assert_eq!(
        add.planned,
        Some(planned(
            Make::AddWorktree {
                path: added.clone()
            },
            "b"
        ))
    );
    assert_eq!(add.check.worktree, Some(added));
    assert_eq!(add.check.note.as_deref(), Some("adds a worktree for it"));
    assert_eq!(switch.planned, Some(planned(Make::SwitchTo, "b")));
    assert_eq!(switch.check.worktree, None);
    assert_eq!((as_is.planned, as_is.check.problem), (None, None));
    assert_eq!(
        as_is.check.note.as_deref(),
        Some("works in api as it is now")
    );
    assert_eq!(gone.planned, None);
    assert_eq!(
        gone.check.problem.as_deref(),
        Some("its worktree at /p/api-wt/gone is gone; run git worktree prune")
    );
    assert_eq!(gone.check.note, None);
    assert_eq!(gone.check.existing_options, existing_options(&info()));
    assert_eq!(
        unknown.check.problem.as_deref(),
        Some("nope is not a branch of api")
    );
    assert_eq!((unknown.check.note, unknown.planned), (None, None));
}

#[test]
fn an_answer_that_describes_no_repository_fails_it() {
    let other = repo_state(Some(Ok(GitAnswer::Changes(0))));
    let missing = repo_state(None);

    for state in [other, missing] {
        assert!(
            matches!(&state, RepoState::Failed(error) if error == "git did not describe the repository"),
            "{state:?}"
        );
    }
}

#[test]
fn a_checked_out_workspace_has_nothing_to_undo() {
    let main = Head::Branch("main".into());

    assert!(undo_steps(Path::new("/p/api"), &Workspace::CheckedOut, &main).is_empty());
}

#[test]
fn undoing_existing_work_never_deletes_the_branch() {
    let repository = Path::new("/p/api");
    let before = Head::Detached("abc1234".into());
    let path = PathBuf::from("/p/api-wt/feat/x");

    assert_eq!(
        undo_steps(
            repository,
            &Workspace::ExistingBranch {
                branch: "feat/x".into()
            },
            &before
        ),
        [GitStep::RestoreHead {
            repository: repository.to_path_buf(),
            head: before.clone(),
        }]
    );
    assert_eq!(
        undo_steps(
            repository,
            &Workspace::WorktreeOnExisting {
                branch: "feat/x".into(),
                path: path.clone(),
            },
            &before
        ),
        [GitStep::RemoveWorktree {
            repository: repository.to_path_buf(),
            path: path.clone(),
        }]
    );
    assert!(
        undo_steps(
            repository,
            &Workspace::ExistingWorktree {
                branch: "feat/x".into(),
                path,
            },
            &before
        )
        .is_empty()
    );
}

#[test]
fn every_mode_has_its_sheet_label() {
    assert_eq!(label(BranchMode::Worktree), "Worktree");
    assert_eq!(label(BranchMode::NewBranch), "New branch");
    assert_eq!(label(BranchMode::CheckedOut), "Checked-out");
    assert_eq!(label(BranchMode::ExistingBranch), "Existing branch");
    assert_eq!(label(BranchMode::ExistingWorktree), "Existing worktree");
}
