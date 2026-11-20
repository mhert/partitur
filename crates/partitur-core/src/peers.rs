//! The tools that let a session look at the other sessions of its project, and the text they
//! answer with: the tool specs the preamble lists, and one line per session.

use crate::ids::SessionId;
use crate::orchestrator::ToolSpec;
use crate::session::{Scope, Session, Status};
use crate::workflow;

/// The tools Partitur's tool server offers every session about its project's sessions, in the
/// order they are listed.
pub const PEER_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "propose_session",
        signature: "propose_session(title, prompt, repository?, base?)",
        description: "proposes one new session in this project for work that should run on its own, such as a hand-off. The user approves it on a card before it starts; end your turn after calling it. You are woken with its session id, or with the rejection and the user's note. repository defaults to yours; base suggests the branch a new branch starts from.",
    },
    ToolSpec {
        name: "list_sessions",
        signature: "list_sessions()",
        description: "lists this project's sessions, archived ones included, with their title, repository, session id and status. Use it to find a session to pick up from or learn from.",
    },
    ToolSpec {
        name: "read_session",
        signature: "read_session(session_id, page?)",
        description: "returns a session's conversation as text, a page at a time: the user's prompts, the agent's messages and one line per tool call. Without page it returns the last page; each page names the one before it.",
    },
];

/// The word the session lists show for a session's status.
pub(crate) fn status_word(status: Status) -> &'static str {
    match status {
        Status::Idle => "idle",
        Status::Running => "running",
        Status::WaitingForYou => "waiting for you",
        Status::Interrupted => "interrupted",
        Status::Done => "done",
        Status::Failed => "failed",
    }
}

/// The line `list_sessions` shows for `session`:
/// `- <title> · <repository> · session <id> · <status>`, where the repository slot reads
/// `orchestrator` for an orchestrator and `project` for any other session of the whole
/// project. It goes on with the workflow step, then what the session waits for (`waits at its
/// <gate>` for the open `gate`'s word, `asks a question` for an open question), then `child of
/// <parent>`, `archived` and, when it is the `caller`'s own, `this session`.
pub(crate) fn session_line(session: &Session, caller: &SessionId, gate: Option<&str>) -> String {
    let place = match &session.scope {
        Scope::Repository(repository) => repository.to_string(),
        Scope::Project if session.orchestrator => "orchestrator".to_owned(),
        Scope::Project => "project".to_owned(),
    };
    let mut parts = vec![
        format!("- {}", session.title),
        place,
        format!("session {}", session.id),
        status_word(session.status).to_owned(),
    ];
    if let Some(status) = &session.workflow {
        parts.push(workflow::step_cell(status).label);
    }
    if session.question.is_some() {
        parts.push("asks a question".to_owned());
    } else if let Some(word) = gate {
        parts.push(format!("waits at its {word}"));
    }
    if let Some(parent) = &session.parent {
        parts.push(format!("child of {parent}"));
    }
    if session.archived.is_some() {
        parts.push("archived".to_owned());
    }
    if &session.id == caller {
        parts.push("this session".to_owned());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use crate::archive::Archived;
    use crate::ids::{ProjectId, QuestionId, RepositoryId};
    use crate::workflow::{ModuleState, WorkflowStatus, superpowers_v7};

    fn session(title: &str, scope: Scope) -> Session {
        let json = format!(
            r#"{{
                "id": "{}",
                "project": "{}",
                "scope": "Project",
                "cwd": "/work",
                "title": "{title}",
                "backend_session": null,
                "backend": "claude",
                "model": null,
                "effort": null,
                "status": "Idle",
                "parent": null,
                "auto_allow": false,
                "instructions_injected": false,
                "created_at": 1
            }}"#,
            SessionId::new(),
            ProjectId::new()
        );
        let mut session: Session = serde_json::from_str(&json).unwrap();
        session.scope = scope;
        session
    }

    fn api() -> Scope {
        Scope::Repository(RepositoryId::try_from("api").unwrap())
    }

    #[test]
    fn a_session_line_names_title_repository_id_and_status() {
        let live = session("Fix login", api());

        assert_eq!(
            session_line(&live, &SessionId::new(), None),
            format!("- Fix login · api · session {} · idle", live.id)
        );
    }

    #[test]
    fn a_session_line_marks_orchestrator_child_archived_and_self() {
        let mut orchestrator = session("Plan", Scope::Project);
        orchestrator.orchestrator = true;
        let mut child = session("Fix", api());
        child.parent = Some(orchestrator.id.clone());
        child.archived = Some(Archived { at: 5 });

        let line = session_line(&orchestrator, &SessionId::new(), None);
        assert_eq!(
            line,
            format!("- Plan · orchestrator · session {} · idle", orchestrator.id)
        );
        assert_eq!(
            session_line(&child, &child.id, None),
            format!(
                "- Fix · api · session {} · idle · child of {} · archived · this session",
                child.id, orchestrator.id
            )
        );
    }

    #[test]
    fn a_session_line_adds_step_and_wait() {
        let mut gated = session("Plan", api());
        let status = WorkflowStatus {
            applied: 1,
            updated_at: 1,
            gate: None,
            state: ModuleState::SuperpowersV7(superpowers_v7::State::default()),
        };
        let step = workflow::step_cell(&status).label;
        gated.workflow = Some(status);
        let mut asking = session("Ask", api());
        asking.question = Some(QuestionId::new());

        assert_eq!(
            session_line(&gated, &SessionId::new(), Some("design")),
            format!(
                "- Plan · api · session {} · idle · {step} · waits at its design",
                gated.id
            )
        );
        assert_eq!(
            session_line(&asking, &SessionId::new(), None),
            format!(
                "- Ask · api · session {} · idle · asks a question",
                asking.id
            )
        );
    }

    #[test]
    fn a_project_scope_session_reads_project() {
        let whole = session("Look", Scope::Project);

        assert_eq!(
            session_line(&whole, &SessionId::new(), None),
            format!("- Look · project · session {} · idle", whole.id)
        );
    }
}
