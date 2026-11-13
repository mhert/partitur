//! What the user does about the current session's workflow: approving the gate it waits
//! at, asking for changes instead, and reading the design and the plan it reported.
//!
//! Approving sends the gate's fixed approval prompt, from the rail's button, the Spec tab's
//! gate bar or with Enter on the Session and Spec tabs. It is offered only while the
//! session waits at a gate, is not in a turn and is not archived, as the core tells it;
//! otherwise Enter does nothing while a gate is open, so a proposed split is never approved
//! in its place. Without a gate, Enter does on the Session tab what it always does there, and
//! nothing on the Spec tab. Asking for changes shows the Session tab with the prompt box
//! holding the start of the request, such as `Changes to the plan: `, before any draft the
//! box held, the cursor at the end and the focus in the box. The gate closes once that
//! prompt is sent, as it does for any prompt the session is sent.
//!
//! The documents the session reported are read and parsed for the Spec tab on a thread of
//! their own (`document-reader`), so neither a slow disk nor a long document holds up the
//! window. A document is read again whenever the agent reports a new revision of it, or the
//! user asks to refresh it; until the text arrives the document is shown as being read. A
//! path is taken relative to the session's working folder unless it is absolute. A text that
//! arrives for a revision or a path the session no longer reports is dropped. Opening a
//! document in the editor hands it to the desktop's opener, in the background as well.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, io, thread};

use futures::channel::oneshot;
use iced::Task;
use iced::widget::{markdown, operation, text_editor};
use partitur_core::{
    AppError, Document, GateKind, LinkOpener, SessionId, SessionViewModel, Step, WorkflowStatus,
};

use crate::app::{self, Message, State, Tab};
use crate::{link, projects_view, session_view};

/// The documents of a workflow the Spec tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecDoc {
    /// The design document.
    Design,
    /// The implementation plan.
    Plan,
}

impl SpecDoc {
    /// The latest revision of this document that `status` reports, if any.
    pub(crate) fn reported(self, status: &WorkflowStatus) -> Option<&Document> {
        match self {
            Self::Design => status.design.as_ref(),
            Self::Plan => status.plan.as_ref(),
        }
    }

    /// The document's name as the interface says it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Plan => "plan",
        }
    }

    /// The document a gate of `kind` asks the user to approve; `None` for a checkpoint.
    pub(crate) fn of_gate(kind: GateKind) -> Option<Self> {
        match kind {
            GateKind::Design => Some(Self::Design),
            GateKind::Plan => Some(Self::Plan),
            GateKind::Checkpoint => None,
        }
    }
}

/// The document a step works on: the design while brainstorming and designing, the plan
/// from planning on.
pub(crate) fn spec_doc(step: Step) -> SpecDoc {
    match step {
        Step::Brainstorm | Step::Design => SpecDoc::Design,
        Step::Plan | Step::Execute | Step::Finish => SpecDoc::Plan,
    }
}

/// A document as its reader hands it over: its title and its Markdown, parsed already.
#[derive(Debug, Clone)]
pub struct Parsed {
    /// The text it was parsed from.
    text: String,
    /// Its first `# ` heading, or its file name.
    title: String,
    /// Its Markdown without the title's heading.
    items: Vec<markdown::Item>,
}

impl Parsed {
    /// Parses `text`, read from `path`, taking its title out of the Markdown.
    pub(crate) fn new(text: String, path: &Path) -> Self {
        let title = document_title(&text, path);
        let items = markdown::Content::parse(&without_title(&text))
            .items()
            .to_vec();
        Self { text, title, items }
    }
}

/// The parsed Markdown follows from the text and the title, so those two tell documents
/// apart; the parsed items cannot be compared.
impl PartialEq for Parsed {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.title == other.title
    }
}

/// Why a document could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFailure {
    /// There is no file at its path.
    NotFound,
    /// The file is there but cannot be read, for the reason given.
    Other(String),
}

/// An interaction with the current session's workflow.
#[derive(Debug, Clone, PartialEq)]
pub enum FlowMessage {
    /// The user approved the gate the session waits at.
    Approve,
    /// The user asked for changes to what the gate offers instead of approving it.
    RequestChanges,
    /// The user asked to see a document on the Spec tab.
    Open(SpecDoc),
    /// The user picked a document on the Spec tab's outline.
    Select(SpecDoc),
    /// The user asked to read the shown document again.
    Refresh,
    /// The user asked to open the document the gate offers in their editor.
    OpenInEditor,
    /// A document was read in the background.
    Read {
        /// The session that reported the document.
        session: SessionId,
        /// Which of its documents it is.
        doc: SpecDoc,
        /// Where it was read from.
        path: PathBuf,
        /// The revision it was read for.
        rev: u32,
        /// It parsed, or why it could not be read.
        result: Result<Parsed, ReadFailure>,
    },
    /// The user clicked a link in a document.
    LinkClicked(markdown::Uri),
}

/// What the interface remembers of the sessions' documents: which one the Spec tab shows
/// for each session, and each document as it was last read.
#[derive(Debug, Default)]
pub struct FlowState {
    /// The document the user picked per session; without a pick the current step's.
    selected: HashMap<SessionId, SpecDoc>,
    /// Every document read or being read, by session and kind.
    documents: HashMap<(SessionId, SpecDoc), Loaded>,
}

/// A document at the path and revision it is read for.
#[derive(Debug)]
struct Loaded {
    /// Where it is read from.
    path: PathBuf,
    /// The revision it is read for.
    rev: u32,
    /// How far reading it has come.
    state: DocumentState,
}

/// How far reading a document has come.
#[derive(Debug)]
pub(crate) enum DocumentState {
    /// It is being read.
    Loading,
    /// It was read: its title and its parsed text without the title's heading.
    Ready {
        /// Its first `# ` heading, or its file name.
        title: String,
        /// Its Markdown.
        items: Vec<markdown::Item>,
    },
    /// There is no file at its path.
    Missing,
    /// It cannot be read, for the reason given.
    Failed(String),
}

impl FlowState {
    /// The document the Spec tab shows for `session`, whose workflow stands at `status`:
    /// the one the user picked, or else the current step's.
    pub(crate) fn selected(&self, session: &SessionId, status: &WorkflowStatus) -> SpecDoc {
        self.selected
            .get(session)
            .copied()
            .unwrap_or_else(|| status.step.map_or(SpecDoc::Design, spec_doc))
    }

    /// How far reading the document `doc` of `session` has come; `None` before it was
    /// asked for.
    pub(crate) fn document(&self, session: &SessionId, doc: SpecDoc) -> Option<&DocumentState> {
        self.documents
            .get(&(session.clone(), doc))
            .map(|loaded| &loaded.state)
    }

    /// Marks the document `doc` of `session` as being read from `path` for `rev`.
    fn start(&mut self, session: SessionId, doc: SpecDoc, path: PathBuf, rev: u32) {
        let loaded = Loaded {
            path,
            rev,
            state: DocumentState::Loading,
        };
        self.documents.insert((session, doc), loaded);
    }

    /// Takes in the outcome of a read when the document is still asked for at that `path`
    /// and `rev`; drops it otherwise.
    fn finish(
        &mut self,
        key: &(SessionId, SpecDoc),
        path: &Path,
        rev: u32,
        result: Result<Parsed, ReadFailure>,
    ) {
        let Some(loaded) = self.documents.get_mut(key) else {
            return;
        };
        if loaded.path != path || loaded.rev != rev {
            return;
        }
        loaded.state = match result {
            Ok(parsed) => DocumentState::Ready {
                title: parsed.title,
                items: parsed.items,
            },
            Err(ReadFailure::NotFound) => DocumentState::Missing,
            Err(ReadFailure::Other(error)) => DocumentState::Failed(error),
        };
    }
}

/// Applies `message` to the current session and returns the follow-up work: approving
/// scrolls the transcript to the prompt it sent, asking for changes focuses the prompt box,
/// and reading a document or opening it runs in the background. An approval the core
/// refuses is shown in the error bar.
pub fn update(state: &mut State, message: FlowMessage) -> Task<Message> {
    match message {
        FlowMessage::Approve => approve(state),
        FlowMessage::RequestChanges => request_changes(state),
        FlowMessage::Open(doc) => {
            select(state, doc);
            if app::spec_enabled(state) {
                state.tab = Tab::Spec;
            }
            sync(state)
        }
        FlowMessage::Select(doc) => {
            select(state, doc);
            Task::none()
        }
        FlowMessage::Refresh => refresh(state),
        FlowMessage::OpenInEditor => open_in_editor(state),
        FlowMessage::Read {
            session,
            doc,
            path,
            rev,
            result,
        } => {
            state.flow.finish(&(session, doc), &path, rev, result);
            Task::none()
        }
        FlowMessage::LinkClicked(uri) => {
            let opener = state
                .app
                .current_session()
                .ok_or(AppError::NoCurrentSession)
                .and_then(|session| state.app.link_opener(&session));
            match app::report(&mut state.error, opener) {
                Some(opener) => link::open(opener, uri),
                None => Task::none(),
            }
        }
    }
}

/// Starts reading, for the current session, every document it reported whose path and
/// revision are not read or being read yet. Nothing is read here: each read is a task.
pub(crate) fn sync(state: &mut State) -> Task<Message> {
    let Some(model) = state.app.session_view() else {
        return Task::none();
    };
    let Some(workflow) = &model.workflow else {
        return Task::none();
    };
    let mut reads = Vec::new();
    for doc in [SpecDoc::Design, SpecDoc::Plan] {
        let Some(document) = doc.reported(&workflow.status) else {
            continue;
        };
        let path = absolute(&model, document);
        let known = state
            .flow
            .documents
            .get(&(model.id.clone(), doc))
            .is_some_and(|loaded| loaded.path == path && loaded.rev == document.rev);
        if !known {
            state
                .flow
                .start(model.id.clone(), doc, path.clone(), document.rev);
            reads.push(read(model.id.clone(), doc, path, document.rev));
        }
    }
    Task::batch(reads)
}

/// Where the session `model` keeps `document`: its path taken relative to the session's
/// working folder, unless it is absolute.
pub(crate) fn absolute(model: &SessionViewModel<'_>, document: &Document) -> PathBuf {
    model.cwd.join(&document.path)
}

/// The title of the document with the text `text` at `path`: its first `# ` heading, or
/// else its file name.
pub(crate) fn document_title(text: &str, path: &Path) -> String {
    heading(text).map_or_else(
        || {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        },
        |(_, title)| title.to_owned(),
    )
}

/// The first `# ` heading of `text`: the byte range of its line and its title.
fn heading(text: &str) -> Option<(std::ops::Range<usize>, &str)> {
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if let Some(title) = line.trim_end().strip_prefix("# ") {
            return Some((start..start + line.len(), title.trim()));
        }
        start += line.len();
    }
    None
}

/// `text` without the line of its first `# ` heading, which the Spec tab shows as the
/// document's title instead.
fn without_title(text: &str) -> String {
    match heading(text) {
        Some((line, _)) => {
            let (before, after) = (text.get(..line.start), text.get(line.end..));
            format!(
                "{}{}",
                before.unwrap_or_default(),
                after.unwrap_or_default()
            )
        }
        None => text.to_owned(),
    }
}

/// Reads the document `doc` of `session` from `path` for `rev` in the background; the
/// outcome arrives as [`FlowMessage::Read`].
fn read(session: SessionId, doc: SpecDoc, path: PathBuf, rev: u32) -> Task<Message> {
    Task::perform(read_in_background(path.clone()), move |result| {
        Message::Flow(FlowMessage::Read {
            session,
            doc,
            path,
            rev,
            result,
        })
    })
}

/// Reads and parses the file at `path` on a thread of its own and resolves with the parsed
/// document, or why it could not be read.
async fn read_in_background(path: PathBuf) -> Result<Parsed, ReadFailure> {
    let (sender, receiver) = oneshot::channel();
    let started = thread::Builder::new()
        .name("document-reader".into())
        .spawn(move || {
            // The receiver may be gone when the window closed meanwhile.
            let _ = sender.send(read_file(&path).map(|text| Parsed::new(text, &path)));
        });
    if let Err(error) = started {
        return Err(ReadFailure::Other(error.to_string()));
    }
    // The thread always sends before it ends, so a dropped sender means it panicked.
    receiver
        .await
        .unwrap_or_else(|_| Err(ReadFailure::Other("the reader stopped".into())))
}

/// The text of the file at `path`, or why it cannot be read.
fn read_file(path: &Path) -> Result<String, ReadFailure> {
    fs::read_to_string(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ReadFailure::NotFound,
        _ => ReadFailure::Other(error.to_string()),
    })
}

/// Shows `doc` on the Spec tab for the current session.
fn select(state: &mut State, doc: SpecDoc) {
    if let Some(session) = state.app.current_session() {
        state.flow.selected.insert(session, doc);
    }
}

/// Reads the document the Spec tab shows for the current session again.
fn refresh(state: &mut State) -> Task<Message> {
    let Some(model) = state.app.session_view() else {
        return Task::none();
    };
    let Some(workflow) = &model.workflow else {
        return Task::none();
    };
    let doc = state.flow.selected(&model.id, &workflow.status);
    let Some(document) = doc.reported(&workflow.status) else {
        return Task::none();
    };
    let path = absolute(&model, document);
    state
        .flow
        .start(model.id.clone(), doc, path.clone(), document.rev);
    read(model.id.clone(), doc, path, document.rev)
}

/// Opens the document the current session's gate offers with the desktop's opener, in the
/// background; nothing happens without such a document.
fn open_in_editor(state: &mut State) -> Task<Message> {
    let path = state.app.session_view().and_then(|model| {
        let status = &model.workflow.as_ref()?.status;
        let doc = SpecDoc::of_gate(status.gate.as_ref()?.kind)?;
        Some(absolute(&model, doc.reported(status)?))
    });
    path.map_or_else(Task::none, |path| {
        link::open(LinkOpener::default(), path.display().to_string())
    })
}

/// What G does on every tab when no widget took the key: it opens the Spec tab of the
/// session that has waited longest at a gate, wherever it is, and does nothing when no
/// session waits at one.
pub(crate) fn next_gate(state: &mut State) -> Task<Message> {
    let Some(session) = state.app.next_gate() else {
        return Task::none();
    };
    if app::report(&mut state.error, state.app.select_session(session)).is_none() {
        return Task::none();
    }
    projects_view::show_current_session(state);
    state.tab = Tab::Spec;
    sync(state)
}

/// What Enter does on the Session tab when no widget took the key: while a gate is open it
/// approves the gate when it can be approved and does nothing otherwise, so a proposed split
/// is never approved in its place; without a gate it does what Enter does on the tab.
pub(crate) fn press_enter(state: &mut State) -> Task<Message> {
    let gate_open = state
        .app
        .current_workflow()
        .is_some_and(|status| status.gate.is_some());
    if !gate_open {
        session_view::press_enter(state)
    } else if state.app.approvable_gate().is_some() {
        approve(state)
    } else {
        Task::none()
    }
}

/// What Enter does on the Spec tab when no widget took the key: it approves the gate when
/// it can be approved, and does nothing otherwise. A proposed split is never approved from
/// here, since only the Session tab shows it.
pub(crate) fn press_enter_on_spec(state: &mut State) -> Task<Message> {
    if state.app.approvable_gate().is_some() {
        approve(state)
    } else {
        Task::none()
    }
}

/// Approves the current session's gate, then shows the transcript's end.
fn approve(state: &mut State) -> Task<Message> {
    let Some(session) = state.app.current_session() else {
        return Task::none();
    };
    if app::report(&mut state.error, state.app.approve_gate(session)).is_none() {
        return Task::none();
    }
    session_view::to_the_end(&mut state.session)
}

/// Shows the Session tab with the start of a request for changes to the current session's
/// gate in the prompt box, its cursor at the end and the focus in the box. A draft the box
/// holds already is kept behind that start, or as it is when it begins with it. Without a
/// gate nothing changes.
fn request_changes(state: &mut State) -> Task<Message> {
    let kind = state.app.session_view().and_then(|model| {
        let gate = model.workflow.as_ref()?.status.gate.as_ref()?;
        Some(gate.kind)
    });
    let Some(kind) = kind else {
        return Task::none();
    };
    state.tab = Tab::Session;
    let prefix = kind.change_prefix();
    let draft = state.session.prompt.text();
    let text = if draft.starts_with(prefix) {
        draft
    } else {
        format!("{prefix}{draft}")
    };
    let mut prompt = text_editor::Content::with_text(&text);
    prompt.perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    state.session.prompt = prompt;
    operation::focus(session_view::PROMPT)
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

    use iced_test::simulator;
    use partitur_core::testing::Call;
    use partitur_core::{
        BackendEvent, PartiturTool, PlannedTask, PromptContent, ProposalId, ProposedChild,
        RepositoryId, SessionId, SuperpowersEvent, Update,
    };

    use super::*;
    use crate::app::{BareKey, update as apply, view};
    use crate::test_support::{
        Plain, deliver, deliver_tool, end_turn, orchestrated, plain, read_message,
    };

    /// Feeds the superpowers report `event` of `session` through the application.
    fn report(state: &mut State, session: &SessionId, event: SuperpowersEvent) {
        deliver_tool(state, session.clone(), PartiturTool::Superpowers(event));
    }

    /// The plan of `count` tasks, task `n` titled `Task n`.
    fn tasks(count: u32) -> Vec<PlannedTask> {
        (1..=count)
            .map(|n| PlannedTask {
                n,
                title: format!("Task {n}"),
                files: Vec::new(),
            })
            .collect()
    }

    /// Asks for approval of a plan of eleven tasks.
    fn request_plan(state: &mut State, session: &SessionId) {
        report(
            state,
            session,
            SuperpowersEvent::RequestPlanApproval {
                path: PathBuf::from("plans/p.md"),
                tasks: tasks(11),
                checkpoints: vec![3, 6],
            },
        );
    }

    /// The texts of every prompt `backend` was sent.
    fn prompts(fixture: &Plain) -> Vec<String> {
        fixture
            .backend
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Prompt { content, .. } => Some(content),
                _ => None,
            })
            .map(|content| {
                content
                    .into_iter()
                    .filter_map(|part| match part {
                        PromptContent::Text(text) => Some(text),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .collect()
    }

    /// Has `orchestrator` propose a split of one child and returns the proposal's id.
    fn propose_split(state: &mut State, orchestrator: &SessionId) -> ProposalId {
        let id = ProposalId::new();
        deliver(
            state,
            BackendEvent::Update {
                session: orchestrator.clone(),
                update: Update::ChildrenProposed {
                    id: id.clone(),
                    children: vec![ProposedChild {
                        repository: RepositoryId::try_from("web").unwrap(),
                        prompt: "Add the page".into(),
                        title: "Web page".into(),
                        title_source: partitur_core::TitleSource::Given,
                    }],
                },
            },
        );
        id
    }

    /// A plain session waiting at the plan's gate, its turn ended, on the Session tab.
    fn waiting_at_the_plan() -> Plain {
        let mut fixture = plain();
        let session = fixture.session.clone();
        request_plan(&mut fixture.state, &session);
        end_turn(&mut fixture.state, &session);
        fixture.state.tab = Tab::Session;
        fixture
    }

    #[test]
    fn the_rail_shows_the_steps_and_the_current_task() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        request_plan(state, &session);
        end_turn(state, &session);
        let _task = update(state, FlowMessage::Approve);
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Subagent,
            },
        );
        report(
            state,
            &session,
            SuperpowersEvent::StartTask { n: 4, note: None },
        );

        let mut ui = simulator(view(state));
        ui.find("superpowers").unwrap();
        for step in ["brainstorm", "design", "plan", "execute", "finish"] {
            ui.find(step).unwrap();
        }
        ui.find("task 4 of 11").unwrap();
        ui.find("Task 4").unwrap();
        assert!(ui.find("tdd green").is_err());

        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::MakeTestPass { n: 4, note: None },
        );
        let mut ui = simulator(view(state));
        ui.find("tdd green").unwrap();
    }

    #[test]
    fn the_rail_names_the_phase_of_the_current_task() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        report(
            state,
            &session,
            SuperpowersEvent::WriteFailingTest { n: 2, note: None },
        );
        // Before the plan is executed, the rail names no task's phase.
        let mut ui = simulator(view(state));
        assert!(ui.find("tdd red").is_err());
        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Inline,
            },
        );
        let mut ui = simulator(view(state));
        ui.find("tdd red").unwrap();

        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::StartRefactoring { n: 2, note: None },
        );
        let mut ui = simulator(view(state));
        ui.find("tdd refactor").unwrap();
    }

    #[test]
    fn the_rail_sums_up_the_plan_and_tells_the_outcome() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        report(state, &session, SuperpowersEvent::StartWritingPlan);
        request_plan(state, &session);
        // The gate's detail stands in for the plan's summary while it waits.
        let mut ui = simulator(view(state));
        assert!(ui.find("11 tasks · 2 checkpoints").is_err());
        drop(ui);
        end_turn(state, &session);
        let _task = update(state, FlowMessage::RequestChanges);
        state.app.submit_prompt("Fewer tasks", Vec::new()).unwrap();
        let mut ui = simulator(view(state));
        ui.find("11 tasks · 2 checkpoints").unwrap();
        assert!(ui.find("· waiting for your approval").is_err());

        drop(ui);
        report(
            state,
            &session,
            SuperpowersEvent::FinishSession {
                outcome: partitur_core::Outcome::PrOpened,
            },
        );
        let mut ui = simulator(view(state));
        ui.find("PR opened").unwrap();
    }

    #[test]
    fn the_rail_sums_up_a_plan_without_checkpoints_by_its_tasks() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        report(
            state,
            &session,
            SuperpowersEvent::RequestPlanApproval {
                path: PathBuf::from("plans/p.md"),
                tasks: tasks(12),
                checkpoints: Vec::new(),
            },
        );
        end_turn(state, &session);
        state.app.submit_prompt("Fewer tasks", Vec::new()).unwrap();

        let mut ui = simulator(view(state));

        ui.find("12 tasks").unwrap();
    }

    #[test]
    fn a_session_without_a_workflow_has_no_rail() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;

        let mut ui = simulator(view(&fixture.state));

        assert!(ui.find("superpowers").is_err());
        drop(ui);
        fixture.state.tab = Tab::Env;
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("superpowers").is_err());
    }

    #[test]
    fn a_waiting_gate_offers_approve_and_enter_sends_it() {
        let mut fixture = waiting_at_the_plan();

        let mut ui = simulator(view(&fixture.state));
        ui.find("· waiting for your approval").unwrap();
        ui.find("Enter").unwrap();
        ui.click("Approve plan").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::Approve)]);
        for message in messages {
            let _task = apply(&mut fixture.state, message);
        }
        assert_eq!(
            prompts(&fixture).last().unwrap(),
            "Plan approved. Execute it."
        );
        assert_eq!(fixture.state.error, None);
        let mut ui = simulator(view(&fixture.state));
        assert!(ui.find("Approve plan").is_err());

        let mut fixture = waiting_at_the_plan();
        let _task = apply(&mut fixture.state, Message::Key(BareKey::Enter));
        assert_eq!(
            prompts(&fixture).last().unwrap(),
            "Plan approved. Execute it."
        );
    }

    #[test]
    fn approve_is_disabled_while_the_turn_runs() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        fixture.state.tab = Tab::Session;
        report(
            &mut fixture.state,
            &session,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        let before = fixture.backend.calls();

        let mut ui = simulator(view(&fixture.state));
        ui.find("· waiting for your approval").unwrap();
        ui.click("Approve design").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        let _task = apply(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(fixture.backend.calls(), before);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn enter_prefers_the_gate_over_an_undecided_split() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        let id = propose_split(state, &orchestrator);
        report(
            state,
            &orchestrator,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        end_turn(state, &orchestrator);

        let _task = apply(state, Message::Key(BareKey::Enter));

        let model = state.app.session_view().unwrap();
        assert!(model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::Proposal { id: shown, decision: None, .. } if *shown == id
        )));
        assert!(model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::User(text)
                if text == "Design approved. Write the implementation plan."
        )));
    }

    #[test]
    fn enter_leaves_an_undecided_split_alone_while_a_gate_waits_in_the_turn() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        let id = propose_split(state, &orchestrator);
        report(
            state,
            &orchestrator,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        end_turn(state, &orchestrator);
        state.app.submit_prompt("Go on", Vec::new()).unwrap();
        report(
            state,
            &orchestrator,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        assert!(state.app.current_workflow().unwrap().gate.is_some());
        assert!(state.app.approvable_gate().is_none());

        let _task = apply(state, Message::Key(BareKey::Enter));

        let model = state.app.session_view().unwrap();
        assert!(model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::Proposal { id: shown, decision: None, .. } if *shown == id
        )));
        assert!(!model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::User(text)
                if text == "Design approved. Write the implementation plan."
        )));
    }

    #[test]
    fn enter_acts_on_an_undecided_split_once_the_gate_closed() {
        let mut fixture = orchestrated();
        let orchestrator = fixture.orchestrator.clone();
        let state = &mut fixture.state;
        state.tab = Tab::Session;
        let id = propose_split(state, &orchestrator);
        report(
            state,
            &orchestrator,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("docs/design.md"),
            },
        );
        end_turn(state, &orchestrator);
        state.app.submit_prompt("Go on", Vec::new()).unwrap();
        end_turn(state, &orchestrator);
        assert!(state.app.current_workflow().unwrap().gate.is_none());

        let _task = apply(state, Message::Key(BareKey::Enter));

        let model = state.app.session_view().unwrap();
        assert!(model.entries.iter().any(|entry| matches!(
            entry,
            partitur_core::Entry::Proposal { id: shown, decision: Some(_), .. } if *shown == id
        )));
    }

    #[test]
    fn request_changes_keeps_a_typed_draft_behind_the_prefix() {
        for (draft, expected) in [
            ("Fewer tasks", "Changes to the plan: Fewer tasks"),
            (
                "Changes to the plan: Fewer tasks",
                "Changes to the plan: Fewer tasks",
            ),
        ] {
            let mut fixture = waiting_at_the_plan();
            fixture.state.session.prompt = text_editor::Content::with_text(draft);

            let _task = apply(
                &mut fixture.state,
                Message::Flow(FlowMessage::RequestChanges),
            );

            assert_eq!(fixture.state.session.prompt.text(), expected);
            assert_eq!(
                fixture.state.session.prompt.cursor().position,
                text_editor::Position {
                    line: 0,
                    column: expected.len()
                }
            );
        }
    }

    #[test]
    fn request_changes_prefills_and_focuses_the_prompt() {
        let mut fixture = waiting_at_the_plan();
        fixture.state.tab = Tab::Env;

        let _task = apply(
            &mut fixture.state,
            Message::Flow(FlowMessage::RequestChanges),
        );

        assert_eq!(fixture.state.tab, Tab::Session);
        assert_eq!(fixture.state.session.prompt.text(), "Changes to the plan: ");
        assert_eq!(
            fixture.state.session.prompt.cursor().position,
            text_editor::Position {
                line: 0,
                column: "Changes to the plan: ".len()
            }
        );
    }

    #[test]
    fn request_changes_without_a_gate_changes_nothing() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Env;

        let _task = update(&mut fixture.state, FlowMessage::RequestChanges);

        assert_eq!(fixture.state.tab, Tab::Env);
        assert_eq!(fixture.state.session.prompt.text(), "");
    }

    #[test]
    fn the_env_tab_shows_the_rail_too() {
        let mut fixture = waiting_at_the_plan();
        fixture.state.tab = Tab::Env;

        let mut ui = simulator(view(&fixture.state));

        ui.find("superpowers").unwrap();
        ui.find("· waiting for your approval").unwrap();
        ui.click("Approve plan").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::Approve)]);
    }

    #[test]
    fn an_archived_session_cannot_be_approved() {
        let mut fixture = waiting_at_the_plan();
        let session = fixture.session.clone();
        fixture.state.app.archive(session).unwrap();
        let before = fixture.backend.calls();

        let mut ui = simulator(view(&fixture.state));
        ui.click("Approve plan").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
        let _task = apply(&mut fixture.state, Message::Key(BareKey::Enter));

        assert_eq!(fixture.backend.calls(), before);
    }

    #[test]
    fn a_refused_approval_shows_in_the_error_bar() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;

        let _task = update(&mut fixture.state, FlowMessage::Approve);

        assert!(fixture.state.error.is_some());
    }

    /// The cache entry of the current session's document `doc`: its path, its revision and
    /// whether it is still being read.
    fn entry(state: &State, doc: SpecDoc) -> (PathBuf, u32, bool) {
        let session = state.app.current_session().unwrap();
        let loaded = &state.flow.documents[&(session, doc)];
        let loading = matches!(loaded.state, DocumentState::Loading);
        (loaded.path.clone(), loaded.rev, loading)
    }

    #[test]
    fn reading_a_file_maps_missing_and_unreadable() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("plan.md");
        std::fs::write(&file, "# Plan\n").unwrap();

        assert_eq!(read_file(&file), Ok("# Plan\n".to_owned()));
        assert_eq!(
            read_file(&dir.path().join("gone.md")),
            Err(ReadFailure::NotFound)
        );
        assert!(matches!(read_file(dir.path()), Err(ReadFailure::Other(_))));

        // A report only asks for the read: the entry waits until its result arrives.
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        state
            .app
            .call_tool(partitur_core::ToolCall {
                session: session.clone(),
                tool: PartiturTool::Superpowers(SuperpowersEvent::RequestDesignApproval {
                    path: PathBuf::from("docs/design.md"),
                }),
                reply: partitur_core::testing::RecordingReply::new(),
            })
            .unwrap();
        let task = sync(state);
        assert!(task.units() > 0);
        let cwd = state.app.session_view().unwrap().cwd;
        assert_eq!(
            entry(state, SpecDoc::Design),
            (cwd.join("docs/design.md"), 1, true)
        );
        // Asked again for the same revision, nothing is read twice.
        assert_eq!(sync(state).units(), 0);
    }

    #[test]
    fn a_file_is_read_and_parsed_on_a_thread_of_its_own() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("design.md");
        std::fs::write(&file, "# Design\nBody").unwrap();

        let parsed = futures::executor::block_on(read_in_background(file.clone())).unwrap();
        assert_eq!(parsed, Parsed::new("# Design\nBody".to_owned(), &file));
        // The thread hands the document over parsed, its title taken out of the Markdown.
        assert_eq!(parsed.title, "Design");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(
            futures::executor::block_on(read_in_background(dir.path().join("gone"))),
            Err(ReadFailure::NotFound)
        );
    }

    #[test]
    fn an_absolute_path_is_read_where_it_is() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        report(
            &mut fixture.state,
            &session,
            SuperpowersEvent::RequestDesignApproval {
                path: PathBuf::from("/srv/specs/design.md"),
            },
        );

        assert_eq!(
            entry(&fixture.state, SpecDoc::Design).0,
            PathBuf::from("/srv/specs/design.md")
        );
    }

    #[test]
    fn a_stale_read_is_dropped() {
        let mut fixture = waiting_at_the_plan();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        let rev_one = read_message(state, SpecDoc::Plan, Ok("# Old\n".into()));

        request_plan(state, &session);
        let _task = apply(state, rev_one);

        let (_, rev, loading) = entry(state, SpecDoc::Plan);
        assert_eq!((rev, loading), (2, true));

        // A read from another path is dropped as well.
        let Message::Flow(FlowMessage::Read {
            session, doc, rev, ..
        }) = read_message(state, SpecDoc::Plan, Ok(String::new()))
        else {
            unreachable!()
        };
        let elsewhere = Message::Flow(FlowMessage::Read {
            session,
            doc,
            path: PathBuf::from("/elsewhere.md"),
            rev,
            result: Ok(Parsed::new(
                "# Elsewhere\n".into(),
                Path::new("/elsewhere.md"),
            )),
        });
        let _task = apply(state, elsewhere);
        assert!(entry(state, SpecDoc::Plan).2);

        let current = read_message(state, SpecDoc::Plan, Ok("# New\n".into()));
        let _task = apply(state, current);
        assert!(!entry(state, SpecDoc::Plan).2);
    }

    #[test]
    fn a_read_for_a_document_never_asked_for_is_dropped() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;

        let _task = update(
            state,
            FlowMessage::Read {
                session: session.clone(),
                doc: SpecDoc::Plan,
                path: PathBuf::from("/p.md"),
                rev: 1,
                result: Ok(Parsed::new("# P\n".into(), Path::new("/p.md"))),
            },
        );

        assert!(state.flow.document(&session, SpecDoc::Plan).is_none());
    }

    #[test]
    fn refreshing_reads_the_shown_document_again() {
        let mut fixture = waiting_at_the_plan();
        let state = &mut fixture.state;
        let read = read_message(state, SpecDoc::Plan, Ok("# Plan\n".into()));
        let _task = apply(state, read);
        assert!(!entry(state, SpecDoc::Plan).2);

        let task = update(state, FlowMessage::Refresh);

        assert!(task.units() > 0);
        assert!(entry(state, SpecDoc::Plan).2);

        // A document not reported yet has nothing to refresh.
        let mut fixture = plain();
        let session = fixture.session.clone();
        report(
            &mut fixture.state,
            &session,
            SuperpowersEvent::StartBrainstorming,
        );
        assert_eq!(update(&mut fixture.state, FlowMessage::Refresh).units(), 0);
        let mut fixture = plain();
        assert_eq!(update(&mut fixture.state, FlowMessage::Refresh).units(), 0);
    }

    #[test]
    fn a_document_is_titled_by_its_first_heading() {
        let path = Path::new("/work/plans/p.md");

        assert_eq!(document_title("Intro\n# The plan \nBody", path), "The plan");
        assert_eq!(document_title("## Sub\nBody", path), "p.md");
        assert_eq!(document_title("", Path::new("/")), "/");
        assert_eq!(without_title("Intro\n# The plan\nBody"), "Intro\nBody");
        assert_eq!(without_title("# The plan"), "");
        assert_eq!(without_title("Body"), "Body");
    }

    #[test]
    fn open_in_editor_runs_off_the_update_loop() {
        let mut fixture = waiting_at_the_plan();
        let state = &mut fixture.state;
        let before = (state.tab, state.error.clone(), entry(state, SpecDoc::Plan));

        let task = update(state, FlowMessage::OpenInEditor);

        assert!(task.units() > 0);
        assert_eq!(
            (state.tab, state.error.clone(), entry(state, SpecDoc::Plan)),
            before
        );

        // Without a gate there is no document to open.
        let mut fixture = plain();
        assert_eq!(
            update(&mut fixture.state, FlowMessage::OpenInEditor).units(),
            0
        );
    }

    #[test]
    fn a_link_in_a_document_opens_in_the_background() {
        let mut fixture = waiting_at_the_plan();

        let task = update(
            &mut fixture.state,
            FlowMessage::LinkClicked("https://example.com".into()),
        );

        assert!(task.units() > 0);
        assert_eq!(fixture.state.error, None);
    }

    #[test]
    fn a_rail_step_opens_its_document() {
        let mut fixture = waiting_at_the_plan();
        let session = fixture.session.clone();

        let mut ui = simulator(view(&fixture.state));
        ui.click("design").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(
            messages,
            [Message::Flow(FlowMessage::Open(SpecDoc::Design))]
        );
        for message in messages {
            let _task = apply(&mut fixture.state, message);
        }
        assert_eq!(fixture.state.tab, Tab::Spec);
        let status = fixture
            .state
            .app
            .session_view()
            .unwrap()
            .workflow
            .unwrap()
            .status;
        assert_eq!(
            fixture.state.flow.selected(&session, &status),
            SpecDoc::Design
        );

        // While the plan is executed, the rail links to it.
        let state = &mut fixture.state;
        let _task = update(state, FlowMessage::Approve);
        report(
            state,
            &session,
            SuperpowersEvent::StartExecution {
                mode: partitur_core::ExecutionMode::Inline,
            },
        );
        report(
            state,
            &session,
            SuperpowersEvent::StartTask { n: 1, note: None },
        );
        state.tab = Tab::Session;
        let mut ui = simulator(view(state));
        ui.click("open plan").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Flow(FlowMessage::Open(SpecDoc::Plan))]);
        for message in messages {
            let _task = apply(state, message);
        }
        let status = state.app.session_view().unwrap().workflow.unwrap().status;
        assert_eq!(
            (state.tab, state.flow.selected(&session, &status)),
            (Tab::Spec, SpecDoc::Plan)
        );
    }

    #[test]
    fn opening_a_document_without_a_workflow_stays_on_the_tab() {
        let mut fixture = plain();
        fixture.state.tab = Tab::Session;

        let _task = update(&mut fixture.state, FlowMessage::Open(SpecDoc::Plan));

        assert_eq!(fixture.state.tab, Tab::Session);
    }

    #[test]
    fn the_default_document_is_the_current_steps() {
        let mut fixture = plain();
        let session = fixture.session.clone();
        let state = &mut fixture.state;
        let shown = |state: &State| {
            let status = state.app.session_view().unwrap().workflow.unwrap().status;
            state.flow.selected(&session, &status)
        };
        report(
            state,
            &session,
            SuperpowersEvent::StartSession {
                topic: "Topic".into(),
            },
        );
        assert_eq!(shown(state), SpecDoc::Design);
        for (event, doc) in [
            (SuperpowersEvent::StartBrainstorming, SpecDoc::Design),
            (SuperpowersEvent::StartWritingPlan, SpecDoc::Plan),
            (SuperpowersEvent::StartFinishingBranch, SpecDoc::Plan),
        ] {
            report(state, &session, event);
            assert_eq!(shown(state), doc);
        }
        assert_eq!(spec_doc(Step::Design), SpecDoc::Design);
        assert_eq!(spec_doc(Step::Execute), SpecDoc::Plan);
    }
}
