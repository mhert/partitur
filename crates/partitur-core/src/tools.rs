//! The catalog of Partitur's own tools: every tool's name, the call as the orchestrator's
//! preamble shows it, its description and the roles of the sessions it is offered to. The tool
//! server lists and accepts a tool by this catalog, and the preamble lists the orchestration
//! tools from it. The workflow reporting tools are not in it: each workflow module describes
//! its own (`WorkflowModule::tools`).
//!
//! This is the description of the tools, not their handling: what a call does once it reached
//! the core is `app::tools`.

use crate::tool_call::ToolRole;

/// One of Partitur's own tools, as the tool server lists it and the preamble describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    /// The tool's name, without the server prefix.
    pub name: &'static str,
    /// The call as the preamble shows it: the name and its parameters.
    pub signature: &'static str,
    /// What the tool does, as one sentence fragment following the signature.
    pub description: &'static str,
    /// What the tool does as an orchestrator is told, when that differs from
    /// [`description`](Self::description).
    pub orchestrator_description: Option<&'static str>,
    /// The roles of the sessions the tool is offered to.
    pub roles: &'static [ToolRole],
}

impl ToolSpec {
    /// What the tool does as a session in `role` is told.
    #[must_use]
    pub fn description_for(&self, role: ToolRole) -> &'static str {
        match self.orchestrator_description {
            Some(description) if role == ToolRole::Orchestrator => description,
            _ => self.description,
        }
    }

    /// Whether the tool is an orchestration tool, one only an orchestrator is offered.
    #[must_use]
    pub fn is_orchestration(&self) -> bool {
        self.roles == ORCHESTRATOR
    }

    /// Whether a session in `role` is offered the tool.
    #[must_use]
    pub fn is_offered_to(&self, role: ToolRole) -> bool {
        self.roles.contains(&role)
    }
}

/// Every role there is.
const EVERY_ROLE: &[ToolRole] = &[ToolRole::Plain, ToolRole::Orchestrator, ToolRole::Child];

/// Only an orchestrator.
const ORCHESTRATOR: &[ToolRole] = &[ToolRole::Orchestrator];

/// Every one of Partitur's own tools, in the order the tool server lists them: the
/// orchestration tools, the peer tools, the title tools and `ask_user` last.
pub const CATALOG: &[ToolSpec] = &[
    ToolSpec {
        name: "propose_children",
        signature: "propose_children(children: [{repository, prompt, title}])",
        description: "proposes one child session per entry. The proposal is shown to the user for approval; end your turn after calling it. You are woken with the started children and their session ids, or with the rejection and the user's note.",
        orchestrator_description: None,
        roles: ORCHESTRATOR,
    },
    ToolSpec {
        name: "send_to_child",
        signature: "send_to_child(session_id, prompt, decision?)",
        description: "sends a follow-up prompt to a child; it is queued while the child is mid-turn unless a gate or question is open. At a gate the child waits at, decision \"approve\" approves it, with the prompt after the approval when given, and \"changes\" asks for the changes the prompt names; a child that asks a question is answered by a prompt without a decision.",
        orchestrator_description: None,
        roles: ORCHESTRATOR,
    },
    ToolSpec {
        name: "list_children",
        signature: "list_children()",
        description: "lists your children with their title, repository, session id and status and, for a child that has them, its workflow step and the gate or question it waits at.",
        orchestrator_description: None,
        roles: ORCHESTRATOR,
    },
    ToolSpec {
        name: "cancel_child",
        signature: "cancel_child(session_id)",
        description: "stops a child's current turn; its result then reaches you as a child result marked cancelled.",
        orchestrator_description: None,
        roles: ORCHESTRATOR,
    },
    ToolSpec {
        name: "get_child_result",
        signature: "get_child_result(session_id)",
        description: "returns a child's last final message and its status and, when it has them, its workflow step and the gate or question it waits at.",
        orchestrator_description: None,
        roles: ORCHESTRATOR,
    },
    ToolSpec {
        name: "propose_session",
        signature: "propose_session(title, prompt, repository?, base?)",
        description: "proposes one new session in this project for work that should run on its own, such as a hand-off. The user approves it on a card before it starts; end your turn after calling it. You are woken with its session id, or with the rejection and the user's note. repository defaults to yours; base suggests the branch a new branch starts from.",
        orchestrator_description: None,
        roles: &[ToolRole::Plain],
    },
    ToolSpec {
        name: "list_sessions",
        signature: "list_sessions()",
        description: "lists this project's sessions, archived ones included, with their title, repository, session id and status. Use it to find a session to pick up from or learn from.",
        orchestrator_description: None,
        roles: EVERY_ROLE,
    },
    ToolSpec {
        name: "read_session",
        signature: "read_session(session_id, page?)",
        description: "returns a session's conversation as text, a page at a time: the user's prompts, the agent's messages and one line per tool call. Without page it returns the last page; each page names the one before it.",
        orchestrator_description: None,
        roles: EVERY_ROLE,
    },
    ToolSpec {
        name: "rename_session",
        signature: "rename_session(title, session_id?)",
        description: "renames this session. The title replaces any earlier one, also one the user typed.",
        orchestrator_description: Some(
            "renames this session, or the child session_id. The title replaces any earlier one, also one the user typed: list_children marks such a title \"set by you\", so weigh it against a child's proposal before you replace it.",
        ),
        roles: &[ToolRole::Plain, ToolRole::Orchestrator],
    },
    ToolSpec {
        name: "propose_title",
        signature: "propose_title(title)",
        description: "proposes a new title for this session to your orchestrator, which decides. Call it when your work drifted from your title.",
        orchestrator_description: None,
        roles: &[ToolRole::Child],
    },
    ToolSpec {
        name: "ask_user",
        signature: "ask_user(question, options)",
        description: "asks the user to choose between 2 to 6 named options; the user picks one or answers in their own words. End your turn after calling it: the answer arrives as your next prompt.",
        orchestrator_description: None,
        roles: EVERY_ROLE,
    },
];

/// The tools of [`CATALOG`] a session in `role` is offered, in catalog order.
pub fn offered(role: ToolRole) -> impl Iterator<Item = &'static ToolSpec> {
    CATALOG.iter().filter(move |spec| spec.is_offered_to(role))
}

/// The orchestration tools: those only an orchestrator is offered, in catalog order. The
/// preamble lists them, and the tool server lists them before the workflow tools.
pub fn orchestration() -> impl Iterator<Item = &'static ToolSpec> {
    CATALOG.iter().filter(|spec| spec.is_orchestration())
}

/// The tool of [`CATALOG`] named `name`, `None` when there is none.
#[must_use]
pub fn find(name: &str) -> Option<&'static ToolSpec> {
    CATALOG.iter().find(|spec| spec.name == name)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::collections::HashSet;

    use super::*;

    fn names(specs: impl Iterator<Item = &'static ToolSpec>) -> Vec<&'static str> {
        specs.map(|spec| spec.name).collect()
    }

    #[test]
    fn catalog_names_are_unique() {
        let unique: HashSet<_> = CATALOG.iter().map(|spec| spec.name).collect();

        assert_eq!(unique.len(), CATALOG.len());
    }

    #[test]
    fn each_role_is_offered_its_tools() {
        assert_eq!(
            names(offered(ToolRole::Plain)),
            [
                "propose_session",
                "list_sessions",
                "read_session",
                "rename_session",
                "ask_user"
            ]
        );
        assert_eq!(
            names(offered(ToolRole::Orchestrator)),
            [
                "propose_children",
                "send_to_child",
                "list_children",
                "cancel_child",
                "get_child_result",
                "list_sessions",
                "read_session",
                "rename_session",
                "ask_user"
            ]
        );
        assert_eq!(
            names(offered(ToolRole::Child)),
            ["list_sessions", "read_session", "propose_title", "ask_user"]
        );
    }

    #[test]
    fn the_orchestration_tools_are_those_only_an_orchestrator_gets() {
        assert_eq!(
            names(orchestration()),
            [
                "propose_children",
                "send_to_child",
                "list_children",
                "cancel_child",
                "get_child_result"
            ]
        );
    }

    #[test]
    fn find_names_a_catalogued_tool_only() {
        assert_eq!(find("ask_user").map(|spec| spec.name), Some("ask_user"));
        assert_eq!(find("rename_child"), None);
    }

    #[test]
    fn only_rename_tells_an_orchestrator_something_else() {
        let rename = find("rename_session").unwrap();

        assert!(
            rename
                .description_for(ToolRole::Orchestrator)
                .starts_with("renames this session, or the child session_id.")
        );
        assert_eq!(rename.description_for(ToolRole::Plain), rename.description);
        assert_eq!(rename.description_for(ToolRole::Child), rename.description);
        for spec in CATALOG.iter().filter(|spec| spec.name != "rename_session") {
            assert_eq!(
                spec.description_for(ToolRole::Orchestrator),
                spec.description
            );
        }
    }
}
