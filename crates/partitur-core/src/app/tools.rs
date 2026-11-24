//! How Partitur answers the tool calls of its sessions: each call is checked against the
//! session that makes it, handled, and answered through its reply. An orchestrator proposes
//! children, prompts them, lists them, cancels them and reads their last result; a session that
//! may not use a tool is told why, and nothing changes. Any session may show the user a
//! drawing with `show_svg` or `show_html`: the app queues it for the UI when the session is the
//! selected one and answers that it kept it otherwise.
//!
//! The child list and a child's last result name where the child stands in its workflow, as
//! its child results do. The orchestrator decides for a waiting child with `send_to_child`: a
//! decision approves the child's gate or asks for changes, and a prompt answers its question;
//! either is recorded as the orchestrator's.

use super::live::AfterLoad;
use super::scheduler::{Generated, NO_FINAL_MESSAGE};
use super::split::checked_children;
use super::{App, AppError};
use crate::backend::{GateDecision, OrchestratorTool, ProposedChild, TurnOrigin};
use crate::drawing::{Drawing, EMPTY, KEPT, MAX_DRAWING_BYTES, SHOWN, TOO_LARGE};
use crate::ids::SessionId;
use crate::orchestrator::{PROPOSAL_RECORDED, state_line, state_lines};
use crate::peers::status_word;
use crate::question::QuestionOutcome;
use crate::record::TranscriptRecord;
use crate::session::{Scope, Session, Status, TitleSource};
use crate::tool_call::{PartiturTool, PeerTool, SessionTool, ToolCall, ToolRole};
use crate::workflow::{self, Closing, Decider, GateCloser};

/// The answer to a tool call from a session that does not exist or is not attached.
const UNKNOWN_SESSION: &str = "unknown session";

/// The answer to a tool call from a session that was deleted.
pub(super) const DELETED_SESSION: &str = "the session was deleted";

/// The answer to a tool call from a session that is not an orchestrator.
const NOT_AN_ORCHESTRATOR: &str = "this session is not an orchestrator";

/// The answer to a `propose_title` call from a session that is not a child.
const NOT_A_CHILD: &str = "this session is not a child";

/// The answer to a `rename_session` call from a child.
const CHILD_PROPOSES_TITLES: &str =
    "a child cannot rename itself; propose a title with propose_title";

/// The answer to a `propose_session` call from an orchestrator.
const ORCHESTRATOR_PROPOSES_CHILDREN: &str =
    "an orchestrator proposes children with propose_children";

/// The answer to a `propose_session` call from a child.
const CHILD_CANNOT_START: &str = "a child cannot start sessions; ask your orchestrator";

/// The answer to a `send_to_child` call with a blank prompt.
const NO_PROMPT: &str = "send_to_child needs a prompt";

impl App {
    /// Handles a session's tool call: validates it against the core's state, records
    /// what it changes and answers through the call's reply. Never fails on a bad call: the
    /// caller gets the error text.
    ///
    /// A call whose caller stopped waiting, checked when the call is taken up, changes
    /// nothing. A call from a session that does not exist or is not attached is answered with
    /// `unknown session`, one from a session that is not an orchestrator with `this session
    /// is not an orchestrator` when it asks for an orchestrator's tool or names a session to
    /// rename, `rename_session` from a child with the hint to propose a title instead, and
    /// `propose_title` from a session that is not a child with `this session is not a child`;
    /// none of them changes anything. `show_svg` and `show_html` from any session answer `the
    /// drawing is empty` or `the drawing is larger than 2 MB` for a bad drawing, `shown to the
    /// user` when the session is the selected one and `kept in the session; ...` otherwise. A call whose handling fails is answered with the failure's text
    /// before the failure is returned, so the caller does not wait for an answer that never
    /// comes.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Backend`] when a prompt cannot be queued.
    pub(super) fn tool_call_received(&mut self, call: ToolCall) -> Result<(), AppError> {
        if !call.reply.is_waiting() {
            return Ok(());
        }
        if self.tool_refusal(&call.session, &call.tool).is_none()
            && let Some(unloaded) = self.unloaded_for(&call.session, &call.tool)
        {
            return self.with_transcript(&unloaded, AfterLoad::Tool(call));
        }
        let ToolCall {
            session,
            tool,
            reply,
        } = call;
        let handled = match (self.tool_refusal(&session, &tool), tool) {
            (Some(refusal), _) => Ok(Err(refusal.to_owned())),
            (None, PartiturTool::Workflow(event)) => self.workflow_reported(&session, event),
            (None, PartiturTool::AskUser(question)) => self.ask(&session, question),
            (None, PartiturTool::Show(drawing)) => Ok(self.show(&session, drawing)),
            (
                None,
                PartiturTool::Session(SessionTool::Rename {
                    title,
                    session: target,
                }),
            ) => self.rename_by_agent(&session, target.as_ref(), &title),
            (None, PartiturTool::Session(SessionTool::ProposeTitle { title })) => {
                self.propose_title(&session, &title)
            }
            (None, PartiturTool::Peers(tool)) => {
                match self.peer_tool(&session, tool, &reply).transpose() {
                    Some(handled) => handled,
                    None => return Ok(()),
                }
            }
            (None, PartiturTool::Orchestrator(tool)) => match tool {
                OrchestratorTool::ProposeChildren(children) => self.propose(&session, children),
                OrchestratorTool::SendToChild {
                    child,
                    prompt,
                    decision,
                } => self.send_to_child(&session, &child, prompt, decision),
                OrchestratorTool::ListChildren => self.list_children(&session).map(Ok),
                OrchestratorTool::CancelChild { child } => self.cancel_child(&session, &child),
                OrchestratorTool::GetChildResult { child } => {
                    self.get_child_result(&session, &child)
                }
            },
        };
        match handled {
            Ok(answer) => {
                reply.send(answer);
                Ok(())
            }
            Err(error) => {
                reply.send(Err(error.to_string()));
                Err(error)
            }
        }
    }

    /// Handles `show_svg` and `show_html`: a blank or oversized drawing is refused; otherwise
    /// the drawing is queued for the UI when `session` is the selected one, and a newer one
    /// replaces a queued one. A session the user is not looking at keeps its drawing in the
    /// transcript only.
    fn show(&mut self, session: &SessionId, drawing: Drawing) -> Result<String, String> {
        if drawing.markup.trim().is_empty() {
            return Err(EMPTY.to_owned());
        }
        if drawing.markup.len() > MAX_DRAWING_BYTES {
            return Err(TOO_LARGE.to_owned());
        }
        if self.current_session().as_ref() == Some(session) {
            self.drawing = Some(drawing);
            Ok(SHOWN.to_owned())
        } else {
            Ok(KEPT.to_owned())
        }
    }

    /// Why a call of `tool` from `session` is refused before looking at it, or `None` when it
    /// is taken up. Only an orchestrator may call an orchestrator's tool, report on a whole run
    /// or rename another session; every other report about the workflow is taken from every
    /// session. A child never renames itself.
    fn tool_refusal(&self, session: &SessionId, tool: &PartiturTool) -> Option<&'static str> {
        if let Some(refusal) = self.unknown_caller(session) {
            return Some(refusal);
        }
        let orchestrators_only = match tool {
            PartiturTool::Orchestrator(_) => true,
            PartiturTool::Workflow(event) => event.run_level(),
            PartiturTool::AskUser(_)
            | PartiturTool::Show(_)
            | PartiturTool::Peers(PeerTool::ListSessions | PeerTool::ReadSession { .. }) => false,
            PartiturTool::Session(SessionTool::Rename {
                session: target, ..
            }) => {
                if self
                    .session(session)
                    .is_ok_and(|caller| caller.tool_role() == ToolRole::Child)
                {
                    return Some(CHILD_PROPOSES_TITLES);
                }
                target.is_some()
            }
            PartiturTool::Session(SessionTool::ProposeTitle { .. }) => {
                return self
                    .session(session)
                    .is_ok_and(|caller| caller.tool_role() != ToolRole::Child)
                    .then_some(NOT_A_CHILD);
            }
            PartiturTool::Peers(PeerTool::ProposeSession { .. }) => {
                return match self.session(session).map(Session::tool_role) {
                    Ok(ToolRole::Orchestrator) => Some(ORCHESTRATOR_PROPOSES_CHILDREN),
                    Ok(ToolRole::Child) => Some(CHILD_CANNOT_START),
                    _ => None,
                };
            }
        };
        (orchestrators_only
            && self
                .session(session)
                .is_ok_and(|caller| !caller.role.is_orchestrator()))
        .then_some(NOT_AN_ORCHESTRATOR)
    }

    /// Why a call from `session` is refused whatever it asks for: the session was deleted, or
    /// does not exist or is not attached.
    fn unknown_caller(&self, session: &SessionId) -> Option<&'static str> {
        if self.deleted.contains(session) {
            Some(DELETED_SESSION)
        } else if self.session(session).is_ok() && self.attached(session) {
            None
        } else {
            Some(UNKNOWN_SESSION)
        }
    }

    /// `propose_children`: records the proposal in the orchestrator's transcript and among the
    /// undecided proposals, and answers at once without waiting for the user. A blank title is
    /// derived from the child's prompt.
    ///
    /// The call is refused, and nothing recorded, when it proposes no child, names a
    /// repository that is not one of the project's (the project root is none) or gives a
    /// child a blank prompt.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] or [`AppError::UnknownProject`] when the session or its
    /// project does not exist.
    fn propose(
        &mut self,
        session: &SessionId,
        children: Vec<ProposedChild>,
    ) -> Result<Result<String, String>, AppError> {
        let project = self.project(&self.session(session)?.project)?;
        let children = match checked_children(project, children) {
            Ok(children) => children,
            Err(refusal) => return Ok(Err(refusal)),
        };
        self.record_proposal(session, children)?;
        Ok(Ok(PROPOSAL_RECORDED.to_owned()))
    }

    /// `send_to_child`: prompts the orchestrator's child `child`, decides the gate it waits
    /// at or answers its question, and records in the orchestrator's transcript which child it
    /// prompted with what, and with which decision.
    ///
    /// A blank prompt is refused before the child is looked at, unless it comes with an
    /// approval, and so is a session that does not exist or is not a child of `session`. A
    /// plain prompt to a child with neither a gate nor a question open goes out now or is
    /// queued until the child is free, as [`deliver`](Self::deliver) decides. Every other call
    /// is refused while the child is still in its turn, and otherwise:
    ///
    /// - With a question open (which takes precedence over a gate), the prompt answers it, as
    ///   the user's answer does, and closes the gate too; a decision is refused.
    /// - At a gate, an approval sends the gate's approval text, followed by the prompt after a
    ///   blank line when it is not blank, and closes the gate as approved; a request for changes
    ///   sends the gate's change prefix and the prompt, and closes the gate as a prompt does.
    ///   No decision is refused.
    /// - A decision for a child with no open gate is refused.
    /// - A decision, or a prompt that would answer a question, is refused when the child's
    ///   wait is not the one its last `child waits` told the orchestrator about: the child
    ///   moved on to a newer gate or question, whose own `child waits` reaches the
    ///   orchestrator, so a stale decision never closes a wait it has not seen.
    ///
    /// The orchestrator is recorded as the one that decided. A child that takes no prompt
    /// (archived, or its backend gone) refuses a decision or an answer with the reason. A
    /// refused call records nothing.
    ///
    /// # Errors
    ///
    /// As [`deliver`](Self::deliver).
    fn send_to_child(
        &mut self,
        session: &SessionId,
        child: &SessionId,
        prompt: String,
        decision: Option<GateDecision>,
    ) -> Result<Result<String, String>, AppError> {
        if prompt.trim().is_empty() && decision != Some(GateDecision::Approve) {
            return Ok(Err(NO_PROMPT.to_owned()));
        }
        if let Err(refusal) = self.check_child(session, child) {
            return Ok(Err(refusal));
        }
        let question = self.question_open(child);
        let gate = self.gate_open(child);
        let sent = if decision.is_none() && !question && gate.is_none() {
            self.deliver(child, Generated::FromOrchestrator(prompt.clone()))?
        } else {
            if self.in_turn(child)? {
                return Ok(Err(format!("child {child} is still in its turn")));
            }
            if self.wait_mark(child) != self.session(child)?.wait_reported {
                if question {
                    return Ok(Err(format!(
                        "child {child} now asks another question; it reaches you as a child \
                         waits prompt"
                    )));
                }
                if let Some((module, kind)) = &gate {
                    let word = workflow::gate_texts(*module, kind).word;
                    return Ok(Err(format!(
                        "child {child} now waits at its {word}; that wait reaches you as a \
                         child waits prompt"
                    )));
                }
            }
            let (text, by) = match (question, gate, decision) {
                (true, _, None) => (prompt.clone(), GateCloser::Prompt),
                (true, _, Some(_)) => {
                    return Ok(Err(format!(
                        "child {child} asks a question; answer it with a prompt and no decision"
                    )));
                }
                (false, None, _) => return Ok(Err(format!("child {child} has no open gate"))),
                (false, Some((module, kind)), None) => {
                    let word = workflow::gate_texts(module, &kind).word;
                    return Ok(Err(format!(
                        "child {child} waits at its {word}; send decision \"approve\" or \
                         \"changes\""
                    )));
                }
                (false, Some((module, kind)), Some(GateDecision::Approve)) => {
                    let approval = workflow::gate_texts(module, &kind).approval_text;
                    let text = if prompt.trim().is_empty() {
                        approval.to_owned()
                    } else {
                        format!("{approval}\n\n{prompt}")
                    };
                    (text, GateCloser::Approve)
                }
                (false, Some((module, kind)), Some(GateDecision::Changes)) => {
                    let prefix = workflow::gate_texts(module, &kind).change_prefix;
                    (format!("{prefix}{prompt}"), GateCloser::Prompt)
                }
            };
            if let Err(refusal) = self.check_promptable(child) {
                return Ok(Err(refusal.to_string()));
            }
            self.start_turn(
                child,
                &[TranscriptRecord::OrchestratorPrompt(text.clone())],
                text,
                Vec::new(),
                TurnOrigin::Orchestrator,
                Some(Closing {
                    by,
                    decider: Decider::Orchestrator,
                }),
            )?;
            if question {
                self.close_question(
                    child,
                    QuestionOutcome::Answered(prompt.clone()),
                    Decider::Orchestrator,
                )?;
            }
            true
        };
        let prompted = TranscriptRecord::ChildPrompted {
            child: child.clone(),
            prompt,
            decision,
        };
        self.apply_update(session, &prompted)?;
        let answer = if sent {
            format!("prompt sent to child {child}")
        } else {
            format!("prompt queued for child {child}; it goes out when the child is free")
        };
        Ok(Ok(answer))
    }

    /// `cancel_child`: stops the current turn of `child` when it is in a turn or has a prompt
    /// that has not gone out; any other child is reported as not running.
    ///
    /// # Errors
    ///
    /// As [`stop_turn`](Self::stop_turn).
    fn cancel_child(
        &mut self,
        session: &SessionId,
        child: &SessionId,
    ) -> Result<Result<String, String>, AppError> {
        if let Err(refusal) = self.check_child(session, child) {
            return Ok(Err(refusal));
        }
        if self.in_turn(child)? || self.has_queued(child) {
            self.stop_turn(child.clone())?;
            return Ok(Ok(format!(
                "cancel requested for child {child}; its result follows as a child result"
            )));
        }
        let status = status_word(self.session(child)?.status);
        Ok(Ok(format!("child {child} is not running ({status})")))
    }

    /// `get_child_result`: the last final message of `child` with its status and, for a child
    /// with a workflow or a wait, the lines of its [`child_state`](Self::child_state) under
    /// the header; or a note that the child is still working or has not answered yet.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::UnknownSession`] when the child does not exist.
    fn get_child_result(
        &mut self,
        session: &SessionId,
        child: &SessionId,
    ) -> Result<Result<String, String>, AppError> {
        if let Err(refusal) = self.check_child(session, child) {
            return Ok(Err(refusal));
        }
        let status = status_word(self.session(child)?.status);
        if self.in_turn(child)? || self.has_queued(child) {
            return Ok(Ok(format!(
                "child {child} is still working ({status}); its result follows as a child result"
            )));
        }
        let text = self.final_message_of(child);
        let header = std::iter::once(format!("child {child} · {status}"))
            .chain(state_lines(&self.child_state(child)?))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Ok(if text != NO_FINAL_MESSAGE {
            format!("{header}\n\n{text}")
        } else if self.session(child)?.status == Status::Failed {
            format!("child {child} failed without a final message")
        } else {
            format!("child {child} has no result yet ({status})")
        }))
    }

    /// The first session whose transcript the answer to `tool`, called by `caller`, reads and
    /// that is not loaded: the child of `get_child_result`, a child of `list_children` with a
    /// question open, and the parent `propose_title` delivers to, whose undecided proposals
    /// decide how the turn the proposal starts ends. The call waits for that transcript.
    fn unloaded_for(&self, caller: &SessionId, tool: &PartiturTool) -> Option<SessionId> {
        let read: Vec<SessionId> = match tool {
            PartiturTool::Orchestrator(OrchestratorTool::GetChildResult { child }) => self
                .check_child(caller, child)
                .map_or_else(|_| Vec::new(), |()| vec![child.clone()]),
            PartiturTool::Orchestrator(OrchestratorTool::ListChildren) => self
                .sessions
                .iter()
                .filter(|child| child.role.parent() == Some(caller) && child.question.is_some())
                .map(|child| child.id.clone())
                .collect(),
            PartiturTool::Session(SessionTool::ProposeTitle { .. }) => self
                .session(caller)
                .ok()
                .and_then(|child| child.role.parent())
                .filter(|parent| self.session(parent).is_ok())
                .into_iter()
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        read.into_iter()
            .find(|session| self.transcript(session).is_none())
    }

    /// Whether `child` is a session of this app whose parent is the orchestrator `session`;
    /// otherwise the refusal text for the tool call.
    pub(super) fn check_child(&self, session: &SessionId, child: &SessionId) -> Result<(), String> {
        let Ok(target) = self.session(child) else {
            return Err(format!("unknown child {child}"));
        };
        if target.role.parent() != Some(session) {
            return Err(format!("{child} is not a child of this session"));
        }
        Ok(())
    }

    /// `list_children`: one line per direct child of the orchestrator `session`, in session-list
    /// order, with its title, repository, session id and status, then the [`state_line`] of a
    /// child with a workflow or a wait, and `set by you` after a title the user typed; a note
    /// when it has none.
    ///
    /// # Errors
    ///
    /// As [`child_state`](Self::child_state).
    fn list_children(&mut self, session: &SessionId) -> Result<String, AppError> {
        let children: Vec<(SessionId, String, &'static str)> = self
            .sessions
            .iter()
            .filter_map(|child| match (child.role.parent(), &child.scope) {
                (Some(parent), Scope::Repository(repository)) if parent == session => {
                    let by_user = if child.title_source == TitleSource::Given {
                        " · set by you"
                    } else {
                        ""
                    };
                    let line = format!(
                        "- {} · {repository} · session {} · {}",
                        child.title,
                        child.id,
                        status_word(child.status)
                    );
                    Some((child.id.clone(), line, by_user))
                }
                _ => None,
            })
            .collect();
        if children.is_empty() {
            return Ok("no children yet".to_owned());
        }
        let mut lines = Vec::new();
        for (child, line, by_user) in children {
            let state = state_line(&self.child_state(&child)?)
                .map_or_else(String::new, |state| format!(" · {state}"));
            lines.push(format!("{line}{state}{by_user}"));
        }
        Ok(format!("children:\n{}", lines.join("\n")))
    }
}

#[cfg(test)]
mod tests;
