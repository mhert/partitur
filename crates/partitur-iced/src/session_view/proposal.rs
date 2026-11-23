//! The proposed split: an undecided proposal of children as a table, editable before it is
//! decided.
//!
//! Every undecided proposal of the current session gets a [`ProposalDraft`] as soon as it
//! shows up (see [`sync_drafts`]). The card lists the draft's children, one row each: the
//! repository, the title over the prompt and the child's branch mode and name, and when the
//! child starts. The project's repositories without a child follow as muted `skipped` rows.
//! A click on a child's row turns it into a form that edits the title, the prompt, the
//! auto-allow switch and the child's branch settings (mode, branch name, base branch and
//! whether the base is fetched first, with the worktree folder they lead to) or removes the
//! child; a click on a skipped row adds a child for that repository and opens its form.
//! `Edit split` opens every form at once and adds a picker that offers each of the project's
//! repositories for a further child. Under the card, `Approve split` (or Enter) starts the
//! draft's children, and `Reject` sends the optional rejection note to the orchestrator.
//!
//! Every child, also one added on the card, starts from the branch settings the orchestrator
//! keeps for its children, and a dim `from orchestrator` marks each value that still equals
//! the orchestrator's. The core checks the children in their own repositories when the draft
//! is made, after every edit of their branch settings and after every git answer, never while
//! drawing, since a check can ask git again. A row says `loading…` until git has described
//! its repository, and else why the child cannot start, if it cannot, in the failure colour.
//! `Approve split` and Enter do nothing while any child is loading or has a problem; while one
//! has a problem, a line over the buttons names the first such child's repository and its
//! problem, and how many more children have one, so the reason shows even when that row is
//! scrolled out of view.
//!
//! While the approval's git work is in flight the card is locked: Approve reads `approving…`,
//! and no field, row, picker or button of the card takes input, nor does Enter; the rest of
//! the window works as usual. The approval holds its repositories busy meanwhile, which the
//! card does not show as a problem. Why an approval failed shows under the card, which is
//! editable again then.
//!
//! A plain session's proposal of one sibling session is the same card with one row, headed
//! `Proposed session` and without `skipped` rows, `Edit split`, the picker that adds a child or
//! the button that removes one: the user rejects it instead. Its Approve button reads
//! `Approve`. The row starts from the proposer's branch settings (its mode and fetch choice,
//! the base the agent suggested, else the proposer's own base in the proposer's repository,
//! and no branch name), and no value is marked `from orchestrator`. In the proposer's
//! repository the base picker marks the branch the proposer works on `(proposer's branch)`.
//! Once approved, the decision shows as the same one-row card headed `Proposed session`, whose
//! row opens the session that started.

use std::fmt;
use std::path::Path;

use iced::widget::{
    Column, Row, button, column, container, pick_list, row, space, text, text_editor, text_input,
    toggler,
};
use iced::{Alignment, Border, Element, Length, Task, Theme, border};
use partitur_core::{
    App, AppError, Approval, BranchChoice, BranchMode, ChildCheck, ChildChoice, Decision, Entry,
    ProposalId, ProposedChild, RepositoryId, RepositoryNode, SessionId, SessionViewModel,
};

use super::entries::LINE;
use super::{SessionMessage, repository_name, sync_markdown, with_chevron};
use crate::app::{self, Message, State};
use crate::branch_controls::{
    PICKER_PADDING, PICKER_WIDTH, base_picker, fetch_toggle, mode_label, mode_switch,
};
use crate::ellipsis::keep_start;
use crate::icon::{Icon, icon};
use crate::projects_view::ProjectsMessage;
use crate::{badge, home_path, style, theme};

/// The placeholder of the picker that adds a child.
const ADD_CHILD: &str = "add child";

/// The placeholder of the note that goes to the orchestrator with a rejection.
const REJECTION_NOTE: &str = "rejection note (optional)";

/// What the card's header says about its rows.
const HINT: &str = "click a row to edit its brief";

/// What stands for the prompt of a child that has none yet.
const NO_BRIEF: &str = "no brief yet";

/// The title of a split's card.
const SPLIT_TITLE: &str = "Proposed split";

/// The title of a proposed session's card.
const SIBLING_TITLE: &str = "Proposed session";

/// What a row says when its child starts on approval.
const STARTS_NOW: &str = "starts now";

/// What the row of a proposed session says once it started.
const STARTED: &str = "started";

/// What the header of an approved session's card says about its row.
const OPEN_HINT: &str = "click the row to open the session";

/// What the header of a proposed session's card says about it.
const SIBLING_SUMMARY: &str = "1 new session";

/// What the header of a proposed session's card says once it started.
const DECIDED_SIBLING_SUMMARY: &str = "1 session started";

/// What marks a branch setting that still equals the orchestrator's.
const FROM_ORCHESTRATOR: &str = "from orchestrator";

/// What a row says while git has not described the child's repository yet.
const LOADING: &str = "loading…";

/// The modes a child can pick, in the order of the switch.
const MODES: [BranchMode; 3] = [
    BranchMode::Worktree,
    BranchMode::NewBranch,
    BranchMode::CheckedOut,
];

/// The width of the column that names the repository, in logical pixels.
const REPOSITORY_WIDTH: f32 = 110.0;

/// The width of the column that says when a child starts.
const STARTS_WIDTH: f32 = 150.0;

/// The width of the labels in front of a child's branch settings.
const BRANCH_LABEL_WIDTH: f32 = 90.0;

/// The space between two columns of the table.
const COLUMN_GAP: f32 = 14.0;

/// The space between the card and the buttons that decide it.
const ACTIONS_GAP: f32 = 14.0;

/// The most characters of a repository's name its column shows.
const REPOSITORY_BUDGET: usize = 14;

/// The corner radius of what touches the card's border from inside: the card's own, less
/// the border.
const INNER_RADIUS: f32 = style::CARD_RADIUS - 1.0;

/// An undecided proposal as the user is editing it on its card.
#[derive(Debug)]
pub struct ProposalDraft {
    /// The children to start, in order.
    pub(crate) children: Vec<DraftChild>,
    /// The note that goes to the orchestrator with a rejection.
    pub(crate) note: String,
    /// Which children show their form.
    pub(crate) editing: Editing,
    /// How each child would start, in order, as the core checked them last; why the core
    /// could not check them otherwise.
    pub(crate) checks: Result<Vec<ChildCheck>, String>,
}

/// One child on the approval card.
#[derive(Debug)]
pub struct DraftChild {
    /// The repository the child works in.
    pub(crate) repository: RepositoryId,
    /// The child's display title; a blank one is derived from the prompt.
    pub(crate) title: String,
    /// The child's first prompt.
    pub(crate) prompt: text_editor::Content,
    /// Whether the child grants permission requests without asking.
    pub(crate) auto_allow: bool,
    /// How the child uses its repository's git.
    pub(crate) branch: BranchChoice,
}

/// Which children of a draft show the form that edits them instead of their row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Editing {
    /// No child: each one is a row.
    #[default]
    Nothing,
    /// The child at this position, whose row the user clicked.
    Row(usize),
    /// Every child, after `Edit split`.
    All,
}

impl Editing {
    /// Whether the child at `index` shows its form.
    fn shows(self, index: usize) -> bool {
        match self {
            Self::Nothing => false,
            Self::Row(row) => row == index,
            Self::All => true,
        }
    }
}

/// An interaction on an approval card; the child it concerns is given by its position.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalMessage {
    /// The user edited a child's prompt.
    EditPrompt(usize, text_editor::Action),
    /// The user edited a child's title.
    EditTitle(usize, String),
    /// The user switched a child's auto-allow on or off.
    SetAutoAllow(usize, bool),
    /// The user picked a child's branch mode.
    SetMode(usize, BranchMode),
    /// The user edited the name of a child's new branch.
    EditBranch(usize, String),
    /// The user picked the branch a child's new branch starts from.
    SetBase(usize, String),
    /// The user switched fetching a child's base first on or off.
    SetFetch(usize, bool),
    /// The user removed a child.
    Remove(usize),
    /// The user added a child in this repository.
    Add(RepositoryId),
    /// The user clicked a child's row, which becomes its form.
    OpenRow(usize),
    /// The user left the form of the one child that showed it.
    CloseRow,
    /// The user pressed `Edit split`: every child shows its form, or none any more.
    ToggleEditAll,
    /// The user edited the rejection note.
    EditNote(String),
    /// The user approved the proposal with the draft's children.
    Approve,
    /// The user rejected the proposal with the draft's note.
    Reject,
}

impl ProposalDraft {
    /// A draft of the proposed `children` as the orchestrator proposed them, each granting
    /// permission requests without asking when `auto_allow` and starting from the branch
    /// settings `branch` gives it, without a note, with every child shown as a row and not
    /// checked yet.
    pub(crate) fn from_entry(
        children: &[ProposedChild],
        auto_allow: bool,
        branch: impl Fn(&ProposedChild) -> BranchChoice,
    ) -> Self {
        let children = children
            .iter()
            .map(|child| DraftChild {
                repository: child.repository.clone(),
                title: child.title.clone(),
                prompt: text_editor::Content::with_text(&child.prompt),
                auto_allow,
                branch: branch(child),
            })
            .collect();
        Self {
            children,
            note: String::new(),
            editing: Editing::default(),
            checks: Ok(Vec::new()),
        }
    }

    /// The children to approve, with their prompts and titles trimmed.
    pub(crate) fn choices(&self) -> Vec<ChildChoice> {
        self.children
            .iter()
            .map(|child| ChildChoice {
                repository: child.repository.clone(),
                prompt: child.prompt.text().trim().to_owned(),
                title: child.title.trim().to_owned(),
                auto_allow: child.auto_allow,
                branch: child.branch.clone(),
            })
            .collect()
    }

    /// Lets the core check the children as they are now, as the children of the proposal
    /// `id` of the orchestrator `session`; returns whether the proposal is still undecided
    /// and its orchestrator still there, so the draft is still wanted. A check the user
    /// caused, `by_user`, reads the repositories git failed to read again; any other keeps
    /// what git said.
    fn check(
        &mut self,
        app: &mut App,
        session: &SessionId,
        id: &ProposalId,
        by_user: bool,
    ) -> bool {
        let choices = self.choices();
        let checked = if by_user {
            app.check_split(session, id, &choices)
        } else {
            app.recheck_split(session, id, &choices)
        };
        let wanted = !matches!(
            checked,
            Err(AppError::UnknownProposal(_) | AppError::UnknownSession(_))
        );
        self.checks = checked.map_err(|error| error.to_string());
        wanted
    }

    /// What the last check said about the child at `index`.
    fn check_of(&self, index: usize) -> Option<&ChildCheck> {
        self.checks
            .as_ref()
            .ok()
            .and_then(|checks| checks.get(index))
    }

    /// Whether the split can be approved: it has children, and the last check found every
    /// one of them read and without a problem.
    fn approvable(&self) -> bool {
        let ready = |checks: &Vec<ChildCheck>| {
            checks.len() == self.children.len()
                && checks
                    .iter()
                    .all(|check| !check.loading && check.problem.is_none())
        };
        !self.children.is_empty() && self.checks.as_ref().is_ok_and(ready)
    }

    /// The first child the last check found a problem with, as its position and the
    /// problem, and how many children have one in all; `None` when no child has one.
    fn blocker(&self) -> Option<(usize, &str, usize)> {
        let checks = self.checks.as_ref().ok()?;
        let mut problems = checks
            .iter()
            .enumerate()
            .filter_map(|(index, check)| Some((index, check.problem.as_deref()?)));
        let (index, problem) = problems.next()?;
        Some((index, problem, problems.count().saturating_add(1)))
    }

    /// The note to send with a rejection; none when it is blank.
    fn rejection_note(&self) -> Option<String> {
        let note = self.note.trim();
        (!note.is_empty()).then(|| note.to_owned())
    }
}

/// Creates a draft for every undecided proposal of the current session that has none yet,
/// its children granting permission requests without asking when the session does and
/// starting from the session's branch settings for its children, or, for a proposed
/// sibling, from the proposer's settings for its siblings, and checks it.
///
/// Called from [`sync_markdown`], which runs after every backend event, selection and new
/// session, so the view finds a draft for every pending proposal.
pub(crate) fn sync_drafts(state: &mut State) {
    let Some(model) = state.app.session_view() else {
        return;
    };
    let defaults = model.child_defaults.clone().unwrap_or_default();
    let branch = |child: &ProposedChild| match &model.siblings {
        Some(siblings) => siblings.for_child(child),
        None => defaults.clone(),
    };
    let mut made = Vec::new();
    for entry in model.entries {
        if let Entry::Proposal {
            id,
            children,
            decision: None,
        } = entry
        {
            let key = (model.id.clone(), id.clone());
            if !state.session.proposals.contains_key(&key) {
                let draft = ProposalDraft::from_entry(children, model.auto_allow, branch);
                state.session.proposals.insert(key.clone(), draft);
                made.push(key);
            }
        }
    }
    for (session, id) in made {
        if let Some(draft) = state
            .session
            .proposals
            .get_mut(&(session.clone(), id.clone()))
        {
            // A draft made just now is for an undecided proposal of the current session.
            let _wanted = draft.check(&mut state.app, &session, &id, false);
        }
    }
}

/// Lets the core check the children of every draft again, which takes in what git answered
/// and how the sessions changed since; called after every git answer and after every
/// change of the sessions. A draft whose proposal was decided meanwhile, by an approval
/// that waited for git, or whose orchestrator is gone, is dropped.
pub(crate) fn refresh_checks(state: &mut State) {
    let app = &mut state.app;
    state
        .session
        .proposals
        .retain(|(session, id), draft| draft.check(app, session, id, false));
}

/// Whether the approval of the proposal `id` of the session `model` is in flight, which
/// locks its card.
fn approving(model: &SessionViewModel<'_>, id: &ProposalId) -> bool {
    model
        .approval
        .as_ref()
        .is_some_and(|approval| approval.proposal == *id && approval.pending)
}

/// The split card of the proposal `id` of the session `model`, editing `draft`, over the
/// buttons that decide the proposal; the card of a proposed session for a plain `model`.
pub(crate) fn card<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    draft: &'a ProposalDraft,
) -> Element<'a, Message> {
    let locked = approving(model, id);
    let sibling = model.siblings.is_some();
    let defaults = model.child_defaults.as_ref();
    let taken: Vec<&RepositoryId> = draft
        .children
        .iter()
        .map(|child| &child.repository)
        .collect();
    let skipped = skipped(model, &taken);
    let editing_all = draft.editing == Editing::All && !sibling;
    // The last row touches the card's rounded corners, unless the picker follows it.
    let last = draft
        .children
        .len()
        .saturating_add(skipped.len())
        .saturating_sub(1);
    let rounded = |position: usize| position == last && !editing_all;
    let children = draft.children.iter().enumerate().map(|(index, child)| {
        let name = repository_name(model, &child.repository);
        let check = draft.check_of(index);
        if draft.editing.shows(index) {
            let edit = Edit {
                id,
                index,
                check,
                defaults,
                proposer: proposer_branch(model, &child.repository),
                removable: !sibling,
                locked,
            };
            form(&edit, &name, child, !editing_all)
        } else {
            let open = (!locked).then(|| message(id, ProposalMessage::OpenRow(index)));
            let row = child_row(
                &name,
                &child.title,
                &child.prompt.text(),
                Some(branch_summary(&child.branch, check, locked)),
                STARTS_NOW,
                open,
                rounded(index),
            );
            container(row)
                .id(widget(id, &format!("row-{index}")))
                .into()
        }
    });
    let skipped_rows = skipped.iter().enumerate().map(|(offset, repository)| {
        let add = (!locked).then(|| message(id, ProposalMessage::Add(repository.id.clone())));
        let position = draft.children.len().saturating_add(offset);
        let row = skipped_row(&repository.name, add, rounded(position));
        container(row)
            .id(widget(id, &format!("skipped-{}", repository.id)))
            .into()
    });
    let picker = editing_all.then(|| add_row(model, id, locked));
    let header = header(model, draft.children.len(), skipped.len());
    let rows = children.chain(skipped_rows).chain(picker);
    let mut card = column![table(header, Some(HINT), rows)].spacing(ACTIONS_GAP);
    let failed = model
        .approval
        .as_ref()
        .filter(|approval| approval.proposal == *id && !approval.pending)
        .and_then(|approval| approval.error.clone());
    // A locked card's own repositories are busy, which is no reason to show.
    let blocked = draft
        .blocker()
        .filter(|_| !locked)
        .map(|(index, problem, count)| {
            let name = draft
                .children
                .get(index)
                .map(|child| repository_name(model, &child.repository))
                .unwrap_or_default();
            match count {
                1 => format!("Cannot approve: {name}: {problem}"),
                more => format!(
                    "Cannot approve: {name}: {problem} (and {} more)",
                    more.saturating_sub(1)
                ),
            }
        });
    for failure in [failed, draft.checks.as_ref().err().cloned(), blocked]
        .into_iter()
        .flatten()
    {
        card = card.push(text(failure).size(theme::SMALL).style(failed_ink));
    }
    card.push(actions(id, draft, locked, sibling)).into()
}

/// The split card of an undecided proposal of `children` that has no draft: the same
/// table, which takes no clicks and offers no decision.
pub(super) fn read_only<'a>(
    model: &SessionViewModel<'_>,
    children: &'a [ProposedChild],
) -> Element<'a, Message> {
    let taken: Vec<&RepositoryId> = children.iter().map(|child| &child.repository).collect();
    let skipped = skipped(model, &taken);
    let rows = children.iter().map(|child| {
        child_row(
            &repository_name(model, &child.repository),
            &child.title,
            &child.prompt,
            None,
            STARTS_NOW,
            None,
            false,
        )
    });
    let skipped_rows = skipped
        .iter()
        .map(|repository| skipped_row(&repository.name, None, false));
    table(
        header(model, children.len(), skipped.len()),
        None,
        rows.chain(skipped_rows),
    )
}

/// The card of the proposal `id` of a plain session `model` that the user approved, with
/// the session it `started`: the one-row `Proposed session` card, whose row names the
/// session's branch and opens it. `None` for any other decision, which the timeline words.
pub(super) fn decided<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    decision: &'a Decision,
) -> Option<Element<'a, Message>> {
    let Decision::Approved(started) = decision else {
        return None;
    };
    if model.siblings.is_none() || started.is_empty() {
        return None;
    }
    let last = started.len().saturating_sub(1);
    let rows = started.iter().enumerate().map(|(index, session)| {
        let open = Message::Projects(ProjectsMessage::Select(session.session.clone()));
        let branch = BranchChoice {
            mode: session.mode,
            branch: session.branch.clone().unwrap_or_default(),
            ..BranchChoice::default()
        };
        let row = child_row(
            &repository_name(model, &session.repository),
            &session.title,
            &session.prompt,
            Some(branch_summary(&branch, None, false)),
            STARTED,
            Some(open),
            index == last,
        );
        container(row)
            .id(widget(id, &format!("row-{index}")))
            .into()
    });
    let header = Header {
        title: SIBLING_TITLE,
        summary: DECIDED_SIBLING_SUMMARY.to_owned(),
    };
    Some(table(header, Some(OPEN_HINT), rows))
}

/// The repositories of the session's project that none of `taken`, the repositories of a
/// proposal's children, names, in project order; none for a proposed session, which leaves
/// no repository out.
fn skipped<'m>(
    model: &'m SessionViewModel<'_>,
    taken: &[&RepositoryId],
) -> Vec<&'m RepositoryNode> {
    if model.siblings.is_some() {
        return Vec::new();
    }
    model
        .repositories
        .iter()
        .filter(|repository| !taken.contains(&&repository.id))
        .collect()
}

/// What the card's header says: its title, and what the proposal comes to.
struct Header {
    title: &'static str,
    summary: String,
}

/// The header of a proposal of `model` with `children` that leaves `untouched` repositories
/// without one: `Proposed split` with its [`summary`], or `Proposed session` for a plain
/// session's proposal.
fn header(model: &SessionViewModel<'_>, children: usize, untouched: usize) -> Header {
    if model.siblings.is_some() {
        Header {
            title: SIBLING_TITLE,
            summary: SIBLING_SUMMARY.to_owned(),
        }
    } else {
        Header {
            title: SPLIT_TITLE,
            summary: summary(children, untouched),
        }
    }
}

/// What the card's header says the split comes to: how many child sessions it starts and,
/// when there are any, how many repositories it leaves `untouched`.
fn summary(children: usize, untouched: usize) -> String {
    let sessions = if children == 1 {
        "1 child session".to_owned()
    } else {
        format!("{children} child sessions")
    };
    match untouched {
        0 => sessions,
        1 => format!("{sessions} · 1 repo untouched"),
        several => format!("{sessions} · {several} repos untouched"),
    }
}

/// The card: its `header` and, at the right, `hint`; the names of the columns; then `rows`,
/// parted by hairlines.
fn table<'a>(
    header: Header,
    hint: Option<&'a str>,
    rows: impl Iterator<Item = Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut header = row![
        text(header.title)
            .size(theme::HEADING)
            .font(theme::UI_SEMIBOLD),
        text(header.summary).style(style::muted),
        space::horizontal(),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if let Some(hint) = hint {
        header = header.push(text(hint).size(theme::SMALL).style(style::muted));
    }
    let column_name = |name| {
        text(name)
            .size(theme::SMALL)
            .font(theme::UI_MEDIUM)
            .style(style::muted)
    };
    let columns = cells(
        column_name("Repo").into(),
        column_name("Brief").into(),
        column_name("Starts").into(),
    );
    let mut table = column![
        container(header).padding([10, 14]),
        style::hairline(|tokens| tokens.border),
        container(columns).padding([6, 14]),
    ];
    for row in rows {
        table = table
            .push(style::hairline(|tokens| tokens.border_soft))
            .push(row);
    }
    // The padding keeps the rows off the border, which a hovered row would paint over.
    container(table)
        .padding(1)
        .width(Length::Fill)
        .clip(true)
        .style(|theme: &Theme| {
            let tokens = theme::tokens(theme);
            container::Style {
                background: Some(tokens.panel.into()),
                border: Border::default()
                    .rounded(style::CARD_RADIUS)
                    .width(1)
                    .color(tokens.border_split),
                ..container::Style::default()
            }
        })
        .into()
}

/// One line of the table: the repository, the brief and when the child starts, each in
/// its column and all starting at the line's top.
fn cells<'a>(
    repository: Element<'a, Message>,
    brief: Element<'a, Message>,
    starts: Element<'a, Message>,
) -> Row<'a, Message> {
    row![
        container(repository).width(REPOSITORY_WIDTH),
        container(brief).width(Length::Fill),
        container(starts).width(STARTS_WIDTH),
    ]
    .spacing(COLUMN_GAP)
    .align_y(Alignment::Start)
}

/// A repository's name as its column writes it, in the font of a child's row.
fn repository_cell<'a>(name: &str) -> iced::widget::Text<'a> {
    text(keep_start(name, REPOSITORY_BUDGET)).line_height(LINE)
}

/// The row of a child in the repository `name`: the title over the prompt and, when given,
/// its `branch` line, and when the child `starts`. A click sends `on_press`; `rounded` says
/// that the row is the card's last, whose corners follow the card's.
fn child_row<'a>(
    name: &str,
    title: &'a str,
    prompt: &str,
    branch: Option<Element<'a, Message>>,
    starts: &'a str,
    on_press: Option<Message>,
    rounded: bool,
) -> Element<'a, Message> {
    let mut brief = Column::new().spacing(2);
    if !title.trim().is_empty() {
        brief = brief.push(text(title).line_height(LINE));
    }
    let prompt = prompt.trim();
    brief = brief.push(if prompt.is_empty() {
        text(NO_BRIEF).line_height(LINE).style(style::muted)
    } else {
        text(prompt.to_owned()).line_height(LINE).style(style::soft)
    });
    if let Some(branch) = branch {
        brief = brief.push(branch);
    }
    let line = cells(
        repository_cell(name).font(theme::UI_MEDIUM).into(),
        brief.into(),
        text(starts).line_height(LINE).style(style::muted).into(),
    );
    table_row(line, on_press, rounded)
}

/// A child row's line about its branch: the mode and, unless the child works checked out,
/// the branch name; under it `loading…` while its repository is not read yet, else the
/// problem `check` found, unless the card is `locked` by its approval, whose own repositories
/// the check finds busy.
fn branch_summary<'a>(
    branch: &BranchChoice,
    check: Option<&ChildCheck>,
    locked: bool,
) -> Element<'a, Message> {
    let mut summary = row![
        text(mode_label(branch.mode))
            .size(theme::SMALL)
            .style(style::muted)
    ]
    .spacing(6);
    let name = branch.branch.trim();
    if branch.mode != BranchMode::CheckedOut && !name.is_empty() {
        summary = summary.push(
            text(name.to_owned())
                .size(theme::SMALL)
                .font(theme::MONO)
                .style(style::soft),
        );
    }
    let mut lines = column![summary].spacing(2);
    if let Some(note) = note(check, locked) {
        lines = lines.push(note);
    }
    lines.into()
}

/// The dim line under a child's branch settings, as [`branch_summary`] describes it; `None`
/// when there is nothing to say.
fn note<'a>(check: Option<&ChildCheck>, locked: bool) -> Option<Element<'a, Message>> {
    let note = match check {
        Some(check) if check.loading => text(LOADING).style(style::dim),
        Some(check) if !locked => text(check.problem.clone()?).style(failed_ink),
        _ => return None,
    };
    Some(note.size(theme::SMALL).into())
}

/// The style of a line that says what keeps the split from being approved.
fn failed_ink(theme: &Theme) -> text::Style {
    style::ink(|tokens| tokens.failed.fg)(theme)
}

/// The row of the repository `name` that gets no child, all muted. A click sends
/// `on_press`; `rounded` as for a [`child_row`].
fn skipped_row<'a>(name: &str, on_press: Option<Message>, rounded: bool) -> Element<'a, Message> {
    let line = cells(
        repository_cell(name).style(style::muted).into(),
        text("—").line_height(LINE).style(style::dim).into(),
        text("skipped").line_height(LINE).style(style::muted).into(),
    );
    table_row(line, on_press, rounded)
}

/// `line` as a row of the table that sends `on_press` when clicked and lights up under the
/// pointer; with the bottom corners of the card when `rounded`.
fn table_row(
    line: Row<'_, Message>,
    on_press: Option<Message>,
    rounded: bool,
) -> Element<'_, Message> {
    let corners = if rounded {
        border::bottom(INNER_RADIUS)
    } else {
        border::radius(0)
    };
    button(line)
        .padding([10, 14])
        .width(Length::Fill)
        .style(move |theme: &Theme, status| {
            let tokens = theme::tokens(theme);
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then(|| tokens.border_soft.into()),
                text_color: tokens.text,
                border: Border::default().rounded(corners),
                ..button::Style::default()
            }
        })
        .on_press_maybe(on_press)
        .into()
}

/// What the form of one child works with: the card of the proposal `id`, the child's
/// position `index`, what the last check said about it, the orchestrator's branch settings
/// for its children, the branch a proposed session's proposer works on in the child's
/// repository, whether the child can be removed and whether the card is `locked` by its
/// approval.
struct Edit<'e> {
    id: &'e ProposalId,
    index: usize,
    check: Option<&'e ChildCheck>,
    defaults: Option<&'e BranchChoice>,
    proposer: Option<&'e str>,
    removable: bool,
    locked: bool,
}

impl Edit<'_> {
    /// The message that edits the child, unless the card is locked.
    fn send<T: 'static>(
        &self,
        edit: fn(usize, T) -> ProposalMessage,
    ) -> Option<impl Fn(T) -> Message + 'static> {
        let (id, index) = (self.id.clone(), self.index);
        (!self.locked).then_some(move |value| message(&id, edit(index, value)))
    }

    /// The id of the form's widget `name`, numbered with the child's position.
    fn widget(&self, name: &str) -> iced::widget::Id {
        widget(self.id, &format!("{name}-{}", self.index))
    }
}

/// The form of the child, as `edit` describes it, which works in the repository `name`: its
/// title, its prompt, its branch settings, its auto-allow switch and, when it is removable,
/// the button that removes it; when `closable`, also the link that turns the form back into
/// the child's row.
fn form<'a>(
    edit: &Edit<'_>,
    name: &str,
    child: &'a DraftChild,
    closable: bool,
) -> Element<'a, Message> {
    let (id, index, editable) = (edit.id, edit.index, !edit.locked);
    let title = text_input("title", &child.title)
        .id(edit.widget("title"))
        .on_input_maybe(edit.send(ProposalMessage::EditTitle))
        // A field without a submit lets Enter through to the window, where it approves the
        // split; submitting the title as it is keeps the key in the field.
        .on_submit(message(
            id,
            ProposalMessage::EditTitle(index, child.title.clone()),
        ))
        .padding([5, 8])
        .style(field);
    let mut prompt = text_editor(&child.prompt)
        .id(edit.widget("prompt"))
        .placeholder("brief")
        .padding([6, 8])
        .line_height(LINE)
        .style(area);
    if let Some(on_action) = edit.send(ProposalMessage::EditPrompt) {
        prompt = prompt.on_action(on_action);
    }
    let auto_allow = toggler(child.auto_allow)
        .label("auto-allow")
        .size(14)
        .spacing(6)
        .text_size(theme::SMALL)
        .style(style::switch)
        .on_toggle_maybe(edit.send(ProposalMessage::SetAutoAllow));
    let small = |word| text(word).size(theme::SMALL);
    let mut controls = row![
        container(auto_allow).id(edit.widget("auto")),
        space::horizontal(),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if edit.removable {
        let remove = button(small("remove"))
            .padding([3, 6])
            .style(style::danger_button)
            .on_press_maybe(editable.then(|| message(id, ProposalMessage::Remove(index))));
        controls = controls.push(container(remove).id(edit.widget("remove")));
    }
    if closable {
        let done = button(small("done"))
            .padding([3, 0])
            .style(style::link_button)
            .on_press_maybe(editable.then(|| message(id, ProposalMessage::CloseRow)));
        controls = controls.push(container(done).id(edit.widget("done")));
    }
    let branch = branch_form(edit, &child.branch, std::env::home_dir().as_deref());
    let line = row![
        container(repository_cell(name).font(theme::UI_MEDIUM))
            .width(REPOSITORY_WIDTH)
            .padding([5, 0]),
        column![title, prompt, branch, controls].spacing(8),
    ]
    .spacing(COLUMN_GAP);
    container(line).padding([10, 14]).into()
}

/// A child's branch settings in its form, as `edit` describes the child: the mode switch
/// and, unless the child works checked out, the branch name, the base picker (which marks
/// the proposer's branch), the fetch
/// checkbox and, in Worktree mode, the folder the worktree goes to, written with `~` under
/// `home`; then the problem the check found. Each value that equals the orchestrator's is
/// marked: the base and fetch as they apply once git has described the repository.
fn branch_form<'a>(
    edit: &Edit<'_>,
    branch: &BranchChoice,
    home: Option<&Path>,
) -> Element<'a, Message> {
    let defaults = edit.defaults;
    let (id, index) = (edit.id, edit.index);
    let switch = mode_switch(
        &MODES,
        branch.mode,
        |mode| widget(id, &format!("mode-{index}-{}", mode_slug(mode))),
        edit.send(ProposalMessage::SetMode),
    );
    let inherited = defaults.is_some_and(|defaults| defaults.mode == branch.mode);
    let mut lines = column![setting("mode", switch, inherited)].spacing(6);
    if branch.mode != BranchMode::CheckedOut {
        let check = edit.check.cloned().unwrap_or_default();
        let name = text_input("name of the new branch", &branch.branch)
            .id(edit.widget("branch"))
            .on_input_maybe(edit.send(ProposalMessage::EditBranch))
            // As in the title, Enter stays in the field instead of approving the split.
            .on_submit(message(
                id,
                ProposalMessage::EditBranch(index, branch.branch.clone()),
            ))
            .width(PICKER_WIDTH)
            .padding(PICKER_PADDING)
            .size(theme::SMALL)
            .font(theme::MONO)
            .style(field);
        let base = base_picker(
            &check.bases,
            check.base.as_deref(),
            edit.proposer,
            check.loading,
            edit.widget("base"),
            edit.send(ProposalMessage::SetBase),
        );
        // While git reads, the base and fetch rows show what the child wants; then what
        // applies, which the mark compares with the orchestrator's.
        let fetch = if check.loading {
            branch.fetch
        } else {
            check.fetch
        };
        // An orchestrator without a common base leaves each child on its checked-out branch.
        let base_inherited = defaults.is_some_and(|defaults| match &defaults.base {
            Some(_) if !check.loading => check.base == defaults.base,
            _ => defaults.base == branch.base,
        });
        let fetch_inherited = defaults.is_some_and(|defaults| defaults.fetch == fetch);
        let fetch = fetch_toggle(
            fetch,
            check.fetch_remote.as_deref(),
            check.base.as_deref(),
            check.loading,
            edit.widget("fetch"),
            edit.send(ProposalMessage::SetFetch),
        );
        lines = lines
            .push(setting(
                "branch name",
                name.into(),
                defaults.is_some_and(|defaults| defaults.branch == branch.branch),
            ))
            .push(setting("base branch", base, base_inherited))
            .push(setting("", fetch, fetch_inherited));
        if branch.mode == BranchMode::Worktree
            && let Some(worktree) = &check.worktree
        {
            let folder = text(home_path::abbreviated(worktree, home))
                .size(theme::SMALL)
                .font(theme::MONO)
                .style(style::soft);
            lines = lines.push(setting("worktree at", folder.into(), false));
        }
    }
    let problem = edit
        .check
        .filter(|_| !edit.locked)
        .and_then(|check| check.problem.clone());
    if let Some(problem) = problem {
        lines = lines.push(text(problem).size(theme::SMALL).style(failed_ink));
    }
    lines.into()
}

/// The name a mode has in the ids of its segments.
fn mode_slug(mode: BranchMode) -> &'static str {
    match mode {
        BranchMode::Worktree => "worktree",
        BranchMode::NewBranch => "new-branch",
        BranchMode::CheckedOut => "checked-out",
    }
}

/// One branch setting of a child's form: the muted `label` in its column, then `control`
/// and, when it equals the orchestrator's (`inherited`), the dim mark that says so.
fn setting<'a>(
    label: &'a str,
    control: Element<'a, Message>,
    inherited: bool,
) -> Element<'a, Message> {
    let mark = inherited.then(|| text(FROM_ORCHESTRATOR).size(theme::SMALL).style(style::dim));
    let mut line = row![
        text(label)
            .size(theme::SMALL)
            .width(BRANCH_LABEL_WIDTH)
            .style(style::muted),
        control,
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if let Some(mark) = mark {
        line = line.push(mark);
    }
    line.into()
}

/// The card's last line while every child shows its form: the picker that adds a child in
/// any of the project's repositories; while the card is `locked`, only its placeholder.
fn add_row<'a>(
    model: &SessionViewModel<'_>,
    id: &ProposalId,
    locked: bool,
) -> Element<'a, Message> {
    let on_add = id.clone();
    let picker: Element<'a, Message> = if locked {
        container(text(ADD_CHILD).size(theme::SMALL).style(style::muted))
            .padding([4, 8])
            .into()
    } else {
        let picker = pick_list(
            add_choices(model),
            None::<RepositoryChoice>,
            move |picked| message(&on_add, ProposalMessage::Add(picked.id)),
        )
        .placeholder(ADD_CHILD)
        .text_size(theme::SMALL)
        .padding([4, 8])
        .handle(pick_list::Handle::None)
        .style(|theme: &Theme, _status| {
            let tokens = theme::tokens(theme);
            pick_list::Style {
                text_color: tokens.text_soft,
                placeholder_color: tokens.muted,
                handle_color: tokens.muted,
                background: tokens.panel.into(),
                border: outline(theme, false),
            }
        })
        .menu_style(style::menu);
        with_chevron(picker)
    };
    let line = row![
        icon(Icon::Plus, |tokens| tokens.muted),
        container(picker).id(widget(id, "add")),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    container(line).padding([10, 14]).into()
}

/// The buttons under the card: approve, edit every child at once, and reject with the
/// note written between them; for a proposed `sibling`, Approve reads `Approve` and there is
/// nothing to edit at once. While the card is `locked` by its approval, Approve reads
/// `approving…` and nothing takes input.
fn actions<'a>(
    id: &ProposalId,
    draft: &'a ProposalDraft,
    locked: bool,
    sibling: bool,
) -> Element<'a, Message> {
    // A disabled Approve offers no key: Enter does nothing while the card is locked.
    let label = if locked {
        row![text("approving…").font(theme::UI_SEMIBOLD)]
    } else {
        let approve = if sibling { "Approve" } else { "Approve split" };
        row![
            text(approve).font(theme::UI_SEMIBOLD),
            badge::key_cap_on_primary("Enter"),
        ]
    };
    let approve = button(label.spacing(8).align_y(Alignment::Center))
        .padding([7, 16])
        .style(style::primary_button)
        .on_press_maybe(
            (!locked && draft.approvable()).then(|| message(id, ProposalMessage::Approve)),
        );
    let edit_label = if draft.editing == Editing::All {
        "Done editing"
    } else {
        "Edit split"
    };
    let editable = !locked;
    let edit = button(text(edit_label))
        .padding([7, 14])
        .style(style::outlined_button)
        .on_press_maybe(editable.then(|| message(id, ProposalMessage::ToggleEditAll)));
    let on_note = id.clone();
    let note = text_input(REJECTION_NOTE, &draft.note)
        .id(widget(id, "note"))
        .on_input_maybe(
            editable.then_some(move |note| message(&on_note, ProposalMessage::EditNote(note))),
        )
        // As in a child's title, Enter stays in the field instead of approving the split.
        .on_submit(message(id, ProposalMessage::EditNote(draft.note.clone())))
        .padding([7, 10])
        .style(field);
    let reject = button(text("Reject"))
        .padding([7, 14])
        .style(style::outlined_button)
        .on_press_maybe(editable.then(|| message(id, ProposalMessage::Reject)));
    let mut buttons = row![approve];
    if !sibling {
        buttons = buttons.push(edit);
    }
    buttons
        .push(note)
        .push(reject)
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

/// A text field of the card: the panel colour inside an outline, with a muted value while
/// the field is disabled.
pub(super) fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let tokens = theme::tokens(theme);
    let focused = matches!(status, text_input::Status::Focused { .. });
    let value = if status == text_input::Status::Disabled {
        tokens.muted
    } else {
        tokens.text
    };
    text_input::Style {
        background: tokens.panel.into(),
        border: outline(theme, focused),
        icon: tokens.muted,
        placeholder: tokens.muted,
        value,
        selection: tokens.selection,
    }
}

/// The box a child's prompt is edited in, drawn like a [`field`].
fn area(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let tokens = theme::tokens(theme);
    let focused = matches!(status, text_editor::Status::Focused { .. });
    let value = if status == text_editor::Status::Disabled {
        tokens.muted
    } else {
        tokens.text
    };
    text_editor::Style {
        background: tokens.panel.into(),
        border: outline(theme, focused),
        placeholder: tokens.muted,
        value,
        selection: tokens.selection,
    }
}

/// The outline of an input of the card, which stands out while the input is `focused`.
fn outline(theme: &Theme, focused: bool) -> Border {
    let tokens = theme::tokens(theme);
    let color = if focused {
        tokens.muted
    } else {
        tokens.border_input
    };
    Border::default()
        .rounded(style::ROW_RADIUS)
        .width(1)
        .color(color)
}

/// Applies `message` to the draft of the current session's proposal `id` and returns the
/// follow-up work; a message for a proposal without a draft, or one whose approval is in
/// flight, is ignored. An edit of the children's branch settings, and adding or removing a
/// child, lets the core check the children again.
///
/// Approving and rejecting go through the core [`App`], which records the decision in the
/// transcript; the draft is dropped once it succeeded, and kept with the reason in the error
/// bar when the app refuses. An approval that waits for git keeps the draft.
pub(crate) fn update(state: &mut State, id: ProposalId, message: ProposalMessage) -> Task<Message> {
    let Some((current, auto_allow, defaults)) = state
        .app
        .session_view()
        .filter(|model| !approving(model, &id))
        .map(|model| {
            let defaults = model.child_defaults.clone().unwrap_or_default();
            (model.id, model.auto_allow, defaults)
        })
    else {
        return Task::none();
    };
    let key = (current.clone(), id.clone());
    let Some(draft) = state.session.proposals.get_mut(&key) else {
        return Task::none();
    };
    match message {
        ProposalMessage::Approve => {
            let choices = draft.choices();
            // A pending approval keeps the card until git has made the children's branches.
            let decided = state
                .app
                .approve_proposal(current, id, choices)
                .map(|approval| matches!(approval, Approval::Started(_)));
            decided_by(state, &key, decided)
        }
        ProposalMessage::Reject => {
            let note = draft.rejection_note();
            let decided = state.app.reject_proposal(current, id, note).map(|()| true);
            decided_by(state, &key, decided)
        }
        edit => {
            if edit_draft(draft, edit, auto_allow, defaults) {
                // The draft was found undecided on the current session above.
                let _wanted = draft.check(&mut state.app, &current, &id, true);
            }
            Task::none()
        }
    }
}

/// Applies the edit `message` to `draft`; a child it adds grants permission requests without
/// asking when `auto_allow` and starts from the branch settings `branch`. Returns whether the
/// children's branch settings changed, or children were added or removed, so the core checks
/// them again.
fn edit_draft(
    draft: &mut ProposalDraft,
    message: ProposalMessage,
    auto_allow: bool,
    branch: BranchChoice,
) -> bool {
    match message {
        ProposalMessage::EditPrompt(index, action) => {
            if let Some(child) = draft.children.get_mut(index) {
                child.prompt.perform(action);
            }
            false
        }
        ProposalMessage::EditTitle(index, title) => {
            if let Some(child) = draft.children.get_mut(index) {
                child.title = title;
            }
            false
        }
        ProposalMessage::SetAutoAllow(index, enabled) => {
            if let Some(child) = draft.children.get_mut(index) {
                child.auto_allow = enabled;
            }
            false
        }
        ProposalMessage::SetMode(index, mode) => {
            change_branch(draft, index, |branch| branch.mode = mode)
        }
        ProposalMessage::EditBranch(index, name) => {
            change_branch(draft, index, |branch| branch.branch = name)
        }
        ProposalMessage::SetBase(index, base) => {
            change_branch(draft, index, |branch| branch.base = Some(base))
        }
        ProposalMessage::SetFetch(index, fetch) => {
            change_branch(draft, index, |branch| branch.fetch = fetch)
        }
        ProposalMessage::Remove(index) => {
            let removed = index < draft.children.len();
            if removed {
                draft.children.remove(index);
                // The positions after the removed child shifted, so a single open form
                // would now be another child's.
                if matches!(draft.editing, Editing::Row(_)) {
                    draft.editing = Editing::Nothing;
                }
            }
            removed
        }
        ProposalMessage::Add(repository) => {
            // The new child has nothing to show in a row yet, so its form opens.
            if draft.editing != Editing::All {
                draft.editing = Editing::Row(draft.children.len());
            }
            draft.children.push(DraftChild {
                repository,
                title: String::new(),
                prompt: text_editor::Content::new(),
                auto_allow,
                branch,
            });
            true
        }
        ProposalMessage::OpenRow(index) => {
            if index < draft.children.len() {
                draft.editing = Editing::Row(index);
            }
            false
        }
        ProposalMessage::CloseRow => {
            if matches!(draft.editing, Editing::Row(_)) {
                draft.editing = Editing::Nothing;
            }
            false
        }
        ProposalMessage::ToggleEditAll => {
            draft.editing = if draft.editing == Editing::All {
                Editing::Nothing
            } else {
                Editing::All
            };
            false
        }
        ProposalMessage::EditNote(note) => {
            draft.note = note;
            false
        }
        // A decision is no edit; `update` takes it to the app.
        ProposalMessage::Approve | ProposalMessage::Reject => false,
    }
}

/// Applies `change` to the branch settings of the child at `index` of `draft`; returns
/// whether there is such a child.
fn change_branch(
    draft: &mut ProposalDraft,
    index: usize,
    change: impl FnOnce(&mut BranchChoice),
) -> bool {
    let child = draft.children.get_mut(index);
    let found = child.is_some();
    if let Some(child) = child {
        change(&mut child.branch);
    }
    found
}

/// Settles the draft `key` after the app took a decision on it: `decided` says whether the
/// proposal is decided now, which drops the draft, or why the app refused.
fn decided_by(
    state: &mut State,
    key: &(SessionId, ProposalId),
    decided: Result<bool, AppError>,
) -> Task<Message> {
    if app::report(&mut state.error, decided) == Some(true) {
        state.session.proposals.remove(key);
    }
    // The decision is now in the transcript.
    sync_markdown(state);
    Task::none()
}

/// Approves the newest undecided proposal of the current session with its draft, as its
/// `Approve split` (or `Approve`) button does. Like the button, it does nothing while the draft has no
/// children, while a child is loading or has a problem, while the approval is in flight,
/// and when no proposal is undecided.
pub(crate) fn approve_newest(state: &mut State) -> Task<Message> {
    let newest = state.app.session_view().and_then(|model| {
        model.entries.iter().rev().find_map(|entry| match entry {
            Entry::Proposal {
                id, decision: None, ..
            } => Some(((model.id.clone(), id.clone()), approving(&model, id))),
            _ => None,
        })
    });
    let Some((key, locked)) = newest else {
        return Task::none();
    };
    let approvable = state
        .session
        .proposals
        .get(&key)
        .is_some_and(ProposalDraft::approvable);
    if locked || !approvable {
        return Task::none();
    }
    let (_, id) = key;
    update(state, id, ProposalMessage::Approve)
}

/// The branch the plain session `model` works on, when it proposes a session in its own
/// `repository`; `None` for an orchestrator, and for any other repository.
fn proposer_branch<'m>(
    model: &'m SessionViewModel<'_>,
    repository: &RepositoryId,
) -> Option<&'m str> {
    model
        .siblings
        .as_ref()
        .filter(|siblings| siblings.repository.as_ref() == Some(repository))
        .and_then(|siblings| siblings.proposer_branch.as_deref())
}

/// The project's repositories the picker offers for a new child, in project order.
fn add_choices(model: &SessionViewModel<'_>) -> Vec<RepositoryChoice> {
    model
        .repositories
        .iter()
        .map(|repository| RepositoryChoice {
            id: repository.id.clone(),
            name: repository.name.clone(),
        })
        .collect()
}

/// The id of the widget `name` on the card of the proposal `id`.
fn widget(id: &ProposalId, name: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("proposal-{id}-{name}"))
}

/// `message` for the card of the proposal `id`.
fn message(id: &ProposalId, message: ProposalMessage) -> Message {
    Message::Session(SessionMessage::Proposal(id.clone(), message))
}

/// One repository in the picker that adds a child, shown by its name.
#[derive(Debug, Clone, PartialEq)]
struct RepositoryChoice {
    id: RepositoryId,
    name: String,
}

impl fmt::Display for RepositoryChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[cfg(test)]
mod tests;
