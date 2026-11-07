//! What makes a session an orchestrator: when it is one, the preamble it starts with, the
//! tool calls it may not make, and every prompt Partitur writes to it.

use std::fs;
use std::path::Path;

use crate::backend::{ApprovedChild, ChildOutcome, StopReason, ToolKind, TurnOrigin};
use crate::ids::{RepositoryId, SessionId};
use crate::project::Project;
use crate::session::Scope;

/// The prefix of every tool Partitur's MCP server offers, as the agent names it in a
/// permission request.
pub const PARTITUR_TOOL_PREFIX: &str = "mcp__partitur__";

/// The result an orchestrator's `propose_children` call returns at once, before the user
/// decides the proposal.
pub const PROPOSAL_RECORDED: &str = "proposal recorded, awaiting approval";

/// The instruction file whose first line describes a repository in the preamble.
const INSTRUCTIONS_FILE: &str = "AGENTS.md";

/// What the preamble says about a repository without a usable instruction file.
const NO_INSTRUCTIONS: &str = "(no AGENTS.md)";

/// The preamble between its opening tag and the repository list.
const PREAMBLE_BODY: &str = "You are the orchestrator of this project. You may inspect but never change anything yourself: edit, delete, move and execute requests are denied in this session. Work happens in the repositories, through child sessions you propose.

Tools (MCP server \"partitur\"):
- propose_children(children: [{repository, prompt, title}]): proposes one child session per entry. The proposal is shown to the user for approval; end your turn after calling it. You are woken with the started children and their session ids, or with the rejection and the user's note.
- send_to_child(session_id, prompt): sends a follow-up prompt to a child; it is queued while the child is mid-turn.

Each child's final message reaches you as a prompt marked \"child result\". Report to the user whenever you have nothing further to drive.";

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
        PREAMBLE_BODY.to_owned(),
        String::new(),
        "Repositories:".to_owned(),
    ];
    lines.extend(
        project
            .repositories
            .iter()
            .filter(|repository| !repository.is_root)
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

/// The notice that tells the user an orchestrator's call `title` was denied; `kind` is the
/// word [`restrained`] gave for the call's kind.
pub(crate) fn denial_notice(kind: &str, title: &str) -> String {
    format!(
        "denied: an orchestrator session may inspect but never change; delegate \"{title}\" ({kind}) to a child session"
    )
}

/// The prompt that wakes an orchestrator after the user approved its proposal: one line per
/// started child with its title, repository and session id.
pub(crate) fn children_started_prompt(children: &[ApprovedChild]) -> String {
    let mut lines = vec!["children started:".to_owned()];
    lines.extend(children.iter().map(|child| {
        format!(
            "- {} · {} · session {}",
            child.title, child.repository, child.session
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
    if origin == TurnOrigin::User {
        lines.push("(turn started by the user)".to_owned());
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
            closed: false,
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
        };

        assert_eq!(
            children_started_prompt(&[
                child(&first, &api, "Fix API"),
                child(&second, &web, "Fix web")
            ]),
            format!(
                "children started:\n- Fix API · api · session {first}\n- Fix web · web · session {second}\nEach child's final message will reach you as a prompt marked \"child result\". Use send_to_child(session_id, prompt) for follow-ups."
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
