//! The tools that let a session look at the other sessions of its project, and the text they
//! answer with: the tool specs the preamble lists, one line per session, the note a session
//! started from another's proposal begins with, the prompt that tells the proposer the
//! session started, and a session's transcript as `read_session` returns it.
//!
//! `read_session` renders each transcript entry as one kind of line (`user: …`, `agent: …`,
//! `tool: <title> · <status>`, …) and leaves thoughts, turn markers, tool output and widget
//! content out. The lines, oldest first and separated by a blank line, are packed into pages
//! of at most [`READ_PAGE_CHARS`] characters without splitting an entry; an entry longer than
//! a page is cut to one page ending in `…`. A transcript with nothing to show has one page
//! saying so.

use crate::backend::{ApprovedChild, Decision, ProposedChild, SentAttachment, ToolStatus};
use crate::ids::{RepositoryId, SessionId};
use crate::orchestrator::{ToolSpec, started_line};
use crate::session::{BranchChoice, Scope, Session, Status};
use crate::tool_call::ToolAnswer;
use crate::transcript::Entry;
use crate::workflow;

/// How many characters one page of `read_session` holds at most.
pub(crate) const READ_PAGE_CHARS: usize = 20_000;

/// What separates two entries on a page.
const ENTRY_GAP: &str = "\n\n";

/// The only page of a transcript with nothing to show.
const NO_ENTRIES: &str = "(no entries yet)";

/// The branch settings a plain session's proposed siblings start from: those of the
/// proposer's own workspace, so a sibling branches from the work it follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingDefaults {
    /// The repository the proposer works in, `None` when it has none.
    pub repository: Option<RepositoryId>,
    /// The mode, fetching and base taken from the proposer's workspace, with no branch name.
    pub choice: BranchChoice,
    /// The branch the proposer created, `None` when it works on the checked-out one.
    pub proposer_branch: Option<String>,
}

impl SiblingDefaults {
    /// The branch settings `child` starts from: the proposer's, with the base the agent
    /// suggested when it did. Without a suggestion the proposer's base applies only to a
    /// child in the proposer's own repository, since another repository has no such branch.
    pub fn for_child(&self, child: &ProposedChild) -> BranchChoice {
        let base = child.base.clone().or_else(|| {
            (self.repository.as_ref() == Some(&child.repository))
                .then(|| self.choice.base.clone())
                .flatten()
        });
        BranchChoice {
            base,
            ..self.choice.clone()
        }
    }
}

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

/// The closing sentence of the prompt that tells a session the session it proposed started.
const SESSION_STARTED_FOOTER: &str =
    "It runs on its own and reports nothing back; read it with read_session(session_id).";

/// The note a session started from another's proposal begins its first prompt with: the
/// proposer's `title` and `id`, and how to read the proposer.
pub(crate) fn sibling_note(title: &str, id: &SessionId) -> String {
    format!(
        "Started by session \"{title}\" ({id}). Read it with read_session when you need its context."
    )
}

/// The prompt that wakes a session after the user approved the session it proposed: the
/// `started` session's line, as an orchestrator is told of a started child, and that the new
/// session reports nothing back.
pub(crate) fn session_started_prompt(started: &ApprovedChild) -> String {
    format!(
        "session started:\n{}\n{SESSION_STARTED_FOOTER}",
        started_line(started)
    )
}

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

/// The refusal of `read_session` for `id`, a session that is not one of the caller's
/// project, or not one at all.
pub(crate) fn not_in_project(id: &SessionId) -> String {
    format!("{id} is not a session of this project")
}

/// The line `read_session` shows for `entry`, `None` for an entry it leaves out: a thought,
/// a turn the agent started and a turn's end.
pub(crate) fn entry_text(entry: &Entry) -> Option<String> {
    let text = match entry {
        Entry::User { text, attachments } if attachments.is_empty() => format!("user: {text}"),
        Entry::User { text, attachments } => {
            let names: Vec<&str> = attachments
                .iter()
                .map(|attachment| match attachment {
                    SentAttachment::Image { name, .. } | SentAttachment::File { name, .. } => {
                        name.as_str()
                    }
                })
                .collect();
            format!("user: {text} [attachments: {}]", names.join(", "))
        }
        Entry::Agent(text) => format!("agent: {text}"),
        Entry::Thought(_) | Entry::AgentTurn | Entry::TurnEnded(_) => return None,
        Entry::ToolCall { title, status, .. } => {
            format!("tool: {title} · {}", tool_status_word(*status))
        }
        Entry::Permission {
            title,
            options,
            answer,
            ..
        } => {
            let outcome = match answer {
                None => "open".to_owned(),
                Some(None) => "cancelled".to_owned(),
                Some(Some(chosen)) => options
                    .iter()
                    .find(|option| &option.id == chosen)
                    .map_or_else(|| chosen.to_string(), |option| option.name.clone()),
            };
            format!("permission: {title} · {outcome}")
        }
        Entry::Notice(text) => format!("notice: {text}"),
        Entry::Failed(text) => format!("turn failed: {text}"),
        Entry::Proposal {
            children, decision, ..
        } => {
            let children: Vec<String> = children
                .iter()
                .map(|child| format!("{} ({})", child.title, child.repository))
                .collect();
            let decision = match decision {
                None => "undecided",
                Some(Decision::Approved(_)) => "approved",
                Some(Decision::Rejected { .. }) => "rejected",
            };
            format!("proposed: {} · {decision}", children.join(", "))
        }
        Entry::Question { question, .. } => format!("question: {}", question.text),
        Entry::OrchestratorPrompt(text) => format!("orchestrator: {text}"),
        Entry::PartiturPrompt(text) => format!("partitur: {text}"),
        Entry::ChildResult {
            repository,
            title,
            text,
            ..
        } => format!("child result · {repository} · {title}: {text}"),
        Entry::ChildPrompted { child, prompt, .. } => format!("sent to child {child}: {prompt}"),
        Entry::TitleProposal(proposal) => format!(
            "title proposal: {} (was {})",
            proposal.proposed, proposal.title
        ),
        Entry::Workflow { event, .. } => format!(
            "workflow: {}",
            serde_json::to_string(event).unwrap_or_default()
        ),
    };
    Some(text)
}

/// The word `read_session` shows for a tool call's status.
fn tool_status_word(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::InProgress => "in progress",
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
    }
}

/// `text` cut to one page: when it is longer than [`READ_PAGE_CHARS`] characters, its first
/// characters followed by `…`, [`READ_PAGE_CHARS`] in all.
fn cut(text: String) -> String {
    if text.chars().count() <= READ_PAGE_CHARS {
        return text;
    }
    let mut kept: String = text.chars().take(READ_PAGE_CHARS - 1).collect();
    kept.push('…');
    kept
}

/// The pages of `entries` as `read_session` returns them, oldest first: their lines separated
/// by a blank line, as many to a page as fit in [`READ_PAGE_CHARS`] characters, never one
/// split across pages. There is always at least one page.
pub(crate) fn pages(entries: &[Entry]) -> Vec<String> {
    let gap = ENTRY_GAP.chars().count();
    let mut pages = Vec::new();
    let mut page = String::new();
    let mut length = 0;
    for text in entries.iter().filter_map(entry_text).map(cut) {
        let size = text.chars().count();
        if !page.is_empty() && length + gap + size > READ_PAGE_CHARS {
            pages.push(std::mem::take(&mut page));
            length = 0;
        }
        if !page.is_empty() {
            page.push_str(ENTRY_GAP);
            length += gap;
        }
        page.push_str(&text);
        length += size;
    }
    if !page.is_empty() {
        pages.push(page);
    }
    if pages.is_empty() {
        pages.push(NO_ENTRIES.to_owned());
    }
    pages
}

/// The answer of `read_session` for `page` (the last one when `None`) of the session `id`
/// titled `title`, cut into `pages`: the page under its header
/// `session <id> · <title> · page <n> of <m>`, and, when an earlier page exists, the line
/// naming it. A page out of range is refused, naming the range.
pub(crate) fn read_page(
    id: &SessionId,
    title: &str,
    pages: &[String],
    page: Option<usize>,
) -> ToolAnswer {
    let count = pages.len();
    let number = page.unwrap_or(count);
    let Some(text) = number.checked_sub(1).and_then(|index| pages.get(index)) else {
        return Err(format!(
            "page {number} is out of range; the session has pages 1 to {count}"
        ));
    };
    let header = format!("session {id} · {title} · page {number} of {count}");
    if number > 1 {
        let earlier = number - 1;
        return Ok(format!(
            "{header}\n\n{text}\n\nearlier: read_session(session_id, page: {earlier})"
        ));
    }
    Ok(format!("{header}\n\n{text}"))
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
    use crate::backend::{
        ChildOutcome, ChildState, Decision, PermissionKind, PermissionOption, ProposedChild,
        SentAttachment, StopReason, TitleProposal, ToolKind, ToolStatus, TurnOrigin,
    };
    use crate::ids::{PermissionId, PermissionOptionId, ProposalId, ToolCallId};
    use crate::ids::{ProjectId, QuestionId, RepositoryId};
    use crate::question::Question;
    use crate::session::{BranchMode, TitleSource};
    use crate::workflow::{Decider, ModuleEvent, ModuleState, WorkflowStatus, superpowers_v7};

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

    fn tool(title: &str, status: ToolStatus) -> Entry {
        Entry::ToolCall {
            id: ToolCallId::new("t"),
            title: title.into(),
            kind: ToolKind::Read,
            status,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            call_result: None,
            widget: None,
        }
    }

    fn child(title: &str, repository: &str) -> ProposedChild {
        ProposedChild {
            repository: RepositoryId::try_from(repository).unwrap(),
            prompt: "p".into(),
            title: title.into(),
            title_source: TitleSource::Given,
            base: None,
        }
    }

    fn proposal(children: Vec<ProposedChild>, decision: Option<Decision>) -> Entry {
        Entry::Proposal {
            id: ProposalId::new(),
            children,
            decision,
        }
    }

    #[expect(
        clippy::option_option,
        reason = "the permission entry's own answer: open, cancelled or the option picked"
    )]
    fn permission(answer: Option<Option<PermissionOptionId>>) -> Entry {
        Entry::Permission {
            id: PermissionId(1),
            tool_call_id: ToolCallId::new("t"),
            title: "Edit x".into(),
            kind: ToolKind::Edit,
            options: vec![PermissionOption {
                id: PermissionOptionId::new("allow"),
                name: "Allow once".into(),
                kind: PermissionKind::AllowOnce,
            }],
            answer,
        }
    }

    fn text(entry: &Entry) -> String {
        entry_text(entry).unwrap()
    }

    #[test]
    fn entries_render_as_one_line_kinds() {
        let user = Entry::User {
            text: "fix it".into(),
            attachments: vec![
                SentAttachment::Image {
                    name: "a.png".into(),
                    copy: "c.png".into(),
                    preview: None,
                },
                SentAttachment::File {
                    name: "b.txt".into(),
                    path: "/b.txt".into(),
                },
            ],
        };
        let child_id = SessionId::new();
        let approved = ApprovedChild {
            session: child_id.clone(),
            repository: RepositoryId::try_from("api").unwrap(),
            prompt: "p".into(),
            title: "Fix".into(),
            auto_allow: false,
            mode: BranchMode::default(),
            branch: None,
            cwd: "/w".into(),
        };

        assert_eq!(text(&user), "user: fix it [attachments: a.png, b.txt]");
        assert_eq!(text(&Entry::user("hi")), "user: hi");
        assert_eq!(text(&Entry::Agent("done".into())), "agent: done");
        assert_eq!(entry_text(&Entry::Thought("hm".into())), None);
        assert_eq!(entry_text(&Entry::AgentTurn), None);
        assert_eq!(entry_text(&Entry::TurnEnded(StopReason::EndTurn)), None);
        assert_eq!(
            text(&tool("Read x", ToolStatus::Pending)),
            "tool: Read x · pending"
        );
        assert_eq!(
            text(&tool("Read x", ToolStatus::InProgress)),
            "tool: Read x · in progress"
        );
        assert_eq!(
            text(&tool("Read x", ToolStatus::Completed)),
            "tool: Read x · completed"
        );
        assert_eq!(
            text(&tool("Read x", ToolStatus::Failed)),
            "tool: Read x · failed"
        );
        assert_eq!(text(&Entry::Failed("boom".into())), "turn failed: boom");
        assert_eq!(
            text(&proposal(
                vec![child("Fix", "api"), child("Show", "web")],
                None
            )),
            "proposed: Fix (api), Show (web) · undecided"
        );
        assert_eq!(
            text(&proposal(
                vec![child("Fix", "api")],
                Some(Decision::Approved(vec![approved]))
            )),
            "proposed: Fix (api) · approved"
        );
        assert_eq!(
            text(&proposal(
                vec![child("Fix", "api")],
                Some(Decision::Rejected { note: None })
            )),
            "proposed: Fix (api) · rejected"
        );
        assert_eq!(
            text(&Entry::Question {
                id: QuestionId::new(),
                question: Question {
                    text: "Which one?".into(),
                    options: Vec::new(),
                },
                outcome: None,
                decider: Decider::default(),
            }),
            "question: Which one?"
        );
        assert_eq!(
            text(&Entry::OrchestratorPrompt("t".into())),
            "orchestrator: t"
        );
        assert_eq!(text(&Entry::PartiturPrompt("t".into())), "partitur: t");
        assert_eq!(
            text(&Entry::ChildResult {
                child: child_id.clone(),
                repository: RepositoryId::try_from("api").unwrap(),
                title: "Fix".into(),
                origin: TurnOrigin::User,
                outcome: ChildOutcome::Ended(StopReason::EndTurn),
                text: "all done".into(),
                state: ChildState::default(),
            }),
            "child result · api · Fix: all done"
        );
    }

    #[test]
    fn permissions_notices_and_workflow_steps_render_as_one_line_each() {
        let child_id = SessionId::new();

        assert_eq!(text(&permission(None)), "permission: Edit x · open");
        assert_eq!(
            text(&permission(Some(None))),
            "permission: Edit x · cancelled"
        );
        assert_eq!(
            text(&permission(Some(Some(PermissionOptionId::new("allow"))))),
            "permission: Edit x · Allow once"
        );
        assert_eq!(
            text(&permission(Some(Some(PermissionOptionId::new("gone"))))),
            "permission: Edit x · gone"
        );
        assert_eq!(text(&Entry::Notice("hello".into())), "notice: hello");
        assert_eq!(
            text(&Entry::ChildPrompted {
                child: child_id.clone(),
                prompt: "go on".into(),
                decision: None,
            }),
            format!("sent to child {child_id}: go on")
        );
        assert_eq!(
            text(&Entry::TitleProposal(TitleProposal {
                child: child_id,
                repository: RepositoryId::try_from("api").unwrap(),
                title: "Fix".into(),
                proposed: "Fix login".into(),
                by_user: false,
            })),
            "title proposal: Fix login (was Fix)"
        );
        let workflow = text(&Entry::Workflow {
            at: 1,
            event: ModuleEvent::SuperpowersV7(superpowers_v7::Event::StartBrainstorming).into(),
            row: None,
        });
        assert!(workflow.starts_with("workflow: {"), "{workflow}");
        assert!(workflow.contains("start_brainstorming"), "{workflow}");
        assert!(!workflow.contains('\n'));
    }

    /// An agent entry whose rendered text is `length` characters long.
    fn agent_of(length: usize) -> Entry {
        Entry::Agent("x".repeat(length - "agent: ".len()))
    }

    #[test]
    fn pages_pack_entries_up_to_the_limit() {
        let fits = [agent_of(9_000), agent_of(READ_PAGE_CHARS - 9_002)];
        let over = [agent_of(9_000), agent_of(READ_PAGE_CHARS - 9_001)];
        let three = [
            agent_of(10),
            Entry::Thought("t".into()),
            agent_of(12),
            agent_of(20_000),
        ];

        let packed = pages(&fits);
        assert_eq!(packed.len(), 1);
        assert_eq!(packed[0].chars().count(), READ_PAGE_CHARS);
        assert_eq!(
            packed[0],
            format!("{}\n\n{}", text(&fits[0]), text(&fits[1]))
        );
        assert_eq!(pages(&over), [text(&over[0]), text(&over[1])]);
        assert_eq!(
            pages(&three),
            [
                format!("{}\n\n{}", text(&three[0]), text(&three[2])),
                text(&three[3])
            ]
        );
    }

    #[test]
    fn every_gap_on_a_page_counts_toward_the_limit() {
        let fits = [
            agent_of(6_000),
            agent_of(6_000),
            agent_of(READ_PAGE_CHARS - 12_004),
        ];
        let over = [
            agent_of(6_000),
            agent_of(6_000),
            agent_of(READ_PAGE_CHARS - 12_003),
        ];

        let packed = pages(&fits);
        assert_eq!(packed.len(), 1);
        assert_eq!(packed[0].chars().count(), READ_PAGE_CHARS);
        assert_eq!(
            pages(&over),
            [
                format!("{}\n\n{}", text(&over[0]), text(&over[1])),
                text(&over[2])
            ]
        );
    }

    #[test]
    fn a_longer_entry_is_cut_on_a_char_boundary() {
        let long = pages(&[Entry::Agent("ä".repeat(25_000))]);
        let exact = pages(&[agent_of(READ_PAGE_CHARS)]);

        assert_eq!(long.len(), 1);
        assert_eq!(long[0].chars().count(), READ_PAGE_CHARS);
        assert!(long[0].starts_with("agent: ää"));
        assert!(long[0].ends_with("ä…"));
        assert_eq!(exact, [text(&agent_of(READ_PAGE_CHARS))]);
    }

    #[test]
    fn an_empty_transcript_has_one_page() {
        assert_eq!(pages(&[]), ["(no entries yet)"]);
        assert_eq!(pages(&[Entry::AgentTurn]), ["(no entries yet)"]);
    }

    #[test]
    fn read_page_defaults_to_the_last_page_and_names_the_earlier_one() {
        let id = SessionId::new();
        let three = ["one".to_owned(), "two".to_owned(), "three".to_owned()];

        assert_eq!(
            read_page(&id, "T", &three, None),
            Ok(format!(
                "session {id} · T · page 3 of 3\n\nthree\n\nearlier: read_session(session_id, page: 2)"
            ))
        );
        assert_eq!(
            read_page(&id, "T", &three, Some(2)),
            Ok(format!(
                "session {id} · T · page 2 of 3\n\ntwo\n\nearlier: read_session(session_id, page: 1)"
            ))
        );
        assert_eq!(
            read_page(&id, "T", &three, Some(1)),
            Ok(format!("session {id} · T · page 1 of 3\n\none"))
        );
        assert_eq!(
            read_page(&id, "T", &three, Some(4)),
            Err("page 4 is out of range; the session has pages 1 to 3".to_owned())
        );
        assert_eq!(
            read_page(&id, "T", &three, Some(0)),
            Err("page 0 is out of range; the session has pages 1 to 3".to_owned())
        );
    }

    #[test]
    fn a_session_outside_the_project_is_refused_by_its_id() {
        let id = SessionId::new();

        assert_eq!(
            not_in_project(&id),
            format!("{id} is not a session of this project")
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

    fn sibling_defaults() -> SiblingDefaults {
        SiblingDefaults {
            repository: Some(RepositoryId::try_from("api").unwrap()),
            choice: BranchChoice {
                mode: BranchMode::Worktree,
                branch: String::new(),
                base: Some("main".into()),
                fetch: false,
            },
            proposer_branch: Some("feature-x".into()),
        }
    }

    fn proposed(repository: &str, base: Option<&str>) -> ProposedChild {
        ProposedChild {
            repository: RepositoryId::try_from(repository).unwrap(),
            prompt: "p".into(),
            title: "t".into(),
            title_source: TitleSource::Given,
            base: base.map(str::to_owned),
        }
    }

    #[test]
    fn for_child_prefers_the_agents_base() {
        let defaults = sibling_defaults();

        let suggested = defaults.for_child(&proposed("api", Some("feature-x")));
        assert_eq!(suggested.base.as_deref(), Some("feature-x"));
        assert_eq!(suggested.mode, BranchMode::Worktree);
        assert!(!suggested.fetch);

        let own = defaults.for_child(&proposed("api", None));
        assert_eq!(own.base.as_deref(), Some("main"));
        assert_eq!(own.mode, BranchMode::Worktree);

        let other = defaults.for_child(&proposed("web", None));
        assert_eq!(other.base, None);
        assert_eq!(other.mode, BranchMode::Worktree);
    }

    #[test]
    fn for_child_without_a_proposer_repository_offers_no_base() {
        let mut defaults = sibling_defaults();
        defaults.repository = None;

        assert_eq!(defaults.for_child(&proposed("api", None)).base, None);
    }
}
