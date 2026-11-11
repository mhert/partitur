//! Drives the real [`GitCli`] through the [`Git`] port against temporary repositories made with
//! the `git` command line, and checks the events it reports.
//!
//! Every git process, the fixture's own and the port's, runs with the user's global and system
//! configuration switched off and a fixed author, and no test reaches the network: the remote
//! is a local bare repository.

#![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use futures::channel::oneshot;
use futures::executor::block_on;
use futures::future::{Either, select};
use partitur_core::{
    Git, GitAnswer, GitError, GitEvent, GitQuery, GitRequestId, GitStep, Head, LocalBranch,
    RepoInfo,
};
use partitur_git::GitCli;
use tempfile::TempDir;

/// How long a test waits for one event before it fails.
const GUARD: Duration = Duration::from_secs(10);

/// The environment that keeps git away from the user's configuration.
fn isolated() -> Vec<(String, String)> {
    [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_AUTHOR_NAME", "Partitur Test"),
        ("GIT_AUTHOR_EMAIL", "test@partitur.invalid"),
        ("GIT_COMMITTER_NAME", "Partitur Test"),
        ("GIT_COMMITTER_EMAIL", "test@partitur.invalid"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

/// [`isolated`] plus permission for the `ext::` transport the hanging remote uses.
fn isolated_with_ext() -> Vec<(String, String)> {
    let mut env = isolated();
    env.extend(
        [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "protocol.ext.allow"),
            ("GIT_CONFIG_VALUE_0", "always"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned())),
    );
    env
}

/// A git process in `dir`, isolated like the port's and blind to any repository the test
/// process itself was pointed at.
fn git_command(dir: &Path) -> Command {
    let mut git = Command::new("git");
    git.arg("-C").arg(dir).envs(isolated());
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
    ] {
        git.env_remove(key);
    }
    git
}

/// Runs git in `dir` for the fixture and returns its trimmed output; fails the test when git
/// fails.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_command(dir).args(args).output().expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str, text: &str) {
    fs::write(dir.join(file), text).expect("write a file");
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", text]);
}

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    origin: PathBuf,
    api: PathBuf,
    cli: GitCli,
    events: UnboundedReceiver<GitEvent>,
}

/// A bare `origin` with one commit on `main`, a clone `api` of it, and a port that runs git
/// isolated from the user's configuration.
fn fixture() -> Fixture {
    fixture_with(isolated())
}

fn fixture_with(env: Vec<(String, String)>) -> Fixture {
    let dir = TempDir::new().expect("a temporary folder");
    let root = dir.path().canonicalize().expect("the folder resolves");
    git(
        &root,
        &["init", "--quiet", "--bare", "-b", "main", "origin"],
    );
    git(&root, &["init", "--quiet", "-b", "main", "seed"]);
    let seed = root.join("seed");
    let origin = root.join("origin");
    commit(&seed, "README", "first");
    git(&seed, &["push", "--quiet", &path_text(&origin), "main"]);
    git(&root, &["clone", "--quiet", &path_text(&origin), "api"]);
    let (cli, events) = GitCli::start();
    Fixture {
        _dir: dir,
        api: root.join("api"),
        root,
        origin,
        cli: cli.with_env(env),
        events,
    }
}

/// The [`fixture`] with a remote `slow` whose fetch hangs for five seconds, and a port that
/// stops a fetch after `limit`.
fn slow_fixture(limit: Duration) -> Fixture {
    let fixture = fixture_with(isolated_with_ext());
    // The `ext::` transport runs this command as the remote; `% ` is a space within one word.
    git(
        &fixture.api,
        &["remote", "add", "slow", "ext::sh -c sleep% 5"],
    );
    Fixture {
        cli: fixture.cli.with_fetch_limit(limit),
        ..fixture
    }
}

fn path_text(path: &Path) -> String {
    path.to_str().expect("a UTF-8 path").to_owned()
}

fn next_request() -> GitRequestId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    GitRequestId(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Waits for the next event, failing the test after [`GUARD`].
fn next_event(events: &mut UnboundedReceiver<GitEvent>) -> GitEvent {
    let (fire, alarm) = oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(GUARD);
        let _ = fire.send(());
    });
    match block_on(select(events.next(), alarm)) {
        Either::Left((Some(event), _)) => event,
        Either::Left((None, _)) => panic!("the event channel closed"),
        Either::Right(_) => panic!("no git event within {GUARD:?}"),
    }
}

/// Queues `queries` as one request and returns its answers.
fn ask(
    cli: &GitCli,
    events: &mut UnboundedReceiver<GitEvent>,
    queries: Vec<GitQuery>,
) -> Vec<Result<GitAnswer, GitError>> {
    let request = next_request();
    cli.query(request, queries).expect("the query is queued");
    match next_event(events) {
        GitEvent::Answered {
            request: answered,
            answers,
        } if answered == request => answers,
        other => panic!("expected the answers to {request}, got {other:?}"),
    }
}

/// Queues `steps` as one request and returns how many succeeded and the result.
fn apply(
    cli: &GitCli,
    events: &mut UnboundedReceiver<GitEvent>,
    steps: Vec<GitStep>,
) -> (usize, Result<(), GitError>) {
    let request = next_request();
    cli.run(request, steps).expect("the steps are queued");
    match next_event(events) {
        GitEvent::Ran {
            request: ran,
            done,
            result,
        } if ran == request => (done, result),
        other => panic!("expected the outcome of {request}, got {other:?}"),
    }
}

impl Fixture {
    fn ask_one(&mut self, query: GitQuery) -> Result<GitAnswer, GitError> {
        let mut answers = ask(&self.cli, &mut self.events, vec![query]);
        assert_eq!(answers.len(), 1);
        answers.remove(0)
    }

    fn inspect(&mut self, repository: &Path) -> RepoInfo {
        match self.ask_one(GitQuery::Inspect(repository.to_path_buf())) {
            Ok(GitAnswer::Repo(info)) => info,
            other => panic!("expected a repository, got {other:?}"),
        }
    }

    fn changes(&mut self, worktree: &Path) -> usize {
        match self.ask_one(GitQuery::Changes(worktree.to_path_buf())) {
            Ok(GitAnswer::Changes(count)) => count,
            other => panic!("expected a change count, got {other:?}"),
        }
    }

    fn apply(&mut self, steps: Vec<GitStep>) -> (usize, Result<(), GitError>) {
        apply(&self.cli, &mut self.events, steps)
    }

    fn apply_ok(&mut self, step: GitStep) {
        assert_eq!(self.apply(vec![step]), (1, Ok(())));
    }

    fn slow_fetch(&self) -> GitStep {
        GitStep::Fetch {
            repository: self.api.clone(),
            remote: "slow".into(),
        }
    }
}

fn add_worktree(repository: &Path, path: &Path, branch: &str) -> GitStep {
    GitStep::AddWorktree {
        repository: repository.to_path_buf(),
        path: path.to_path_buf(),
        branch: branch.into(),
        start: "main".into(),
    }
}

fn delete_branch(repository: &Path, branch: &str) -> GitStep {
    GitStep::DeleteBranch {
        repository: repository.to_path_buf(),
        branch: branch.into(),
    }
}

fn assert_failed(result: &Result<impl std::fmt::Debug, GitError>, command: &str) {
    match result {
        Err(GitError::Failed {
            command: failed,
            stderr,
        }) => {
            assert_eq!(failed, command);
            assert!(!stderr.is_empty(), "git said why");
            assert_eq!(stderr.trim(), stderr, "the error output is trimmed");
        }
        other => panic!("expected git {command} to fail, got {other:?}"),
    }
}

// The port.

#[test]
fn a_request_returns_at_once_and_its_outcome_arrives_on_the_channel() {
    let mut fixture = slow_fixture(Duration::from_secs(2));
    let request = next_request();

    let started = Instant::now();
    fixture
        .cli
        .run(request, vec![fixture.slow_fetch()])
        .expect("the fetch is queued");
    let queued = started.elapsed();

    assert!(queued < Duration::from_millis(500), "queued in {queued:?}");
    let event = next_event(&mut fixture.events);
    assert_eq!(event.request(), request);
    assert!(started.elapsed() >= Duration::from_secs(2));
}

#[test]
fn every_query_is_answered_even_after_one_fails() {
    let mut fixture = fixture();
    let nowhere = fixture.root.join("nowhere");
    fs::create_dir(&nowhere).expect("a plain folder");

    let answers = ask(
        &fixture.cli,
        &mut fixture.events,
        vec![
            GitQuery::Inspect(fixture.api.clone()),
            GitQuery::Inspect(nowhere),
            GitQuery::Changes(fixture.api.clone()),
        ],
    );

    assert_eq!(answers.len(), 3);
    assert!(matches!(answers[0], Ok(GitAnswer::Repo(_))));
    assert_failed(&answers[1], "worktree list");
    assert_eq!(answers[2], Ok(GitAnswer::Changes(0)));
}

#[test]
fn a_run_stops_at_the_first_failing_step() {
    let mut fixture = fixture();
    let worktree = fixture.root.join("wt-x");
    let api = fixture.api.clone();

    let (done, result) = fixture.apply(vec![
        add_worktree(&api, &worktree, "x"),
        add_worktree(&api, &worktree, "x"),
        delete_branch(&api, "main"),
    ]);

    assert_eq!(done, 1);
    assert_failed(&result, "worktree add");
    assert!(fixture.inspect(&api).has_branch("main"));
}

#[test]
fn a_slow_fetch_does_not_hold_up_another_request() {
    let mut fixture = slow_fixture(Duration::from_secs(2));
    let slow = next_request();
    let quick = next_request();

    fixture
        .cli
        .run(slow, vec![fixture.slow_fetch()])
        .expect("the fetch is queued");
    fixture
        .cli
        .query(quick, vec![GitQuery::Inspect(fixture.api.clone())])
        .expect("the query is queued");

    let first = next_event(&mut fixture.events);
    let second = next_event(&mut fixture.events);
    assert_eq!(first.request(), quick);
    assert!(matches!(first, GitEvent::Answered { .. }));
    assert_eq!(
        second,
        GitEvent::Ran {
            request: slow,
            done: 0,
            result: Err(GitError::TimedOut {
                command: "fetch".into(),
                seconds: 2
            }),
        }
    );
}

#[test]
fn an_outcome_nobody_receives_any_more_is_no_error() {
    let Fixture {
        cli, events, api, ..
    } = fixture();
    drop(events);

    assert_eq!(
        cli.query(next_request(), vec![GitQuery::Inspect(api)]),
        Ok(())
    );
}

// Git behaviour.

#[test]
fn inspect_lists_the_checkout_branches_upstreams_and_remote_branches() {
    let mut fixture = fixture();
    let api = fixture.api.clone();

    let info = fixture.inspect(&api);

    assert_eq!(info.worktrees.len(), 1);
    assert_eq!(info.worktrees[0].path, api);
    assert_eq!(info.worktrees[0].head, Head::Branch("main".into()));
    assert!(!info.worktrees[0].prunable);
    assert_eq!(
        info.branches,
        [LocalBranch {
            name: "main".into(),
            upstream_remote: Some("origin".into()),
        }]
    );
    assert_eq!(info.remote_branches, ["origin/main"]);
}

#[test]
fn a_repository_named_by_the_environment_does_not_redirect_a_request() {
    let mut env = isolated();
    let fixture = fixture();
    let elsewhere = fixture.root.join("origin");
    env.extend(
        [
            ("GIT_DIR", path_text(&elsewhere)),
            ("GIT_WORK_TREE", path_text(&fixture.root)),
            ("GIT_INDEX_FILE", path_text(&fixture.root.join("index"))),
        ]
        .map(|(key, value)| (key.to_owned(), value)),
    );
    let api = fixture.api.clone();
    let mut fixture = Fixture {
        cli: fixture.cli.with_env(env),
        ..fixture
    };

    fixture.apply_ok(GitStep::CreateBranch {
        repository: api.clone(),
        branch: "feat/x".into(),
        start: "main".into(),
    });

    let info = fixture.inspect(&api);
    assert_eq!(info.worktrees[0].path, api);
    assert_eq!(info.current_branch(), Some("feat/x"));
    assert_eq!(git(&elsewhere, &["branch", "--list", "feat/x"]), "");
}

#[test]
fn a_folder_that_is_no_repository_fails_inspect() {
    let mut fixture = fixture();
    let nowhere = fixture.root.join("nowhere");
    fs::create_dir(&nowhere).expect("a plain folder");

    assert_failed(
        &fixture.ask_one(GitQuery::Inspect(nowhere)),
        "worktree list",
    );
}

#[test]
fn a_new_branch_switches_the_checkout_and_does_not_track() {
    let mut fixture = fixture();
    let api = fixture.api.clone();

    fixture.apply_ok(GitStep::CreateBranch {
        repository: api.clone(),
        branch: "feat/x".into(),
        start: "origin/main".into(),
    });

    let info = fixture.inspect(&api);
    assert_eq!(info.current_branch(), Some("feat/x"));
    assert!(info.branches.contains(&LocalBranch {
        name: "feat/x".into(),
        upstream_remote: None,
    }));
    let merge = git_command(&api)
        .args(["config", "branch.feat/x.merge"])
        .output()
        .expect("git runs");
    assert!(!merge.status.success(), "feat/x has no upstream");
}

#[test]
fn creating_a_branch_that_exists_fails() {
    let mut fixture = fixture();
    let api = fixture.api.clone();

    let (done, result) = fixture.apply(vec![GitStep::CreateBranch {
        repository: api,
        branch: "main".into(),
        start: "origin/main".into(),
    }]);

    assert_eq!(done, 0);
    assert_failed(&result, "switch");
}

#[test]
fn uncommitted_changes_carry_over_to_a_new_branch() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    fs::write(api.join("README"), "changed").expect("modify a file");

    fixture.apply_ok(GitStep::CreateBranch {
        repository: api.clone(),
        branch: "feat/x".into(),
        start: "main".into(),
    });

    assert_eq!(fixture.inspect(&api).current_branch(), Some("feat/x"));
    assert_eq!(
        fs::read_to_string(api.join("README")).ok().as_deref(),
        Some("changed")
    );
    assert_eq!(fixture.changes(&api), 1);
}

#[test]
fn a_worktree_is_added_listed_counted_and_removed_by_force() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    let worktree = fixture.root.join("api-wt").join("feat-x");

    fixture.apply_ok(add_worktree(&api, &worktree, "feat/x"));

    let info = fixture.inspect(&api);
    assert_eq!(info.worktrees.len(), 2);
    assert_eq!(info.worktrees[1].path, worktree);
    assert_eq!(info.worktrees[1].head, Head::Branch("feat/x".into()));
    assert_eq!(info.checked_out_at("feat/x"), Some(worktree.as_path()));
    assert!(info.branches.contains(&LocalBranch {
        name: "feat/x".into(),
        upstream_remote: None,
    }));

    fs::write(worktree.join("one"), "1").expect("an untracked file");
    fs::create_dir(worktree.join("dir")).expect("an untracked folder");
    fs::write(worktree.join("dir").join("two"), "2").expect("an untracked file");
    fs::write(worktree.join("README"), "changed").expect("modify a file");
    assert_eq!(fixture.changes(&worktree), 3);

    fixture.apply_ok(GitStep::RemoveWorktree {
        repository: api.clone(),
        path: worktree.clone(),
    });

    assert!(!worktree.exists());
    assert_eq!(fixture.inspect(&api).worktrees.len(), 1);
}

#[test]
fn the_main_worktree_cannot_be_removed() {
    let mut fixture = fixture();
    let api = fixture.api.clone();

    let (done, result) = fixture.apply(vec![GitStep::RemoveWorktree {
        repository: api.clone(),
        path: api,
    }]);

    assert_eq!(done, 0);
    assert_failed(&result, "worktree remove");
}

#[test]
fn restore_head_returns_to_a_branch_and_to_a_detached_commit() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    let commit = git(&api, &["rev-parse", "HEAD"]);
    fixture.apply_ok(GitStep::CreateBranch {
        repository: api.clone(),
        branch: "feat/x".into(),
        start: "main".into(),
    });

    fixture.apply_ok(GitStep::RestoreHead {
        repository: api.clone(),
        head: Head::Branch("main".into()),
    });
    assert_eq!(
        fixture.inspect(&api).head(),
        Some(&Head::Branch("main".into()))
    );

    fixture.apply_ok(GitStep::RestoreHead {
        repository: api.clone(),
        head: Head::Detached(commit.clone()),
    });
    assert_eq!(fixture.inspect(&api).head(), Some(&Head::Detached(commit)));

    let (done, result) = fixture.apply(vec![GitStep::RestoreHead {
        repository: api,
        head: Head::Branch("gone".into()),
    }]);
    assert_eq!(done, 0);
    assert_failed(&result, "switch");
}

#[test]
fn restore_head_never_creates_a_branch_from_its_remote_namesake() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    git(&api, &["push", "--quiet", "origin", "main:topic"]);
    git(&api, &["fetch", "--quiet", "origin"]);
    assert!(
        fixture
            .inspect(&api)
            .remote_branches
            .contains(&"origin/topic".to_owned())
    );

    let (done, result) = fixture.apply(vec![GitStep::RestoreHead {
        repository: api.clone(),
        head: Head::Branch("topic".into()),
    }]);

    assert_eq!(done, 0);
    assert_failed(&result, "switch");
    let info = fixture.inspect(&api);
    assert!(!info.has_branch("topic"));
    assert_eq!(info.current_branch(), Some("main"));
}

#[test]
fn delete_branch_forces_an_unmerged_branch_and_is_merged_tells() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    let is_merged = |branch: &str| GitQuery::IsMerged {
        repository: api.clone(),
        branch: branch.into(),
        base: "main".into(),
    };
    fixture.apply_ok(GitStep::CreateBranch {
        repository: api.clone(),
        branch: "feat/x".into(),
        start: "main".into(),
    });
    assert_eq!(
        fixture.ask_one(is_merged("feat/x")),
        Ok(GitAnswer::Merged(true))
    );
    commit(&api, "feature", "feature work");
    fixture.apply_ok(GitStep::RestoreHead {
        repository: api.clone(),
        head: Head::Branch("main".into()),
    });

    assert_eq!(
        fixture.ask_one(is_merged("feat/x")),
        Ok(GitAnswer::Merged(false))
    );
    assert_failed(&fixture.ask_one(is_merged("gone")), "merge-base");

    fixture.apply_ok(delete_branch(&api, "feat/x"));
    assert!(!fixture.inspect(&api).has_branch("feat/x"));

    let (done, result) = fixture.apply(vec![delete_branch(&api, "main")]);
    assert_eq!(done, 0);
    assert_failed(&result, "branch");
}

#[test]
fn fetch_updates_the_remote_branch_and_leaves_main_alone() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    let before = git(&api, &["rev-parse", "main"]);
    git(
        &fixture.root,
        &["clone", "--quiet", &path_text(&fixture.origin), "other"],
    );
    let other = fixture.root.join("other");
    commit(&other, "news", "from elsewhere");
    git(&other, &["push", "--quiet", "origin", "main"]);
    let pushed = git(&other, &["rev-parse", "main"]);

    fixture.apply_ok(GitStep::Fetch {
        repository: api.clone(),
        remote: "origin".into(),
    });

    assert_eq!(git(&api, &["rev-parse", "origin/main"]), pushed);
    assert_eq!(git(&api, &["rev-parse", "main"]), before);

    let (done, result) = fixture.apply(vec![GitStep::Fetch {
        repository: api,
        remote: "nowhere".into(),
    }]);
    assert_eq!(done, 0);
    assert_failed(&result, "fetch");
}

#[test]
fn a_hanging_fetch_is_stopped_at_the_limit() {
    let mut fixture = slow_fixture(Duration::from_millis(200));
    let started = Instant::now();

    let step = fixture.slow_fetch();
    let outcome = fixture.apply(vec![step]);

    assert_eq!(
        outcome,
        (
            0,
            Err(GitError::TimedOut {
                command: "fetch".into(),
                seconds: 0
            })
        )
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_worktree_folder_deleted_by_hand_is_prunable() {
    let mut fixture = fixture();
    let api = fixture.api.clone();
    let worktree = fixture.root.join("wt-x");
    fixture.apply_ok(add_worktree(&api, &worktree, "x"));

    fs::remove_dir_all(&worktree).expect("delete the worktree folder");

    let info = fixture.inspect(&api);
    assert_eq!(info.worktrees.len(), 2);
    assert!(!info.worktrees[0].prunable);
    assert!(info.worktrees[1].prunable);
    assert_eq!(info.checked_out_at("x"), None);
}
