//! The jump panel: a search over every session of every open project, opened with Ctrl+K or
//! the toolbar's button, that takes the user to the session they pick.
//!
//! The panel lies over the window, which stays visible under a dimming backdrop: a card
//! with a search field and, under it, the sessions that match what was typed, best match
//! first and at most eight of them. A row names the session, then its project and, for a
//! spawned session, the session it was spawned from, and carries its status pill. One row
//! is highlighted; the arrow keys move the highlight, Enter or a click opens the session on
//! the Session tab, and Escape or a click on the backdrop closes the panel.

use iced::widget::{
    Column, Row, button, column, container, mouse_area, opaque, operation, space, stack, text,
    text_input,
};
use iced::{Alignment, Border, Color, Element, Length, Padding, Task, Theme};
use partitur_core::{SessionId, SessionNode, Status, TreeModel};

use crate::app::{BareKey, Message, State};
use crate::ellipsis::keep_start;
use crate::projects_view::{self, ProjectsMessage};
use crate::theme::Tokens;
use crate::{badge, style, theme};

/// The id of the search field, which takes the focus when the panel opens.
const QUERY: &str = "jump-query";

/// What the search field says while it is empty.
const PLACEHOLDER: &str = "Jump to session…";

/// The width of the panel's card, in logical pixels.
const WIDTH: f32 = 520.0;

/// How far below the window's top edge the card sits.
const TOP: f32 = 96.0;

/// The most sessions the panel lists.
const ROWS: usize = 8;

/// The height of a session's row, in logical pixels.
const ROW_HEIGHT: f32 = 28.0;

/// The room the list keeps around its rows, and a row left and right of its content.
const LIST_PADDING: f32 = 6.0;

/// The space between the parts of a row.
const PART_GAP: f32 = 10.0;

/// The most characters of a session's title a row shows.
const TITLE_BUDGET: usize = 32;

/// The most characters of a session's project and parent a row shows.
const CONTEXT_BUDGET: usize = 28;

/// How much of the window's colour the backdrop lays over the page.
const BACKDROP_STRENGTH: f32 = 0.6;

/// A colour picked from the active theme's tokens.
type Ink = fn(&Tokens) -> Color;

/// What the open panel remembers between frames. A panel opens with an empty query and
/// its first row highlighted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct JumpState {
    /// What the user typed into the search field.
    pub(crate) query: String,
    /// The position of the highlighted row among the listed sessions.
    pub(crate) highlight: usize,
}

/// An interaction with the jump panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpMessage {
    /// The user asked for the panel: it opens and its search field takes the focus.
    Open,
    /// The user dismissed the panel.
    Close,
    /// The user edited the search field.
    Query(String),
    /// The user asked for the row below the highlighted one.
    HighlightNext,
    /// The user asked for the row above the highlighted one.
    HighlightPrevious,
    /// The user asked for the highlighted session.
    OpenHighlighted,
    /// The user picked a session.
    Pick(SessionId),
}

/// A session the panel can take the user to, with what its row shows and what the query is
/// matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    /// The session's identity.
    id: SessionId,
    /// The session's title.
    title: String,
    /// The name of the session's project.
    project: String,
    /// The name of the repository the session works in; empty for a session of the whole
    /// project.
    repository: String,
    /// The title of the session this one was spawned from, if it was spawned.
    parent: Option<String>,
    /// Where the session stands.
    status: Status,
}

/// How well a session matches a query, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// The title contains the query in one piece.
    Title,
    /// The searchable text contains the query in one piece, outside the title.
    Contiguous,
    /// The searchable text contains the query's characters in order, with others between.
    Scattered,
}

/// `page` with the panel of `state` over it, listing the sessions of `tree` that match the
/// panel's query.
///
/// The backdrop takes every click meant for the page; a click on it closes the panel.
pub fn view<'a>(
    page: Element<'a, Message>,
    tree: &TreeModel,
    state: &JumpState,
) -> Element<'a, Message> {
    let query = text_input(PLACEHOLDER, &state.query)
        .id(QUERY)
        .on_input(|query| Message::Jump(JumpMessage::Query(query)))
        .on_submit(Message::Jump(JumpMessage::OpenHighlighted))
        .padding([10, 16])
        .style(query_style);
    let found = matches(tree, &state.query);
    let mut list = Column::new().spacing(2).padding(LIST_PADDING);
    if found.is_empty() {
        list = list.push(
            container(text("No session matches").style(style::muted))
                .center_y(ROW_HEIGHT)
                .padding([0.0, PART_GAP]),
        );
    }
    for (position, target) in found.iter().enumerate() {
        list = list.push(row(target, position == state.highlight));
    }
    let card = container(column![
        query,
        style::hairline(|tokens| tokens.border_soft),
        list
    ])
    .width(WIDTH)
    .style(style::card);
    // The card is opaque to clicks, so only a click beside it reaches the backdrop.
    let backdrop = mouse_area(
        container(opaque(card))
            .center_x(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::ZERO.top(TOP))
            .style(|theme: &Theme| container::Style {
                background: Some(
                    theme::tokens(theme)
                        .bg
                        .scale_alpha(BACKDROP_STRENGTH)
                        .into(),
                ),
                ..container::Style::default()
            }),
    )
    .on_press(Message::Jump(JumpMessage::Close));
    stack![page, opaque(backdrop)].into()
}

/// Applies `message` to the panel and returns the follow-up work.
///
/// Opening gives the search field the focus. Typing starts over at the first match. The
/// highlight stops at the first and the last listed row. Picking a session closes the panel
/// and selects the session the way a click in the tree does, which shows it on the Session
/// tab. Everything but opening means nothing while the panel is closed.
pub fn update(state: &mut State, message: JumpMessage) -> Task<Message> {
    match message {
        JumpMessage::Open => {
            state.jump.get_or_insert_default();
            return operation::focus(QUERY);
        }
        JumpMessage::Close => state.jump = None,
        JumpMessage::Query(query) => {
            if state.jump.is_some() {
                state.jump = Some(JumpState {
                    query,
                    highlight: 0,
                });
            }
        }
        JumpMessage::HighlightNext => {
            let tree = state.app.tree();
            if let Some(panel) = &mut state.jump {
                let last = matches(&tree, &panel.query).len().saturating_sub(1);
                panel.highlight = panel.highlight.saturating_add(1).min(last);
            }
        }
        JumpMessage::HighlightPrevious => {
            if let Some(panel) = &mut state.jump {
                panel.highlight = panel.highlight.saturating_sub(1);
            }
        }
        JumpMessage::OpenHighlighted => {
            let highlighted = state.jump.as_ref().and_then(|panel| {
                matches(&state.app.tree(), &panel.query)
                    .into_iter()
                    .nth(panel.highlight)
            });
            if let Some(target) = highlighted {
                return update(state, JumpMessage::Pick(target.id));
            }
        }
        JumpMessage::Pick(session) => {
            if state.jump.take().is_some() {
                return projects_view::update(state, ProjectsMessage::Select(session));
            }
        }
    }
    Task::none()
}

/// What the bare `key` does while the panel is open: the arrow keys move the highlight,
/// Enter opens the highlighted session and Escape closes the panel. Tab, N and A belong to
/// the page under the panel, which takes no keys while it is covered.
pub fn press(state: &mut State, key: BareKey) -> Task<Message> {
    let message = match key {
        BareKey::ArrowDown => JumpMessage::HighlightNext,
        BareKey::ArrowUp => JumpMessage::HighlightPrevious,
        BareKey::Enter => JumpMessage::OpenHighlighted,
        BareKey::Escape => JumpMessage::Close,
        BareKey::Tab | BareKey::N | BareKey::A => return Task::none(),
    };
    update(state, message)
}

/// The search field: only its text, on the card that holds it.
fn query_style(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    let tokens = theme::tokens(theme);
    text_input::Style {
        // The card shows through: a surface of the field's own would cover the card's
        // rounded corners.
        background: tokens.panel.scale_alpha(0.0).into(),
        border: Border::default(),
        icon: tokens.muted,
        placeholder: tokens.muted,
        value: tokens.text,
        selection: tokens.selection,
    }
}

/// The row of `target`, found by `jump-<id>`: its title, its project and parent, and at the
/// right its status pill. A click picks the session.
fn row<'a>(target: &Target, highlighted: bool) -> Element<'a, Message> {
    // The weaker muted colour is too faint on the selection colour.
    let (title_ink, context_ink): (Ink, Ink) = if highlighted {
        (|tokens| tokens.text_strong, |tokens| tokens.muted_strong)
    } else {
        (|tokens| tokens.text_soft, |tokens| tokens.muted)
    };
    let mut line = Row::new()
        .spacing(PART_GAP)
        .align_y(Alignment::Center)
        .push(text(keep_start(&target.title, TITLE_BUDGET)).style(style::ink(title_ink)))
        .push(
            text(keep_start(&context(target), CONTEXT_BUDGET))
                .size(theme::SMALL)
                .style(style::ink(context_ink)),
        )
        .push(space::horizontal());
    if let Some(status) = badge::status(target.status) {
        line = line.push(status);
    }
    let look = if highlighted {
        style::selected_row_button
    } else {
        style::row_button
    };
    button(
        container(line)
            .id(format!("jump-{}", target.id))
            .center_y(Length::Fill),
    )
    .padding([0.0, PART_GAP])
    .width(Length::Fill)
    .height(ROW_HEIGHT)
    .style(look)
    .on_press(Message::Jump(JumpMessage::Pick(target.id.clone())))
    .into()
}

/// Where `target` belongs, as its row tells it: its project and, for a spawned session,
/// the session it was spawned from.
fn context(target: &Target) -> String {
    match &target.parent {
        Some(parent) => format!("{} › {parent}", target.project),
        None => target.project.clone(),
    }
}

/// The sessions the panel lists for `query`: those of `tree` that match it, the best
/// matches first and equally good ones in the tree's order, cut to the rows the panel has.
/// An empty query matches every session.
fn matches(tree: &TreeModel, query: &str) -> Vec<Target> {
    let mut ranked: Vec<(Rank, Target)> = targets(tree)
        .into_iter()
        .filter_map(|target| Some((rank(&target, query)?, target)))
        .collect();
    // The sort is stable, so equally good matches keep the tree's order.
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked
        .into_iter()
        .take(ROWS)
        .map(|(_, target)| target)
        .collect()
}

/// Every session of every project of `tree` in the tree's order, each followed by the
/// sessions spawned from it.
fn targets(tree: &TreeModel) -> Vec<Target> {
    let mut found = Vec::new();
    for project in &tree.projects {
        collect(&project.sessions, &project.name, None, &mut found);
    }
    found
}

/// Appends `sessions` of the project named `project`, spawned from the session titled
/// `parent` if any, to `found`, each followed by the sessions spawned from it.
fn collect(sessions: &[SessionNode], project: &str, parent: Option<&str>, found: &mut Vec<Target>) {
    for session in sessions {
        found.push(Target {
            id: session.id.clone(),
            title: session.title.clone(),
            project: project.to_owned(),
            repository: session.scope_label.clone(),
            parent: parent.map(str::to_owned),
            status: session.status,
        });
        collect(&session.children, project, Some(&session.title), found);
    }
}

/// How well `target` matches `query`, or `None` when it does not: the query's characters
/// have to appear in order in the session's title, project, repository and parent title,
/// whatever their case.
fn rank(target: &Target, query: &str) -> Option<Rank> {
    let query = query.trim().to_lowercase();
    let title = target.title.to_lowercase();
    let searchable = [
        title.as_str(),
        &target.project,
        &target.repository,
        target.parent.as_deref().unwrap_or_default(),
    ]
    .join(" ")
    .to_lowercase();
    if title.contains(&query) {
        Some(Rank::Title)
    } else if searchable.contains(&query) {
        Some(Rank::Contiguous)
    } else if appears_in_order(&query, &searchable) {
        Some(Rank::Scattered)
    } else {
        None
    }
}

/// Whether the characters of `needle` appear in `haystack` in the same order, with any
/// others between them.
fn appears_in_order(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle
        .chars()
        .all(|wanted| rest.any(|candidate| candidate == wanted))
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

    use iced::Point;
    use iced::keyboard::key;
    use iced::widget::Id;
    use iced_test::simulator::{self, simulator};
    use partitur_core::{ProjectId, ProjectNode, Scope};
    use tempfile::TempDir;

    use super::*;
    use crate::app::{Tab, update as update_app, view as window};
    use crate::test_support::{Orchestrated, orchestrated};

    /// A session row of a tree, titled `title`, working in `repository` (empty for the whole
    /// project), with the sessions spawned from it.
    fn node(title: &str, repository: &str, children: Vec<SessionNode>) -> SessionNode {
        SessionNode {
            id: SessionId::new(),
            title: title.into(),
            status: Status::Idle,
            attention: false,
            scope_label: repository.into(),
            backend: "claude".into(),
            orchestrator: false,
            usage: None,
            children,
            archived: None,
        }
    }

    /// A tree with one project per `(name, sessions)`.
    fn tree(projects: Vec<(&str, Vec<SessionNode>)>) -> TreeModel {
        let projects = projects
            .into_iter()
            .map(|(name, sessions)| ProjectNode {
                id: ProjectId::new(),
                name: name.into(),
                root: PathBuf::from("/work").join(name),
                has_global_instructions: false,
                has_global_claude_md: false,
                sessions,
                archived: Vec::new(),
                repositories: Vec::new(),
                repository_count: 0,
            })
            .collect();
        TreeModel { projects }
    }

    /// The titles of the sessions the panel lists for `query`, top to bottom.
    fn titles(tree: &TreeModel, query: &str) -> Vec<String> {
        matches(tree, query)
            .into_iter()
            .map(|target| target.title)
            .collect()
    }

    /// The project `shop-app` with an orchestrator, its two children and the session spawned
    /// from one of them, and the project `taffle` with one session.
    fn shop_and_taffle() -> TreeModel {
        let migration = node("Write the migration", "backend", Vec::new());
        let api = node("Gift card API", "backend", vec![migration]);
        let form = node("Redeem form", "frontend", Vec::new());
        tree(vec![
            (
                "shop-app",
                vec![node("Add gift cards", "", vec![api, form])],
            ),
            ("taffle", vec![node("Rename config keys", "", Vec::new())]),
        ])
    }

    /// The orchestrator of [`orchestrated`] (titled `New session`) with its child `API
    /// endpoint`, a second project with one session, which is returned, and the panel open.
    /// The second project's folder must outlive the state.
    fn open_panel() -> (Orchestrated, SessionId, TempDir) {
        let dir = TempDir::new().unwrap();
        let mut fixture = orchestrated();
        let project = fixture.state.app.open_project(dir.path()).unwrap();
        let other = fixture
            .state
            .app
            .new_session(project, Scope::Project)
            .unwrap();
        // Creating the session made it the current one; the orchestrator is where the user is.
        fixture
            .state
            .app
            .select_session(fixture.orchestrator.clone())
            .unwrap();
        apply(&mut fixture.state, [JumpMessage::Open]);
        (fixture, other, dir)
    }

    /// Feeds the messages through the application's `update`, dropping the tasks: the
    /// simulator cannot run them.
    fn apply(state: &mut State, messages: impl IntoIterator<Item = JumpMessage>) {
        for message in messages {
            let _task = update_app(state, Message::Jump(message));
        }
    }

    /// Presses the bare `keys` one after the other.
    fn press_keys(state: &mut State, keys: impl IntoIterator<Item = BareKey>) {
        for key in keys {
            let _task = update_app(state, Message::Key(key));
        }
    }

    /// The row of `session` in the open panel.
    fn row_id(session: &SessionId) -> Id {
        Id::from(format!("jump-{session}"))
    }

    #[test]
    fn an_empty_query_lists_every_session_in_the_trees_order() {
        let tree = shop_and_taffle();

        assert_eq!(
            titles(&tree, ""),
            [
                "Add gift cards",
                "Gift card API",
                "Write the migration",
                "Redeem form",
                "Rename config keys"
            ]
        );
        assert_eq!(titles(&tree, "  "), titles(&tree, ""));
    }

    #[test]
    fn a_target_knows_its_project_repository_and_parent() {
        let tree = shop_and_taffle();

        let targets = targets(&tree);

        let migration = &targets[2];
        assert_eq!(migration.title, "Write the migration");
        assert_eq!(migration.project, "shop-app");
        assert_eq!(migration.repository, "backend");
        assert_eq!(migration.parent.as_deref(), Some("Gift card API"));
        assert_eq!(context(migration), "shop-app › Gift card API");
        let top = &targets[4];
        assert_eq!(top.project, "taffle");
        assert_eq!(top.parent, None);
        assert_eq!(context(top), "taffle");
    }

    #[test]
    fn the_list_holds_at_most_eight_sessions() {
        let sessions = (0..12)
            .map(|number| node(&format!("session {number}"), "", Vec::new()))
            .collect();
        let tree = tree(vec![("work", sessions)]);

        let listed = titles(&tree, "");

        assert_eq!(listed.len(), ROWS);
        assert_eq!(listed[0], "session 0");
        assert_eq!(listed[7], "session 7");
        // The cut comes after the ranking: a match further down the tree is still found.
        assert_eq!(titles(&tree, "11"), ["session 11"]);
    }

    #[test]
    fn a_query_matches_its_characters_in_order_whatever_their_case() {
        let tree = shop_and_taffle();

        // The session by its title, then the one spawned from it by its parent's.
        assert_eq!(
            titles(&tree, "GIFT CARD api"),
            ["Gift card API", "Write the migration"]
        );
        assert_eq!(titles(&tree, "rdfm"), ["Redeem form"]);
        // The same words the other way round match nothing.
        assert!(titles(&tree, "form redeem").is_empty());
        assert!(titles(&tree, "zzz").is_empty());
        assert!(appears_in_order("", "anything"));
        assert!(appears_in_order("ace", "abcde"));
        assert!(!appears_in_order("aa", "abc"));
        assert!(!appears_in_order("a", ""));
    }

    #[test]
    fn a_query_also_matches_the_project_the_repository_and_the_parent() {
        let tree = shop_and_taffle();

        assert_eq!(titles(&tree, "taffle"), ["Rename config keys"]);
        assert_eq!(titles(&tree, "frontend"), ["Redeem form"]);
        // The session itself by its title, then the sessions spawned from it.
        assert_eq!(
            titles(&tree, "add gift"),
            ["Add gift cards", "Gift card API", "Redeem form"]
        );
    }

    #[test]
    fn title_matches_come_first_then_other_whole_matches_then_scattered_ones() {
        let tree = tree(vec![
            (
                "gateway",
                vec![
                    node("Add payments interface", "", Vec::new()),
                    node("Rename config", "api", Vec::new()),
                    node("Gift card API", "", Vec::new()),
                    node("Rapid checks", "", Vec::new()),
                    node("Document the api", "", Vec::new()),
                ],
            ),
            ("billing", vec![node("Fix totals", "", Vec::new())]),
        ]);

        assert_eq!(
            titles(&tree, "api"),
            [
                // The title holds the query in one piece; two such keep the tree's order.
                "Gift card API",
                "Rapid checks",
                "Document the api",
                // The repository holds it in one piece.
                "Rename config",
                // Only the letters, in order: "Add pAyments Interface".
                "Add payments interface",
            ]
        );
        assert_eq!(
            rank(&targets(&tree)[2], "API"),
            Some(Rank::Title),
            "the case of the query does not count"
        );
        assert_eq!(rank(&targets(&tree)[1], "api"), Some(Rank::Contiguous));
        assert_eq!(rank(&targets(&tree)[0], "api"), Some(Rank::Scattered));
        assert_eq!(rank(&targets(&tree)[5], "api"), None);
    }

    #[test]
    fn the_toolbar_button_asks_for_the_panel_and_opening_shows_it() {
        let mut fixture = orchestrated();
        let state = &mut fixture.state;
        assert_eq!(state.jump, None);

        let mut ui = simulator(window(state));
        assert!(ui.find(Id::new(QUERY)).is_err());
        ui.find("Ctrl K").unwrap();
        ui.click("Jump to session").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::Jump(JumpMessage::Open)]);
        apply(state, [JumpMessage::Open]);
        assert_eq!(state.jump, Some(JumpState::default()));
        let mut ui = simulator(window(state));
        ui.find(Id::new(QUERY)).unwrap();
        ui.find(row_id(&fixture.orchestrator)).unwrap();
        ui.find(row_id(&fixture.child)).unwrap();
    }

    #[test]
    fn asking_again_keeps_what_the_open_panel_holds() {
        let (mut fixture, _other, _dir) = open_panel();
        let state = &mut fixture.state;
        apply(state, [JumpMessage::Query("new".into())]);
        press_keys(state, [BareKey::ArrowDown]);
        let held = state.jump.clone();

        apply(state, [JumpMessage::Open]);

        assert_eq!(state.jump, held);
        assert_eq!(held.unwrap().highlight, 1);
    }

    #[test]
    fn a_row_names_the_session_its_project_and_its_parent() {
        let (fixture, _other, _dir) = open_panel();
        let tree = fixture.state.app.tree();
        let project = tree.projects[0].name.clone();

        let mut ui = simulator(window(&fixture.state));

        ui.find("API endpoint").unwrap();
        ui.find(format!("{project} › New session").as_str())
            .unwrap();
        ui.find(project.as_str()).unwrap();
        // Both sessions of the fixture are running.
        ui.find("running").unwrap();
    }

    #[test]
    fn escape_closes_the_panel() {
        let (mut fixture, _other, _dir) = open_panel();

        press_keys(&mut fixture.state, [BareKey::Escape]);

        assert_eq!(fixture.state.jump, None);
        let mut ui = simulator(window(&fixture.state));
        assert!(ui.find(Id::new(QUERY)).is_err());
    }

    #[test]
    fn a_click_on_the_backdrop_closes_the_panel_and_one_on_the_card_does_not() {
        let (mut fixture, _other, _dir) = open_panel();
        apply(&mut fixture.state, [JumpMessage::Query("zzz".into())]);

        let mut ui = simulator(window(&fixture.state));
        ui.click("No session matches").unwrap();
        assert_eq!(ui.into_messages().count(), 0);

        let mut ui = simulator(window(&fixture.state));
        ui.point_at(Point::new(20.0, 600.0));
        let _ = ui.simulate(simulator::click());
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::Jump(JumpMessage::Close)]);
        apply(&mut fixture.state, [JumpMessage::Close]);
        assert_eq!(fixture.state.jump, None);
    }

    #[test]
    fn the_page_under_the_panel_takes_no_clicks() {
        let (fixture, _other, _dir) = open_panel();

        let mut ui = simulator(window(&fixture.state));
        ui.click("Session").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        // The tab's button is covered by the backdrop, which the click dismisses instead.
        assert_eq!(messages, [Message::Jump(JumpMessage::Close)]);
    }

    #[test]
    fn typing_filters_the_list_and_starts_over_at_the_first_match() {
        let (mut fixture, other, _dir) = open_panel();
        let state = &mut fixture.state;
        press_keys(state, [BareKey::ArrowDown, BareKey::ArrowDown]);
        assert_eq!(state.jump.as_ref().unwrap().highlight, 2);

        let mut ui = simulator(window(state));
        ui.click(Id::new(QUERY)).unwrap();
        let _ = ui.typewrite("e");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert_eq!(messages, [Message::Jump(JumpMessage::Query("e".into()))]);

        apply(state, [JumpMessage::Query("endpoint".into())]);

        assert_eq!(
            state.jump,
            Some(JumpState {
                query: "endpoint".into(),
                highlight: 0
            })
        );
        let mut ui = simulator(window(state));
        ui.find(row_id(&fixture.child)).unwrap();
        assert!(ui.find(row_id(&fixture.orchestrator)).is_err());
        assert!(ui.find(row_id(&other)).is_err());
        assert!(ui.find("No session matches").is_err());
    }

    #[test]
    fn a_query_nothing_matches_says_so() {
        let (mut fixture, _other, _dir) = open_panel();

        apply(&mut fixture.state, [JumpMessage::Query("zzz".into())]);

        let mut ui = simulator(window(&fixture.state));
        ui.find("No session matches").unwrap();
        assert!(ui.find(row_id(&fixture.child)).is_err());
    }

    #[test]
    fn the_arrow_keys_move_the_highlight_and_stop_at_both_ends() {
        let (mut fixture, _other, _dir) = open_panel();
        let state = &mut fixture.state;
        let mut highlight_after = |key| {
            press_keys(state, [key]);
            state.jump.as_ref().unwrap().highlight
        };

        // Three sessions are listed.
        assert_eq!(highlight_after(BareKey::ArrowUp), 0);
        assert_eq!(highlight_after(BareKey::ArrowDown), 1);
        assert_eq!(highlight_after(BareKey::ArrowDown), 2);
        assert_eq!(highlight_after(BareKey::ArrowDown), 2);
        assert_eq!(highlight_after(BareKey::ArrowUp), 1);

        // With one match left, there is nowhere to go.
        apply(state, [JumpMessage::Query("endpoint".into())]);
        press_keys(state, [BareKey::ArrowDown]);
        assert_eq!(state.jump.as_ref().unwrap().highlight, 0);
        apply(state, [JumpMessage::Query("zzz".into())]);
        press_keys(state, [BareKey::ArrowDown]);
        assert_eq!(state.jump.as_ref().unwrap().highlight, 0);
    }

    #[test]
    fn enter_opens_the_highlighted_session_on_the_session_tab() {
        let (mut fixture, _other, _dir) = open_panel();
        let state = &mut fixture.state;
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.current_session(), Some(fixture.orchestrator));

        press_keys(state, [BareKey::ArrowDown, BareKey::Enter]);

        assert_eq!(state.app.current_session(), Some(fixture.child.clone()));
        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.projects.cursor, Some(fixture.child));
        assert_eq!(state.jump, None);
        assert_eq!(state.error, None);
    }

    #[test]
    fn enter_in_the_search_field_asks_for_the_highlighted_session() {
        let (fixture, _other, _dir) = open_panel();

        let mut ui = simulator(window(&fixture.state));
        ui.click(Id::new(QUERY)).unwrap();
        let _ = ui.tap_key(key::Named::Enter);
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::Jump(JumpMessage::OpenHighlighted)]);
    }

    #[test]
    fn enter_without_a_match_keeps_the_panel_open() {
        let (mut fixture, _other, _dir) = open_panel();
        let state = &mut fixture.state;
        apply(state, [JumpMessage::Query("zzz".into())]);

        press_keys(state, [BareKey::Enter]);

        assert!(state.jump.is_some());
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.current_session(), Some(fixture.orchestrator));
    }

    #[test]
    fn clicking_a_row_opens_its_session() {
        let (mut fixture, other, _dir) = open_panel();
        let state = &mut fixture.state;

        let mut ui = simulator(window(state));
        ui.click(row_id(&other)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        assert_eq!(messages, [Message::Jump(JumpMessage::Pick(other.clone()))]);
        apply(state, [JumpMessage::Pick(other.clone())]);
        assert_eq!(state.app.current_session(), Some(other));
        assert_eq!(state.tab, Tab::Session);
        assert_eq!(state.jump, None);
    }

    #[test]
    fn the_page_under_the_panel_takes_no_keys() {
        let (mut fixture, _other, _dir) = open_panel();
        let state = &mut fixture.state;
        state.projects.cursor = Some(fixture.orchestrator.clone());
        let sessions = |state: &State| targets(&state.app.tree()).len();
        let before = sessions(state);

        press_keys(
            state,
            [
                BareKey::ArrowDown,
                BareKey::ArrowUp,
                BareKey::ArrowDown,
                BareKey::Tab,
                BareKey::N,
            ],
        );

        // The tree's cursor stayed, the tab did not change and no session was started; the
        // arrows moved the panel's highlight instead.
        assert_eq!(state.projects.cursor, Some(fixture.orchestrator));
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(sessions(state), before);
        assert_eq!(state.jump.as_ref().unwrap().highlight, 1);
    }

    #[test]
    fn a_closed_panel_ignores_what_was_meant_for_an_open_one() {
        let mut fixture = orchestrated();
        let state = &mut fixture.state;

        apply(
            state,
            [
                JumpMessage::Query("api".into()),
                JumpMessage::HighlightNext,
                JumpMessage::HighlightPrevious,
                JumpMessage::OpenHighlighted,
                JumpMessage::Pick(fixture.child.clone()),
                JumpMessage::Close,
            ],
        );

        assert_eq!(state.jump, None);
        assert_eq!(state.tab, Tab::Projects);
        assert_eq!(state.app.current_session(), Some(fixture.orchestrator));
    }
}
