//! The ranking behind the jump panel: which sessions of the project tree a search lists, and
//! in what order.
//!
//! [`JumpModel::build`] walks every live session of every project in the tree's order, each
//! followed by the sessions spawned from it, and, when asked, the archived ones the same way
//! after them. A session is listed when the query's characters appear in order in its title,
//! project, repository and parent's title, whatever their case; an empty query lists every
//! session. Live sessions come before archived ones, then sessions waiting for the user, then
//! running ones, then failed, interrupted and idle ones, and done ones and those waiting for
//! their orchestrator last. Among sessions that stand alike a title match comes first, then
//! one elsewhere in one piece, then a scattered one, and equally good ones keep the tree's
//! order. The list holds at most [`JumpModel::ROWS`] entries, cut after the ranking.

use crate::archive::Archived;
use crate::ids::SessionId;
use crate::session::Status;
use crate::view_model::{SessionNode, TreeModel, background_shown};

/// A session the jump panel can take the user to, with what its row shows and what the query
/// is matched against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpEntry {
    /// The session's identity.
    pub id: SessionId,
    /// The session's title.
    pub title: String,
    /// The name of the session's project.
    pub project: String,
    /// The name of the repository the session works in; empty for a session of the whole
    /// project.
    pub repository: String,
    /// The title of the session this one was spawned from, if it was spawned.
    pub parent: Option<String>,
    /// Where the session stands.
    pub status: Status,
    /// How many background tasks the session's agent runs.
    pub background: usize,
    /// Whether the session waits for its orchestrator, not for the user.
    pub delegated: bool,
    /// When the session was archived, `None` while it is live.
    pub archived: Option<Archived>,
}

/// The sessions a search lists, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpModel {
    /// The listed sessions, at most [`ROWS`](Self::ROWS) of them.
    pub entries: Vec<JumpEntry>,
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

impl JumpModel {
    /// The most sessions a search lists.
    pub const ROWS: usize = 8;

    /// The sessions of `tree` that match `query`, archived ones only when `include_archived`
    /// is set and then after the live ones. Each part is ordered by urgency first, whatever
    /// the query, then the best matches first, and equally good ones keep the tree's order;
    /// the list is cut to [`ROWS`](Self::ROWS) after that. An empty query matches every
    /// session.
    #[must_use]
    pub fn build(tree: &TreeModel, query: &str, include_archived: bool) -> Self {
        let mut ranked: Vec<(Rank, JumpEntry)> = entries(tree, include_archived)
            .into_iter()
            .filter_map(|entry| Some((rank(&entry, query)?, entry)))
            .collect();
        // The sort is stable, so equally urgent, equally good matches keep the tree's order.
        ranked.sort_by_key(|(rank, entry)| {
            (
                entry.archived.is_some(),
                urgency(entry.status, entry.background, entry.delegated),
                *rank,
            )
        });
        Self {
            entries: ranked
                .into_iter()
                .take(Self::ROWS)
                .map(|(_, entry)| entry)
                .collect(),
        }
    }
}

/// Every live session of every project of `tree` in the tree's order, each followed by the
/// sessions spawned from it, then, when `include_archived` is set, the archived ones in the
/// same way.
fn entries(tree: &TreeModel, include_archived: bool) -> Vec<JumpEntry> {
    let mut found = Vec::new();
    for project in &tree.projects {
        collect(&project.sessions, &project.name, None, &mut found);
    }
    if include_archived {
        for project in &tree.projects {
            collect(&project.archived, &project.name, None, &mut found);
        }
    }
    found
}

/// Appends `sessions` of the project named `project`, spawned from the session titled
/// `parent` if any, to `found`, each followed by the sessions spawned from it.
fn collect(
    sessions: &[SessionNode],
    project: &str,
    parent: Option<&str>,
    found: &mut Vec<JumpEntry>,
) {
    for session in sessions {
        found.push(JumpEntry {
            id: session.id.clone(),
            title: session.title.clone(),
            project: project.to_owned(),
            repository: session.scope_label.clone(),
            parent: parent.map(str::to_owned),
            status: session.status,
            background: session.background,
            delegated: session.delegated,
            archived: session.archived,
        });
        collect(&session.children, project, Some(&session.title), found);
    }
}

/// How well `entry` matches `query`, or `None` when it does not: the query's characters
/// have to appear in order in the session's title, project, repository and parent title,
/// whatever their case.
fn rank(entry: &JumpEntry, query: &str) -> Option<Rank> {
    let query = query.trim().to_lowercase();
    let title = entry.title.to_lowercase();
    let searchable = [
        title.as_str(),
        &entry.project,
        &entry.repository,
        entry.parent.as_deref().unwrap_or_default(),
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

/// Where a session at `status` running `background` stands in the list, lowest first: a
/// session waiting for the user, then a running one or one whose background tasks run after its
/// turn, then a failed, interrupted or idle one, and a done one or one that is `delegated`,
/// waiting for its orchestrator, last.
fn urgency(status: Status, background: usize, delegated: bool) -> u8 {
    match status {
        _ if delegated => 3,
        Status::WaitingForYou => 0,
        _ if background_shown(status, background) => 1,
        Status::Running => 1,
        Status::Failed | Status::Interrupted | Status::Idle => 2,
        Status::Done => 3,
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

    use super::*;
    use crate::ids::ProjectId;
    use crate::session::Fold;
    use crate::view_model::ProjectNode;

    /// A session row of a tree, titled `title`, working in `repository` (empty for the whole
    /// project), with the sessions spawned from it.
    fn node(title: &str, repository: &str, children: Vec<SessionNode>) -> SessionNode {
        SessionNode {
            id: SessionId::new(),
            title: title.into(),
            status: Status::Idle,
            background: 0,
            attention: false,
            delegated: false,
            scope_label: repository.into(),
            backend: "claude".into(),
            orchestrator: false,
            usage: None,
            first_prompt: String::new(),
            skill: None,
            active_at: 0,
            expanded: false,
            children,
            archived: None,
            step: None,
        }
    }

    /// A session row of the whole project, titled `title`, that stands at `status`.
    fn with_status(title: &str, status: Status) -> SessionNode {
        SessionNode {
            status,
            ..node(title, "", Vec::new())
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
                instruction_files: Vec::new(),
                sessions,
                archived: Vec::new(),
                repositories: Vec::new(),
                repository_count: 0,
                cleaning: false,
                fold: Fold::Open,
            })
            .collect();
        TreeModel {
            projects,
            opening: Vec::new(),
        }
    }

    /// The titles a search for `query` lists, top to bottom.
    fn titles(tree: &TreeModel, query: &str) -> Vec<String> {
        JumpModel::build(tree, query, false)
            .entries
            .into_iter()
            .map(|entry| entry.title)
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
    fn an_entry_knows_its_project_repository_and_parent() {
        let tree = shop_and_taffle();

        let entries = entries(&tree, false);

        let migration = &entries[2];
        assert_eq!(migration.title, "Write the migration");
        assert_eq!(migration.project, "shop-app");
        assert_eq!(migration.repository, "backend");
        assert_eq!(migration.parent.as_deref(), Some("Gift card API"));
        let top = &entries[4];
        assert_eq!(top.project, "taffle");
        assert_eq!(top.parent, None);
    }

    #[test]
    fn an_entry_carries_the_sessions_status_background_and_archive_mark() {
        let mut waiting = with_status("asking", Status::WaitingForYou);
        waiting.background = 2;
        waiting.delegated = true;
        let id = waiting.id.clone();
        let mut tree = tree(vec![("work", vec![waiting])]);
        let mut old = with_status("old", Status::Done);
        old.archived = Some(Archived { at: 7 });
        tree.projects[0].archived.push(old);

        let found = JumpModel::build(&tree, "", true).entries;

        assert_eq!(found[0].id, id);
        assert_eq!(found[0].status, Status::WaitingForYou);
        assert_eq!(found[0].background, 2);
        assert!(found[0].delegated);
        assert_eq!(found[0].archived, None);
        assert_eq!(found[1].archived, Some(Archived { at: 7 }));
        assert!(!found[1].delegated);
        assert_eq!(found[1].background, 0);
    }

    #[test]
    fn the_list_holds_at_most_eight_sessions() {
        let sessions = (0..12)
            .map(|number| node(&format!("session {number}"), "", Vec::new()))
            .collect();
        let tree = tree(vec![("work", sessions)]);

        let listed = titles(&tree, "");

        assert_eq!(listed.len(), JumpModel::ROWS);
        assert_eq!(JumpModel::ROWS, 8);
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
        let all = entries(&tree, false);
        assert_eq!(
            rank(&all[2], "API"),
            Some(Rank::Title),
            "the case of the query does not count"
        );
        assert_eq!(rank(&all[1], "api"), Some(Rank::Contiguous));
        assert_eq!(rank(&all[0], "api"), Some(Rank::Scattered));
        assert_eq!(rank(&all[5], "api"), None);
    }

    #[test]
    fn waiting_sessions_come_first_then_running_then_the_rest_and_done_last() {
        let tree = tree(vec![(
            "work",
            vec![
                with_status("finished", Status::Done),
                with_status("quiet", Status::Idle),
                with_status("busy", Status::Running),
                with_status("broken", Status::Failed),
                with_status("asking", Status::WaitingForYou),
                with_status("cut off", Status::Interrupted),
            ],
        )]);

        assert_eq!(
            titles(&tree, ""),
            ["asking", "busy", "quiet", "broken", "cut off", "finished"]
        );
    }

    #[test]
    fn status_comes_before_how_well_the_query_matches() {
        // "fix" is in the done session's title, only scattered in the waiting one's.
        let tree = tree(vec![(
            "work",
            vec![
                with_status("fix login", Status::Done),
                with_status("find a box", Status::WaitingForYou),
            ],
        )]);

        assert_eq!(titles(&tree, "fix"), ["find a box", "fix login"]);
    }

    #[test]
    fn a_waiting_session_survives_the_cut_below_many_done_ones() {
        let mut sessions: Vec<SessionNode> = (0..10)
            .map(|number| with_status(&format!("done {number}"), Status::Done))
            .collect();
        sessions.push(with_status("asking", Status::WaitingForYou));
        let tree = tree(vec![("work", sessions)]);

        let listed = titles(&tree, "");

        assert_eq!(listed.len(), JumpModel::ROWS);
        assert_eq!(listed[0], "asking");
    }

    #[test]
    fn archived_sessions_follow_live_ones_whatever_their_status() {
        let mut tree = tree(vec![("work", vec![with_status("finished", Status::Done)])]);
        let mut asking = with_status("asking", Status::WaitingForYou);
        asking.archived = Some(Archived { at: 0 });
        tree.projects[0].archived.push(asking);

        let with: Vec<String> = JumpModel::build(&tree, "", true)
            .entries
            .into_iter()
            .map(|entry| entry.title)
            .collect();

        assert_eq!(with, ["finished", "asking"]);
        assert_eq!(titles(&tree, ""), ["finished"]);
    }

    #[test]
    fn a_done_session_running_background_tasks_lists_with_the_running_ones() {
        let mut working = with_status("working", Status::Done);
        working.background = 1;
        let tree = tree(vec![(
            "work",
            vec![
                with_status("finished", Status::Done),
                working,
                with_status("quiet", Status::Idle),
                with_status("busy", Status::Running),
            ],
        )]);

        assert_eq!(titles(&tree, ""), ["working", "busy", "quiet", "finished"]);
    }

    #[test]
    fn a_session_waiting_for_its_orchestrator_lists_with_the_done_ones() {
        let mut delegated = with_status("deferring", Status::WaitingForYou);
        delegated.delegated = true;
        let mut delegated_running = with_status("deferring busy", Status::Running);
        delegated_running.delegated = true;
        let tree = tree(vec![(
            "work",
            vec![
                delegated,
                delegated_running,
                with_status("finished", Status::Done),
                with_status("quiet", Status::Idle),
            ],
        )]);

        assert_eq!(
            titles(&tree, ""),
            ["quiet", "deferring", "deferring busy", "finished"]
        );
    }

    #[test]
    fn urgency_ranks_each_status() {
        assert_eq!(urgency(Status::WaitingForYou, 0, false), 0);
        assert_eq!(urgency(Status::Running, 0, false), 1);
        assert_eq!(urgency(Status::Idle, 1, false), 1);
        assert_eq!(urgency(Status::Done, 1, false), 1);
        assert_eq!(urgency(Status::Failed, 1, false), 2);
        assert_eq!(urgency(Status::Idle, 0, false), 2);
        assert_eq!(urgency(Status::Failed, 0, false), 2);
        assert_eq!(urgency(Status::Interrupted, 0, false), 2);
        assert_eq!(urgency(Status::Done, 0, false), 3);
        assert_eq!(urgency(Status::WaitingForYou, 0, true), 3);
        assert_eq!(urgency(Status::Running, 2, true), 3);
    }
}
