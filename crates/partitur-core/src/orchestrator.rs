//! What makes a session an orchestrator: when it is one, the preamble it starts with, the
//! tool calls it may not make, and every prompt Partitur writes to it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::backend::{ApprovedChild, ChildOutcome, StopReason, ToolKind, TurnOrigin};
use crate::ids::{RepositoryId, SessionId};
use crate::project::{Project, Repository};
use crate::session::{BranchMode, Scope};

/// The result an orchestrator's `propose_children` call returns at once, before the user
/// decides the proposal.
pub const PROPOSAL_RECORDED: &str = "proposal recorded, awaiting approval";

/// The instruction file whose first line describes a repository in the preamble.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";

/// What the preamble says about a repository without a usable instruction file.
const NO_INSTRUCTIONS: &str = "(no AGENTS.md)";

/// The name Partitur's tool server is registered and served under.
pub const TOOL_SERVER: &str = "partitur";

/// One tool Partitur's tool server offers an orchestrator, as the preamble describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    /// The tool's name, without the server prefix.
    pub name: &'static str,
    /// The call as the preamble shows it: the name and its parameters.
    pub signature: &'static str,
    /// What the tool does, as one sentence fragment following the signature.
    pub description: &'static str,
}

/// The tools Partitur's tool server offers an orchestrator, in the order the preamble lists
/// them.
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "propose_children",
        signature: "propose_children(children: [{repository, prompt, title}])",
        description: "proposes one child session per entry. The proposal is shown to the user for approval; end your turn after calling it. You are woken with the started children and their session ids, or with the rejection and the user's note.",
    },
    ToolSpec {
        name: "send_to_child",
        signature: "send_to_child(session_id, prompt)",
        description: "sends a follow-up prompt to a child; it is queued while the child is mid-turn.",
    },
    ToolSpec {
        name: "list_children",
        signature: "list_children()",
        description: "lists your children with their title, repository, session id and status.",
    },
    ToolSpec {
        name: "cancel_child",
        signature: "cancel_child(session_id)",
        description: "stops a child's current turn; its result then reaches you as a child result marked cancelled.",
    },
    ToolSpec {
        name: "get_child_result",
        signature: "get_child_result(session_id)",
        description: "returns a child's last final message and its status.",
    },
];

/// The preamble from its role statement up to the tool list.
const PREAMBLE_INTRO: &str = "You are the orchestrator of this project. You may inspect but never change anything yourself: edit, delete, move and execute requests are denied in this session. Work happens in the repositories, through child sessions you propose.";

/// The preamble between the tool list and the repository list.
const PREAMBLE_OUTRO: &str = "Each child's final message reaches you as a prompt marked \"child result\". Report to the user whenever you have nothing further to drive.";

/// The closing sentence of the prompt that tells an orchestrator its children started.
const CHILDREN_STARTED_FOOTER: &str = "Each child's final message will reach you as a prompt marked \"child result\". Use send_to_child(session_id, prompt) for follow-ups.";

/// The text an orchestrator session's first prompt starts with: its role, Partitur's tools
/// and the project's repositories.
///
/// Every repository except the project root itself is listed in project order, with the
/// first non-blank line of its `AGENTS.md`, trimmed, or `(no AGENTS.md)` when that file is
/// absent, unreadable or blank. The files are read on every call.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use partitur_core::{discover_project, preamble};
///
/// let dir = tempfile::tempdir()?;
/// std::fs::create_dir_all(dir.path().join("api/.git"))?;
/// std::fs::write(dir.path().join("api/AGENTS.md"), "Keep the API compatible.\n")?;
/// let project = discover_project(dir.path())?;
///
/// let text = preamble(&project);
///
/// assert!(text.starts_with("<orchestrator project="));
/// assert!(text.ends_with("- api: Keep the API compatible.\n</orchestrator>"));
/// # Ok(())
/// # }
/// ```
pub fn preamble(project: &Project) -> String {
    let mut lines = vec![
        format!(
            "<orchestrator project=\"{}\" root=\"{}\">",
            project.name,
            project.root.display()
        ),
        PREAMBLE_INTRO.to_owned(),
        String::new(),
        format!("Tools (MCP server \"{TOOL_SERVER}\"):"),
    ];
    lines.extend(
        TOOLS
            .iter()
            .map(|spec| format!("- {}: {}", spec.signature, spec.description)),
    );
    lines.extend([
        String::new(),
        PREAMBLE_OUTRO.to_owned(),
        String::new(),
        "Repositories:".to_owned(),
    ]);
    lines.extend(
        orchestrated_repositories(project)
            .into_iter()
            .map(|repository| format!("- {}: {}", repository.name, first_line(&repository.path))),
    );
    lines.push("</orchestrator>".to_owned());
    lines.join("\n")
}

/// The first non-blank line of the `AGENTS.md` in `repository`, trimmed, or
/// `(no AGENTS.md)` when there is none to read.
fn first_line(repository: &Path) -> String {
    fs::read_to_string(repository.join(INSTRUCTIONS_FILE))
        .ok()
        .and_then(|content| {
            content
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| NO_INSTRUCTIONS.to_owned())
}

/// Whether an orchestrator session is denied a tool call of `kind`: everything that changes
/// the workspace is. A denied kind comes with its lowercase word for the
/// [`denial_notice`]; `None` means the call is not restrained.
pub(crate) fn restrained(kind: ToolKind) -> Option<&'static str> {
    match kind {
        ToolKind::Edit => Some("edit"),
        ToolKind::Delete => Some("delete"),
        ToolKind::Move => Some("move"),
        ToolKind::Execute => Some("execute"),
        ToolKind::Read
        | ToolKind::Search
        | ToolKind::Think
        | ToolKind::Fetch
        | ToolKind::SwitchMode
        | ToolKind::Other => None,
    }
}

/// Whether an orchestrator may read without asking: the call is a [`ToolKind::Read`], it
/// names at least one path and every path lies inside one of `worktrees`.
///
/// A search is never allowed this way: its locations name the folder searched, not
/// everything the pattern reaches. Paths are compared after resolving symbolic links, so a
/// link inside a worktree that leads out of it does not count. A path or worktree that
/// cannot be resolved (it does not exist, or is not readable) is never inside, and neither is
/// a relative path. The file system can still change between this check and the agent's
/// read; the check is a convenience for the user, not a sandbox.
pub(crate) fn reads_only_in(kind: ToolKind, locations: &[PathBuf], worktrees: &[&Path]) -> bool {
    let roots: Vec<PathBuf> = worktrees
        .iter()
        .filter_map(|worktree| fs::canonicalize(worktree).ok())
        .collect();
    kind == ToolKind::Read
        && !locations.is_empty()
        && locations.iter().all(|location| {
            location.is_absolute()
                && fs::canonicalize(location)
                    .is_ok_and(|real| roots.iter().any(|root| real.starts_with(root)))
        })
}

/// The notice that tells the user an orchestrator's call `title` was denied; `kind` is the
/// word [`restrained`] gave for the call's kind.
pub(crate) fn denial_notice(kind: &str, title: &str) -> String {
    format!(
        "denied: an orchestrator session may inspect but never change; delegate \"{title}\" ({kind}) to a child session"
    )
}

/// The prompt that wakes an orchestrator after the user approved its proposal: one line per
/// started child with its title, repository, session id, mode (`worktree`, `new branch` or
/// `checked-out`), the branch it made when it made one, and the folder it works in.
pub(crate) fn children_started_prompt(children: &[ApprovedChild]) -> String {
    let mut lines = vec!["children started:".to_owned()];
    lines.extend(children.iter().map(|child| {
        let mode = match child.mode {
            BranchMode::Worktree => "worktree",
            BranchMode::NewBranch => "new branch",
            BranchMode::CheckedOut => "checked-out",
        };
        let branch = child
            .branch
            .as_ref()
            .map_or_else(String::new, |branch| format!(" {branch}"));
        format!(
            "- {} · {} · session {} · {mode}{branch} · in {}",
            child.title,
            child.repository,
            child.session,
            child.cwd.display()
        )
    }));
    lines.push(CHILDREN_STARTED_FOOTER.to_owned());
    lines.join("\n")
}

/// The prompt that wakes an orchestrator after the user rejected its proposal, with the
/// user's note when it is not blank.
pub(crate) fn rejection_prompt(note: Option<&str>) -> String {
    match note.filter(|note| !note.trim().is_empty()) {
        Some(note) => format!("proposal rejected by the user\nnote: {note}"),
        None => "proposal rejected by the user".to_owned(),
    }
}

/// The prompt that reports a child's finished turn to its orchestrator: a header naming the
/// child, a line for each thing out of the ordinary (the user started the turn, it failed or
/// ended for another reason than the agent finishing), then a blank line and the child's
/// final answer.
pub(crate) fn child_result_prompt(
    child: &SessionId,
    repository: &RepositoryId,
    title: &str,
    origin: TurnOrigin,
    outcome: &ChildOutcome,
    text: &str,
) -> String {
    let mut lines = vec![
        format!("child result · {repository} · {title}"),
        format!("session {child}"),
    ];
    match origin {
        TurnOrigin::User => lines.push("(turn started by the user)".to_owned()),
        TurnOrigin::Agent => lines.push("(turn started by the agent)".to_owned()),
        TurnOrigin::Orchestrator => {}
    }
    match outcome {
        ChildOutcome::Failed(error) => lines.push(format!("(failed: {error})")),
        ChildOutcome::Ended(reason) => {
            if let Some(word) = stop_word(*reason) {
                lines.push(format!("(turn ended: {word})"));
            }
        }
    }
    format!("{}\n\n{text}", lines.join("\n"))
}

/// Why a child's turn ended, in words, `None` when the agent simply finished.
fn stop_word(reason: StopReason) -> Option<&'static str> {
    match reason {
        StopReason::EndTurn => None,
        StopReason::MaxTokens => Some("token limit reached"),
        StopReason::MaxTurnRequests => Some("request limit reached"),
        StopReason::Refusal => Some("the agent refused"),
        StopReason::Cancelled => Some("cancelled"),
    }
}

/// Whether a session of `scope` in `project` is an orchestrator: it works on the whole
/// project, and the project has at least two repositories besides its root.
pub(crate) fn is_orchestrator(project: &Project, scope: &Scope) -> bool {
    matches!(scope, Scope::Project)
        && project
            .repositories
            .iter()
            .filter(|repository| !repository.is_root)
            .count()
            >= 2
}

/// The repositories an orchestrator in `project` works on, in project order: those below
/// the project root, or, when there are none, the root itself if it is a repository. Empty
/// for a folder without any repository.
pub(crate) fn orchestrated_repositories(project: &Project) -> Vec<&Repository> {
    let below: Vec<&Repository> = project
        .repositories
        .iter()
        .filter(|repository| !repository.is_root)
        .collect();
    if below.is_empty() {
        project
            .repositories
            .iter()
            .filter(|repository| repository.is_root)
            .collect()
    } else {
        below
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::path::PathBuf;

    use super::*;
    use crate::ids::ProjectId;
    use crate::project::{Repository, discover_project};

    fn repository(name: &str, is_root: bool) -> Repository {
        Repository {
            id: RepositoryId::try_from(name).unwrap(),
            path: PathBuf::from("/work").join(name),
            name: name.into(),
            has_instructions: false,
            has_claude_md: false,
            is_root,
        }
    }

    fn project(repositories: Vec<Repository>) -> Project {
        Project {
            id: ProjectId::new(),
            root: PathBuf::from("/work"),
            name: "work".into(),
            repositories,
            has_global_instructions: false,
            has_global_claude_md: false,
            closed: false,
            fold: crate::Fold::Open,
        }
    }

    #[test]
    fn the_preamble_lists_the_repositories_with_their_first_lines() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["api", "web"] {
            fs::create_dir_all(dir.path().join(name).join(".git")).unwrap();
        }
        fs::write(
            dir.path().join("api/AGENTS.md"),
            "\n  Keep the API compatible.\n",
        )
        .unwrap();
        let project = discover_project(dir.path()).unwrap();

        let expected = format!(
            "<orchestrator project=\"{}\" root=\"{}\">
You are the orchestrator of this project. You may inspect but never change anything yourself: edit, delete, move and execute requests are denied in this session. Work happens in the repositories, through child sessions you propose.

Tools (MCP server \"partitur\"):
- propose_children(children: [{{repository, prompt, title}}]): proposes one child session per entry. The proposal is shown to the user for approval; end your turn after calling it. You are woken with the started children and their session ids, or with the rejection and the user's note.
- send_to_child(session_id, prompt): sends a follow-up prompt to a child; it is queued while the child is mid-turn.
- list_children(): lists your children with their title, repository, session id and status.
- cancel_child(session_id): stops a child's current turn; its result then reaches you as a child result marked cancelled.
- get_child_result(session_id): returns a child's last final message and its status.

Each child's final message reaches you as a prompt marked \"child result\". Report to the user whenever you have nothing further to drive.

Repositories:
- api: Keep the API compatible.
- web: (no AGENTS.md)
</orchestrator>",
            project.name,
            dir.path().display()
        );
        assert_eq!(preamble(&project), expected);
    }

    #[test]
    fn every_tool_is_listed_in_the_preamble() {
        let text = preamble(&project(vec![]));

        for spec in TOOLS {
            let line = format!("- {}: {}", spec.signature, spec.description);
            assert!(text.contains(&line), "missing {line:?}");
        }
    }

    #[test]
    fn the_tool_server_is_named_partitur() {
        assert_eq!(TOOL_SERVER, "partitur");
        assert!(preamble(&project(vec![])).contains("Tools (MCP server \"partitur\"):"));
    }

    #[test]
    fn only_changing_kinds_are_restrained() {
        for (kind, word) in [
            (ToolKind::Edit, "edit"),
            (ToolKind::Delete, "delete"),
            (ToolKind::Move, "move"),
            (ToolKind::Execute, "execute"),
        ] {
            assert_eq!(
                restrained(kind),
                Some(word),
                "{kind:?} should be restrained"
            );
        }
        for kind in [
            ToolKind::Read,
            ToolKind::Search,
            ToolKind::Fetch,
            ToolKind::Think,
            ToolKind::SwitchMode,
            ToolKind::Other,
        ] {
            assert_eq!(restrained(kind), None, "{kind:?} should not be restrained");
        }
        assert_eq!(
            denial_notice("execute", "Run cargo test"),
            "denied: an orchestrator session may inspect but never change; delegate \"Run cargo test\" (execute) to a child session"
        );
    }

    #[test]
    fn generated_prompts_are_worded_exactly() {
        let (first, second) = (SessionId::new(), SessionId::new());
        let api = RepositoryId::try_from("api").unwrap();
        let web = RepositoryId::try_from("web").unwrap();
        let child = |session: &SessionId, repository: &RepositoryId, title: &str| ApprovedChild {
            session: session.clone(),
            repository: repository.clone(),
            prompt: "do it".into(),
            title: title.into(),
            auto_allow: false,
            mode: BranchMode::CheckedOut,
            branch: None,
            cwd: PathBuf::from(format!("/p/{repository}")),
        };

        assert_eq!(
            children_started_prompt(&[
                child(&first, &api, "Fix API"),
                child(&second, &web, "Fix web")
            ]),
            format!(
                "children started:\n- Fix API · api · session {first} · checked-out · in /p/api\n- Fix web · web · session {second} · checked-out · in /p/web\nEach child's final message will reach you as a prompt marked \"child result\". Use send_to_child(session_id, prompt) for follow-ups."
            )
        );
        assert_eq!(rejection_prompt(None), "proposal rejected by the user");
        assert_eq!(
            rejection_prompt(Some("  ")),
            "proposal rejected by the user"
        );
        assert_eq!(
            rejection_prompt(Some("later")),
            "proposal rejected by the user\nnote: later"
        );

        let header = format!("child result · api · Fix API\nsession {first}");
        let result = |origin, outcome: ChildOutcome| {
            child_result_prompt(&first, &api, "Fix API", origin, &outcome, "Done.")
        };
        assert_eq!(
            result(
                TurnOrigin::Orchestrator,
                ChildOutcome::Ended(StopReason::EndTurn)
            ),
            format!("{header}\n\nDone.")
        );
        assert_eq!(
            result(TurnOrigin::User, ChildOutcome::Ended(StopReason::EndTurn)),
            format!("{header}\n(turn started by the user)\n\nDone.")
        );
        assert_eq!(
            result(
                TurnOrigin::Orchestrator,
                ChildOutcome::Failed("boom".into())
            ),
            format!("{header}\n(failed: boom)\n\nDone.")
        );
        assert_eq!(
            result(TurnOrigin::User, ChildOutcome::Failed("boom".into())),
            format!("{header}\n(turn started by the user)\n(failed: boom)\n\nDone.")
        );
        assert_eq!(
            result(TurnOrigin::Agent, ChildOutcome::Ended(StopReason::EndTurn)),
            format!("{header}\n(turn started by the agent)\n\nDone.")
        );
        for (reason, word) in [
            (StopReason::MaxTokens, "token limit reached"),
            (StopReason::MaxTurnRequests, "request limit reached"),
            (StopReason::Refusal, "the agent refused"),
            (StopReason::Cancelled, "cancelled"),
        ] {
            assert_eq!(
                result(TurnOrigin::Orchestrator, ChildOutcome::Ended(reason)),
                format!("{header}\n(turn ended: {word})\n\nDone.")
            );
            assert_eq!(
                result(TurnOrigin::User, ChildOutcome::Ended(reason)),
                format!("{header}\n(turn started by the user)\n(turn ended: {word})\n\nDone.")
            );
        }
    }

    #[test]
    fn the_approval_prompt_names_mode_branch_and_folder() {
        let session = SessionId::new();
        let child = |mode, branch: Option<&str>, cwd: &str| ApprovedChild {
            session: session.clone(),
            repository: RepositoryId::try_from("api").unwrap(),
            prompt: "do it".into(),
            title: "Fix API".into(),
            auto_allow: false,
            mode,
            branch: branch.map(Into::into),
            cwd: PathBuf::from(cwd),
        };
        let line = |child: ApprovedChild| {
            children_started_prompt(&[child])
                .lines()
                .nth(1)
                .unwrap()
                .to_owned()
        };

        assert_eq!(
            line(child(
                BranchMode::Worktree,
                Some("feat/x"),
                "/p/api-wt/feat/x"
            )),
            format!("- Fix API · api · session {session} · worktree feat/x · in /p/api-wt/feat/x")
        );
        assert_eq!(
            line(child(BranchMode::NewBranch, Some("feat/x"), "/p/api")),
            format!("- Fix API · api · session {session} · new branch feat/x · in /p/api")
        );
        assert_eq!(
            line(child(BranchMode::CheckedOut, None, "/p/api")),
            format!("- Fix API · api · session {session} · checked-out · in /p/api")
        );
    }

    #[test]
    fn a_root_only_preamble_lists_the_root_repository() {
        let text = preamble(&project(vec![repository("work", true)]));

        assert!(
            text.ends_with("Repositories:\n- work: (no AGENTS.md)\n</orchestrator>"),
            "{text}"
        );
    }

    #[test]
    fn orchestrated_repositories_are_those_below_the_root_or_else_the_root() {
        let names = |project: &Project| -> Vec<String> {
            orchestrated_repositories(project)
                .iter()
                .map(|repository| repository.name.clone())
                .collect()
        };

        assert_eq!(
            names(&project(vec![
                repository("api", false),
                repository("web", false)
            ])),
            ["api", "web"]
        );
        assert_eq!(
            names(&project(vec![
                repository("work", true),
                repository("api", false)
            ])),
            ["api"]
        );
        assert_eq!(names(&project(vec![repository("work", true)])), ["work"]);
        assert_eq!(names(&project(vec![repository("api", false)])), ["api"]);
        assert!(names(&project(vec![])).is_empty());
    }

    #[test]
    fn a_project_needs_two_repositories_to_orchestrate() {
        let one = project(vec![repository("api", false)]);
        let two = project(vec![repository("api", false), repository("web", false)]);
        let root = project(vec![repository("work", true), repository("api", false)]);
        let api = RepositoryId::try_from("api").unwrap();

        assert!(!is_orchestrator(&one, &Scope::Project));
        assert!(is_orchestrator(&two, &Scope::Project));
        assert!(!is_orchestrator(&root, &Scope::Project));
        assert!(!is_orchestrator(&two, &Scope::Repository(api)));
    }
}
