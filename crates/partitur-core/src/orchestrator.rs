//! What makes a session an orchestrator: when it is one, the preamble it starts with, the
//! tool calls it may not make, the workflow folders it may still edit in, and every prompt
//! Partitur writes to it. The preamble and the prompt that tells it its children started say
//! that a child's gates and questions reach it as `child waits` prompts, for it to decide; a
//! child's first prompt carries the note that tells the child so too.

use std::path::{Component, Path, PathBuf};

use crate::backend::{
    ApprovedChild, ChildOutcome, ChildState, ChildWait, StopReason, TitleProposal, ToolKind,
    TurnOrigin,
};
use crate::context::SessionContext;
use crate::files::{FilesAnswer, FilesError, FilesQuery, ResolvedPath};
use crate::ids::{RepositoryId, SessionId};
use crate::project::{Project, Repository};
use crate::session::{BranchMode, Scope};
use crate::tools;
use crate::workflow::{ModuleId, document_folders, orchestrator_note};

/// The result an orchestrator's `propose_children` call returns at once, before the user
/// decides the proposal.
pub const PROPOSAL_RECORDED: &str = "proposal recorded, awaiting approval";

/// The instruction file whose first line describes a repository in the preamble.
pub(crate) const INSTRUCTIONS_FILE: &str = "AGENTS.md";

/// What the preamble says about a repository without a usable instruction file.
const NO_INSTRUCTIONS: &str = "(no AGENTS.md)";

/// The name Partitur's tool server is registered and served under.
pub const TOOL_SERVER: &str = "partitur";

/// The preamble from its role statement up to the tool list.
const PREAMBLE_INTRO: &str = "You are the orchestrator of this project. You may inspect but never change the repositories yourself: edit, delete, move and execute requests are denied in this session, except edits in the workflow folders listed below. Work in the repositories happens through child sessions you propose.";

/// The preamble between the tool list and [`PREAMBLE_CHILD_WAITS`].
const PREAMBLE_OUTRO: &str = "Each child's final message reaches you as a prompt marked \"child result\". Report to the user whenever you have nothing further to drive.";

/// The preamble after [`PREAMBLE_OUTRO`]: how a child's gates and questions reach the
/// orchestrator and how it decides them.
const PREAMBLE_CHILD_WAITS: &str = "A child's gates and questions reach you as prompts marked \"child waits\". Decide a gate with send_to_child(session_id, decision, prompt) and answer a question with send_to_child(session_id, prompt). When the user should decide, ask with ask_user and pass the answer on. Never ask the user to open a child.";

/// The heading of the workflow folders in the preamble.
const WORKFLOW_FOLDERS_HEADING: &str =
    "Workflow folders (you may edit files inside them, relative to your working folder):";

/// The closing sentences of the prompt that tells an orchestrator its children started.
const CHILDREN_STARTED_FOOTER: &str = "Each child's final message will reach you as a prompt marked \"child result\", and a child that waits at a gate or with a question as a prompt marked \"child waits\". Use send_to_child(session_id, prompt) for follow-ups.";

/// The note a child's first prompt carries after the note that names its title: it works
/// for an orchestrator, which decides its gates and answers its questions.
pub(crate) const CHILD_ROLE: &str = "You work for an orchestrator. When your workflow opens a gate or you ask with ask_user, end your turn: the orchestrator decides and answers in your next prompt.";

/// The text an orchestrator session starts its first prompt with: its role, Partitur's
/// tools, how its children's gates and questions reach it, the workflow folders it may edit
/// in, who runs which superpowers step and the project's repositories.
///
/// Every repository except the project root itself is listed in project order, with the
/// first non-blank line of its `AGENTS.md` in `context`, trimmed, or `(no AGENTS.md)` when
/// that file was absent, unreadable or blank. The workflow folders take BMAD's configuration
/// from `context`, which names its output folder.
pub(crate) fn preamble(project: &Project, context: &SessionContext) -> String {
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
        tools::orchestration().map(|spec| format!("- {}: {}", spec.signature, spec.description)),
    );
    lines.extend([
        String::new(),
        PREAMBLE_OUTRO.to_owned(),
        String::new(),
        PREAMBLE_CHILD_WAITS.to_owned(),
        String::new(),
        WORKFLOW_FOLDERS_HEADING.to_owned(),
    ]);
    lines.extend(
        workflow_folders(context)
            .iter()
            .map(|folder| format!("- {}/", folder.display())),
    );
    for note in ModuleId::ALL.into_iter().filter_map(orchestrator_note) {
        lines.extend([String::new(), note.to_owned()]);
    }
    lines.extend([String::new(), "Repositories:".to_owned()]);
    lines.extend(
        orchestrated_repositories(project)
            .into_iter()
            .map(|repository| {
                let text = context.agents_md_of(&repository.path);
                format!("- {}: {}", repository.name, first_line(text))
            }),
    );
    lines.push("</orchestrator>".to_owned());
    lines.join("\n")
}

/// The first non-blank line of a repository's `AGENTS.md` text, trimmed, or
/// `(no AGENTS.md)` when there is none.
fn first_line(text: Option<&str>) -> String {
    text.and_then(|content| {
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

/// What the permission check of an orchestrator's tool call looks at: the call's kind and
/// the paths it names, the orchestrator's working folder and its children's worktrees.
///
/// [`check`](Self::check) is pure: it decides on the facts a [`FilesQuery::Resolve`] of
/// [`paths`](Self::paths) found and on the workflow configuration the
/// [`queries`](Self::queries) read, so the file system is asked once per request, in the
/// background. A path the facts do not cover counts as one that cannot be resolved, so a
/// failed or partial answer never lets a call through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathCheck {
    kind: ToolKind,
    locations: Vec<PathBuf>,
    cwd: PathBuf,
    worktrees: Vec<PathBuf>,
}

/// What [`PathCheck::check`] found about an orchestrator's call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PathDecision {
    /// The call is a read of paths that all lie inside its children's worktrees, as the
    /// orchestrator may read without asking.
    pub(crate) reads_in_worktrees: bool,
    /// The call is an edit of paths that all lie inside its workflow folders, as the
    /// orchestrator may edit despite [`restrained`].
    pub(crate) writes_in_workflow_folders: bool,
}

impl PathCheck {
    /// The check of a call of `kind` naming `locations`, from an orchestrator working in
    /// `cwd` whose children work in the worktrees `worktrees`.
    pub(crate) fn new(
        kind: ToolKind,
        locations: &[PathBuf],
        cwd: &Path,
        worktrees: &[&Path],
    ) -> Self {
        Self {
            kind,
            locations: locations.to_vec(),
            cwd: cwd.to_path_buf(),
            worktrees: worktrees
                .iter()
                .map(|worktree| worktree.to_path_buf())
                .collect(),
        }
    }

    /// Whether the decision depends on the file system: the call is a read or an edit naming
    /// at least one path. Any other call is decided without facts, as
    /// [`PathDecision::default`].
    pub(crate) fn needs_facts(&self) -> bool {
        matches!(self.kind, ToolKind::Read | ToolKind::Edit) && !self.locations.is_empty()
    }

    /// The queries whose answers [`check`](Self::check) decides on: one
    /// [`FilesQuery::Resolve`] of the [`paths`](Self::paths), then a [`FilesQuery::Read`] of
    /// each workflow module's configuration file in the working folder, in [`ModuleId::ALL`]
    /// order, so the folders are those its configuration names when the call is made.
    pub(crate) fn queries(&self) -> Vec<FilesQuery> {
        let mut queries = vec![FilesQuery::Resolve(self.paths())];
        queries.extend(
            self.config_files()
                .into_iter()
                .map(|(_, path)| FilesQuery::Read(path)),
        );
        queries
    }

    /// Decides on the `answers` to [`queries`](Self::queries), as [`check`](Self::check)
    /// does. A resolution that failed resolves nothing, and a configuration file that could
    /// not be read counts as absent, as when it is missing.
    pub(crate) fn decide(&self, answers: Vec<Result<FilesAnswer, FilesError>>) -> PathDecision {
        let mut answers = answers.into_iter();
        let resolved = match answers.next() {
            Some(Ok(FilesAnswer::Resolved(resolved))) => resolved,
            _ => Vec::new(),
        };
        let mut context = SessionContext::default();
        for ((module, _), answer) in self.config_files().into_iter().zip(answers) {
            if let Ok(FilesAnswer::Read(Some(text))) = answer {
                context.workflow_config.push((module, text));
            }
        }
        self.check(&resolved, &context)
    }

    /// Every path whose facts [`check`](Self::check) may look at, each once, as written: the
    /// working folder, the worktrees and, for each absolute location, the location with each
    /// folder above it and each of its leading parts.
    pub(crate) fn paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut add = |path: &Path| {
            if !paths
                .iter()
                .any(|known| known.as_os_str() == path.as_os_str())
            {
                paths.push(path.to_path_buf());
            }
        };
        add(&self.cwd);
        for worktree in &self.worktrees {
            add(worktree);
        }
        for location in self
            .locations
            .iter()
            .filter(|location| location.is_absolute())
        {
            for ancestor in location.ancestors() {
                add(ancestor);
            }
            let mut prefix = PathBuf::new();
            for component in location.components() {
                prefix.push(component);
                add(&prefix);
            }
        }
        paths
    }

    /// What the call may do without asking or despite [`restrained`], from the facts
    /// `resolved` and the workflow configuration in `context`, as [`reads_only_in`] and
    /// [`writes_only_in_workflow_folders`] decide it.
    pub(crate) fn check(
        &self,
        resolved: &[ResolvedPath],
        context: &SessionContext,
    ) -> PathDecision {
        let facts = Facts(resolved);
        PathDecision {
            reads_in_worktrees: reads_only_in(self.kind, &self.locations, &self.worktrees, facts),
            writes_in_workflow_folders: writes_only_in_workflow_folders(
                self.kind,
                &self.locations,
                &self.cwd,
                context,
                facts,
            ),
        }
    }

    /// Each workflow module's configuration file in the working folder, with its module.
    fn config_files(&self) -> Vec<(ModuleId, PathBuf)> {
        ModuleId::ALL
            .into_iter()
            .filter_map(|module| Some((module, self.cwd.join(module.config_file()?))))
            .collect()
    }
}

/// The facts a resolution found, looked up by the path exactly as it was asked about.
#[derive(Clone, Copy)]
struct Facts<'a>(&'a [ResolvedPath]);

impl<'a> Facts<'a> {
    /// What was found about `path`, `None` when it was not asked about.
    fn of(self, path: &Path) -> Option<&'a ResolvedPath> {
        self.0
            .iter()
            .find(|fact| fact.given.as_os_str() == path.as_os_str())
    }

    /// The canonical form of `path`, `None` when it cannot be resolved or was not asked
    /// about.
    fn canonical(self, path: &Path) -> Option<&'a PathBuf> {
        self.of(path)?.canonical.as_ref()
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
fn reads_only_in(
    kind: ToolKind,
    locations: &[PathBuf],
    worktrees: &[PathBuf],
    facts: Facts<'_>,
) -> bool {
    let roots: Vec<&PathBuf> = worktrees
        .iter()
        .filter_map(|worktree| facts.canonical(worktree))
        .collect();
    kind == ToolKind::Read
        && !locations.is_empty()
        && locations.iter().all(|location| {
            location.is_absolute()
                && facts
                    .canonical(location)
                    .is_some_and(|real| roots.iter().any(|root| real.starts_with(root)))
        })
}

/// Whether an orchestrator working in `cwd` may edit despite [`restrained`]: the call is a
/// [`ToolKind::Edit`], it names at least one path and every path lies inside one of the
/// [`workflow_folders`] its `context` names, relative to `cwd`.
///
/// A path must be absolute. It is resolved through its deepest existing ancestor, since a
/// file about to be written and often its folder do not exist yet, so a `..` or a link
/// inside a folder that leads out of it does not count, and neither does a dangling link, a
/// `..` below that ancestor, nor a `cwd` that cannot be resolved. Like [`reads_only_in`], the
/// check is a convenience for the user, not a sandbox.
fn writes_only_in_workflow_folders(
    kind: ToolKind,
    locations: &[PathBuf],
    cwd: &Path,
    context: &SessionContext,
    facts: Facts<'_>,
) -> bool {
    if kind != ToolKind::Edit || locations.is_empty() {
        return false;
    }
    let Some(real_cwd) = facts.canonical(cwd) else {
        return false;
    };
    let folders: Vec<PathBuf> = workflow_folders(context)
        .iter()
        .map(|folder| real_cwd.join(folder))
        .collect();
    locations.iter().all(|location| {
        resolved(location, facts)
            .is_some_and(|real| folders.iter().any(|folder| real.starts_with(folder)))
    })
}

/// The folders, relative to an orchestrator's working folder, it may write workflow
/// documents in: those of every workflow module in [`ModuleId::ALL`] order, each given the
/// text of its configuration file in the working folder when `context` holds it.
pub(crate) fn workflow_folders(context: &SessionContext) -> Vec<PathBuf> {
    ModuleId::ALL
        .into_iter()
        .flat_map(|module| document_folders(module, context.config_of(module)))
        .collect()
}

/// `path` with every link and `..` resolved, through its deepest existing ancestor: `None`
/// for a relative path, a dangling link, or a path with a `..` below its deepest existing
/// ancestor, which cannot be resolved without guessing, and for a path the facts do not
/// cover.
fn resolved(path: &Path, facts: Facts<'_>) -> Option<PathBuf> {
    if !path.is_absolute() || steps_back_below_missing(path, facts)? {
        return None;
    }
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        let fact = facts.of(existing)?;
        if let Some(real) = &fact.canonical {
            return Some(
                missing
                    .iter()
                    .rev()
                    .fold(real.clone(), |real, name| real.join(name)),
            );
        }
        if fact.present {
            return None;
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

/// Whether `path` has a `..` after a folder that does not exist, `None` when the facts do
/// not cover a part it needs. Windows drops such a step with the name before it as written,
/// where other systems fail on the missing folder, so [`resolved`] refuses it everywhere.
fn steps_back_below_missing(path: &Path, facts: Facts<'_>) -> Option<bool> {
    let mut prefix = PathBuf::new();
    let mut missing = false;
    for component in path.components() {
        if missing && component == Component::ParentDir {
            return Some(true);
        }
        prefix.push(component);
        missing = missing || !facts.of(&prefix)?.present;
    }
    Some(false)
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
    lines.extend(children.iter().map(started_line));
    lines.push(CHILDREN_STARTED_FOOTER.to_owned());
    lines.join("\n")
}

/// The line that names a `started` session: its title, repository, session id, mode
/// (`worktree`, `new branch` or `checked-out`), the branch it made when it made one, and the
/// folder it works in.
pub(crate) fn started_line(started: &ApprovedChild) -> String {
    let mode = match started.mode {
        BranchMode::Worktree => "worktree",
        BranchMode::NewBranch => "new branch",
        BranchMode::CheckedOut => "checked-out",
    };
    let branch = started
        .branch
        .as_ref()
        .map_or_else(String::new, |branch| format!(" {branch}"));
    format!(
        "- {} · {} · session {} · {mode}{branch} · in {}",
        started.title,
        started.repository,
        started.session,
        started.cwd.display()
    )
}

/// The prompt that wakes an orchestrator after the user rejected its proposal, with the
/// user's note when it is not blank.
pub(crate) fn rejection_prompt(note: Option<&str>) -> String {
    match note.filter(|note| !note.trim().is_empty()) {
        Some(note) => format!("proposal rejected by the user\nnote: {note}"),
        None => "proposal rejected by the user".to_owned(),
    }
}

/// The closing lines of a `child waits` prompt for a child at a gate.
const GATE_FOOTER: &str = "Decide with send_to_child(session_id, decision: \"approve\" or \"changes\", prompt). Ask the user with ask_user first when the user should make this decision.";

/// The closing lines of a `child waits` prompt for a child with an open question.
const QUESTION_FOOTER: &str = "Answer with send_to_child(session_id, prompt); the prompt is the child's answer. Ask the user with ask_user first when the user should answer.";

/// The line that tells an orchestrator where a child stands: its workflow step, then what it
/// waits for, `<step> · waits at its <word>` or `<step> · asks: <question>`, with `<step> · `
/// left out for a child without a workflow. `None` for a child with neither.
pub(crate) fn state_line(state: &ChildState) -> Option<String> {
    let wait = state.waits.as_ref().map(|wait| match wait {
        ChildWait::Gate { word } => format!("waits at its {word}"),
        ChildWait::Question(question) => format!("asks: {}", question.text),
    });
    match (&state.step, wait) {
        (Some(step), Some(wait)) => Some(format!("{step} · {wait}")),
        (Some(step), None) => Some(step.clone()),
        (None, wait) => wait,
    }
}

/// The lines that tell an orchestrator where a child stands: the [`state_line`], and for an
/// open question a line with its options, `options: <label> · <label>`.
pub(crate) fn state_lines(state: &ChildState) -> Vec<String> {
    let mut lines: Vec<String> = state_line(state).into_iter().collect();
    if let Some(ChildWait::Question(question)) = &state.waits {
        let labels: Vec<&str> = question
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        lines.push(format!("options: {}", labels.join(" · ")));
    }
    lines
}

/// The prompt that reports a child's finished turn to its orchestrator: a header naming the
/// child, `child waits` when the child waits for the orchestrator at a gate or with a
/// question and `child result` otherwise, its session id, the [`state_lines`], a line for each
/// thing out of the ordinary (the user started the turn, it failed or ended for another
/// reason than the agent finishing), then a blank line and the child's final answer. A child
/// that waits gets a blank line and a footer that says how to decide or answer.
pub(crate) fn child_result_prompt(
    child: &SessionId,
    repository: &RepositoryId,
    title: &str,
    origin: TurnOrigin,
    outcome: &ChildOutcome,
    state: &ChildState,
    text: &str,
) -> String {
    let marker = if state.waits.is_some() {
        "child waits"
    } else {
        "child result"
    };
    let mut lines = vec![
        format!("{marker} · {repository} · {title}"),
        format!("session {child}"),
    ];
    lines.extend(state_lines(state));
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
    let footer = match &state.waits {
        Some(ChildWait::Gate { .. }) => format!("\n\n{GATE_FOOTER}"),
        Some(ChildWait::Question(_)) => format!("\n\n{QUESTION_FOOTER}"),
        None => String::new(),
    };
    format!("{}\n\n{text}{footer}", lines.join("\n"))
}

/// The closing line of a title proposal: what the orchestrator may do with it.
const PROPOSAL_DECISION: &str =
    "Accept it with rename_session(title, session_id), or keep the current title.";

/// The prompt that brings a child's title proposal to its orchestrator: a header naming the
/// child and its current title, its session id, the proposed title, when the user typed the
/// current title a line saying so, and what the orchestrator may do with it.
pub(crate) fn title_proposal_prompt(proposal: &TitleProposal) -> String {
    let TitleProposal {
        child,
        repository,
        title,
        proposed,
        by_user,
    } = proposal;
    let mut lines = vec![
        format!("title proposal · {repository} · {title}"),
        format!("session {child}"),
        format!("proposed title: {proposed}"),
    ];
    if *by_user {
        lines.push("(the current title was set by the user)".to_owned());
    }
    lines.push(PROPOSAL_DECISION.to_owned());
    lines.join("\n")
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
    #![allow(clippy::disallowed_methods)]
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::ids::ProjectId;
    use crate::project::Repository;
    use crate::question::{Question, QuestionOption};

    fn repository(name: &str, is_root: bool) -> Repository {
        Repository {
            id: RepositoryId::try_from(name).unwrap(),
            path: PathBuf::from("/work").join(name),
            name: name.into(),
            instruction_files: Vec::new(),
            is_root,
        }
    }

    fn project(repositories: Vec<Repository>) -> Project {
        Project {
            id: ProjectId::new(),
            root: PathBuf::from("/work"),
            name: "work".into(),
            repositories,
            instruction_files: Vec::new(),
            closed: false,
            fold: crate::Fold::Open,
        }
    }

    /// An orchestrator's context with `agents_md` for its repositories and BMAD's
    /// configuration `config`, when given.
    fn context(agents_md: &[(&str, &str)], config: Option<&str>) -> SessionContext {
        SessionContext {
            agents_md: agents_md
                .iter()
                .map(|(path, text)| (PathBuf::from(path), (*text).to_owned()))
                .collect(),
            workflow_config: config
                .map(|config| (ModuleId::BmadV6_12, config.to_owned()))
                .into_iter()
                .collect(),
            ..SessionContext::default()
        }
    }

    #[test]
    fn preamble_from_context() {
        let project = project(vec![repository("api", false), repository("web", false)]);
        let context = context(&[("/work/api", "\n  Keep the API compatible.\n")], None);

        let expected = format!(
            "<orchestrator project=\"{}\" root=\"{}\">
You are the orchestrator of this project. You may inspect but never change the repositories yourself: edit, delete, move and execute requests are denied in this session, except edits in the workflow folders listed below. Work in the repositories happens through child sessions you propose.

Tools (MCP server \"partitur\"):
- propose_children(children: [{{repository, prompt, title}}]): proposes one child session per entry. The proposal is shown to the user for approval; end your turn after calling it. You are woken with the started children and their session ids, or with the rejection and the user's note.
- send_to_child(session_id, prompt, decision?): sends a follow-up prompt to a child; it is queued while the child is mid-turn unless a gate or question is open. At a gate the child waits at, decision \"approve\" approves it, with the prompt after the approval when given, and \"changes\" asks for the changes the prompt names; a child that asks a question is answered by a prompt without a decision.
- list_children(): lists your children with their title, repository, session id and status and, for a child that has them, its workflow step and the gate or question it waits at.
- cancel_child(session_id): stops a child's current turn; its result then reaches you as a child result marked cancelled.
- get_child_result(session_id): returns a child's last final message and its status and, when it has them, its workflow step and the gate or question it waits at.

Each child's final message reaches you as a prompt marked \"child result\". Report to the user whenever you have nothing further to drive.

A child's gates and questions reach you as prompts marked \"child waits\". Decide a gate with send_to_child(session_id, decision, prompt) and answer a question with send_to_child(session_id, prompt). When the user should decide, ask with ask_user and pass the answer on. Never ask the user to open a child.

Workflow folders (you may edit files inside them, relative to your working folder):
- docs/superpowers/
- .superpowers/
- _bmad-output/

When you work with the superpowers skills, run brainstorming and writing-plans in this session yourself: write the design and the plan into docs/superpowers/ and open their gates here. Propose children only once the user approved the plan, and give each child the plan's absolute path and the tasks it executes. Checkpoints in the plan stop a child for you, not for the user.

Repositories:
- api: Keep the API compatible.
- web: (no AGENTS.md)
</orchestrator>",
            project.name,
            project.root.display()
        );
        assert_eq!(preamble(&project, &context), expected);
    }

    #[test]
    fn preamble_explains_child_waits() {
        let text = preamble(&project(vec![]), &SessionContext::default());

        assert!(
            text.contains("\n\nA child's gates and questions reach you as prompts marked \"child waits\". Decide a gate with send_to_child(session_id, decision, prompt) and answer a question with send_to_child(session_id, prompt). When the user should decide, ask with ask_user and pass the answer on. Never ask the user to open a child.\n\nWorkflow folders"),
            "{text}"
        );
        assert!(
            text.contains(
                " Checkpoints in the plan stop a child for you, not for the user.\n\nRepositories:"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_child_is_told_that_its_orchestrator_decides() {
        assert_eq!(
            CHILD_ROLE,
            "You work for an orchestrator. When your workflow opens a gate or you ask with ask_user, end your turn: the orchestrator decides and answers in your next prompt."
        );
    }

    #[test]
    fn every_tool_is_listed_in_the_preamble() {
        let text = preamble(&project(vec![]), &SessionContext::default());

        for spec in tools::orchestration() {
            let line = format!("- {}: {}", spec.signature, spec.description);
            assert!(text.contains(&line), "missing {line:?}");
        }
    }

    #[test]
    fn the_tool_server_is_named_partitur() {
        assert_eq!(TOOL_SERVER, "partitur");
        assert!(
            preamble(&project(vec![]), &SessionContext::default())
                .contains("Tools (MCP server \"partitur\"):")
        );
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
                "children started:\n- Fix API · api · session {first} · checked-out · in /p/api\n- Fix web · web · session {second} · checked-out · in /p/web\nEach child's final message will reach you as a prompt marked \"child result\", and a child that waits at a gate or with a question as a prompt marked \"child waits\". Use send_to_child(session_id, prompt) for follow-ups."
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
            child_result_prompt(
                &first,
                &api,
                "Fix API",
                origin,
                &outcome,
                &ChildState::default(),
                "Done.",
            )
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

    fn merge_or_pr() -> ChildWait {
        ChildWait::Question(Question {
            text: "Merge or PR?".into(),
            options: ["Merge", "PR"]
                .into_iter()
                .map(|label| QuestionOption {
                    label: label.into(),
                    description: None,
                })
                .collect(),
        })
    }

    fn checkpoint() -> ChildWait {
        ChildWait::Gate {
            word: "checkpoint".into(),
        }
    }

    #[test]
    fn the_state_line_names_the_step_and_the_wait() {
        let state = |step: Option<&str>, waits| ChildState {
            step: step.map(str::to_owned),
            waits,
        };

        assert_eq!(state_line(&state(None, None)), None);
        assert_eq!(
            state_line(&state(Some("execute · 4/11"), None)).as_deref(),
            Some("execute · 4/11")
        );
        assert_eq!(
            state_line(&state(Some("execute · 4/11"), Some(checkpoint()))).as_deref(),
            Some("execute · 4/11 · waits at its checkpoint")
        );
        assert_eq!(
            state_line(&state(None, Some(checkpoint()))).as_deref(),
            Some("waits at its checkpoint")
        );
        assert_eq!(
            state_lines(&state(Some("finish"), Some(merge_or_pr()))),
            ["finish · asks: Merge or PR?", "options: Merge · PR"]
        );
        assert_eq!(
            state_lines(&state(None, Some(merge_or_pr()))),
            ["asks: Merge or PR?", "options: Merge · PR"]
        );
        assert_eq!(
            state_lines(&state(Some("execute · 4/11"), Some(checkpoint()))),
            ["execute · 4/11 · waits at its checkpoint"]
        );
        assert!(state_lines(&state(None, None)).is_empty());
    }

    #[test]
    fn a_waiting_child_is_reported_as_child_waits_with_a_footer() {
        let child = SessionId::new();
        let api = RepositoryId::try_from("api").unwrap();
        let prompt = |origin, waits, step: Option<&str>| {
            child_result_prompt(
                &child,
                &api,
                "Fix API",
                origin,
                &ChildOutcome::Ended(StopReason::EndTurn),
                &ChildState {
                    step: step.map(str::to_owned),
                    waits,
                },
                "Task 4 is done.",
            )
        };

        assert_eq!(
            prompt(
                TurnOrigin::Orchestrator,
                Some(checkpoint()),
                Some("execute · 4/11")
            ),
            format!(
                "child waits · api · Fix API\nsession {child}\nexecute · 4/11 · waits at its checkpoint\n\nTask 4 is done.\n\nDecide with send_to_child(session_id, decision: \"approve\" or \"changes\", prompt). Ask the user with ask_user first when the user should make this decision."
            )
        );
        assert_eq!(
            prompt(TurnOrigin::User, Some(merge_or_pr()), None),
            format!(
                "child waits · api · Fix API\nsession {child}\nasks: Merge or PR?\noptions: Merge · PR\n(turn started by the user)\n\nTask 4 is done.\n\nAnswer with send_to_child(session_id, prompt); the prompt is the child's answer. Ask the user with ask_user first when the user should answer."
            )
        );
        assert_eq!(
            prompt(TurnOrigin::Orchestrator, None, Some("execute · 4/11")),
            format!(
                "child result · api · Fix API\nsession {child}\nexecute · 4/11\n\nTask 4 is done."
            )
        );
    }

    #[test]
    fn a_title_proposal_prompt_names_the_child_and_the_proposal() {
        let child = SessionId::new();
        let proposal = |by_user| TitleProposal {
            child: child.clone(),
            repository: RepositoryId::try_from("api").unwrap(),
            title: "Fix API".into(),
            proposed: "Auth endpoint".into(),
            by_user,
        };

        assert_eq!(
            title_proposal_prompt(&proposal(false)),
            format!(
                "title proposal · api · Fix API\nsession {child}\nproposed title: Auth endpoint\nAccept it with rename_session(title, session_id), or keep the current title."
            )
        );
        assert_eq!(
            title_proposal_prompt(&proposal(true)),
            format!(
                "title proposal · api · Fix API\nsession {child}\nproposed title: Auth endpoint\n(the current title was set by the user)\nAccept it with rename_session(title, session_id), or keep the current title."
            )
        );
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
        let text = preamble(
            &project(vec![repository("work", true)]),
            &SessionContext::default(),
        );

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

    /// The folder BMAD writes to, given its configuration `config`.
    fn bmad_folder(config: Option<&str>) -> PathBuf {
        let folders = workflow_folders(&context(&[], config));
        folders[folders.len() - 1].clone()
    }

    #[test]
    fn bmad_writes_to_its_default_output_folder_unless_its_config_names_another() {
        assert_eq!(bmad_folder(None), PathBuf::from("_bmad-output"));
        assert_eq!(
            bmad_folder(Some("[core]\noutput_folder = \"out/bmad\"\n")),
            PathBuf::from("out/bmad")
        );
    }

    #[test]
    fn the_workflow_folders_are_superpowers_then_bmad() {
        let context = context(&[], Some("[core]\noutput_folder = \"out\"\n"));

        assert_eq!(
            workflow_folders(&context),
            [
                PathBuf::from("docs/superpowers"),
                PathBuf::from(".superpowers"),
                PathBuf::from("out")
            ]
        );
    }

    #[test]
    fn the_preamble_names_bmads_configured_output_folder() {
        let context = context(&[], Some("[core]\noutput_folder = \"out\"\n"));

        let text = preamble(&project(vec![]), &context);

        assert!(text.contains("\n- .superpowers/\n- out/\n\n"), "{text}");
    }

    /// What the checks below are set up in: a working folder and a folder outside it.
    struct Place {
        work: PathBuf,
        outside: PathBuf,
    }

    /// Something folders, files and symbolic links can be made in.
    trait Layout {
        fn folder(&mut self, path: &Path);
        fn file(&mut self, path: &Path);
        #[cfg_attr(
            not(unix),
            expect(dead_code, reason = "only the Unix tests make links")
        )]
        fn link(&mut self, at: &Path, target: &Path);
    }

    /// One entry of a [`Tree`].
    enum Node {
        Folder,
        File,
        Link(PathBuf),
    }

    /// A file system in memory, by absolute path, whose facts are what the files port
    /// reports: a path resolves as the operating system resolves it, following every link,
    /// and is present when its last part exists without following a link there.
    #[derive(Default)]
    struct Tree {
        nodes: std::collections::BTreeMap<PathBuf, Node>,
    }

    impl Layout for Tree {
        fn folder(&mut self, path: &Path) {
            for folder in path.ancestors() {
                self.nodes
                    .entry(folder.to_path_buf())
                    .or_insert(Node::Folder);
            }
        }

        fn file(&mut self, path: &Path) {
            self.folder(path.parent().unwrap());
            self.nodes.insert(path.to_path_buf(), Node::File);
        }

        fn link(&mut self, at: &Path, target: &Path) {
            self.folder(at.parent().unwrap());
            self.nodes
                .insert(at.to_path_buf(), Node::Link(target.to_path_buf()));
        }
    }

    impl Tree {
        /// Where `path` leads, following every link, or its last part only when
        /// `follow_last`; `None` when a part is missing.
        fn walk(&self, path: &Path, follow_last: bool, depth: usize) -> Option<PathBuf> {
            // A drive's prefix alone (`C:`) names the folder the process is in on that drive,
            // which the disk always finds, though the path is not absolute.
            let mut parts = path.components();
            let bare_prefix = matches!(
                (parts.next(), parts.next()),
                (Some(Component::Prefix(_)), None)
            );
            if depth > 8 || !(path.is_absolute() || bare_prefix) {
                return None;
            }
            // A trailing separator or `.` makes the last part a folder to look into.
            let text = path.to_string_lossy();
            let follow_last = follow_last
                || text.ends_with(std::path::MAIN_SEPARATOR)
                || text.ends_with(&format!("{}.", std::path::MAIN_SEPARATOR));
            let components: Vec<Component<'_>> = path.components().collect();
            let mut real = PathBuf::new();
            for (index, component) in components.iter().enumerate() {
                let last = index + 1 == components.len();
                match component {
                    Component::Prefix(_) | Component::RootDir => real.push(component),
                    Component::CurDir => {}
                    Component::ParentDir => {
                        real.pop();
                    }
                    Component::Normal(name) => {
                        let next = real.join(name);
                        match self.nodes.get(&next)? {
                            Node::Link(target) if follow_last || !last => {
                                real = self.walk(target, true, depth + 1)?;
                            }
                            Node::File if !last => return None,
                            Node::Folder | Node::File | Node::Link(_) => real = next,
                        }
                    }
                }
            }
            Some(real)
        }

        /// What the files port reports about `path`.
        fn fact(&self, path: &Path) -> ResolvedPath {
            let unfollowed = self.walk(path, false, 0);
            ResolvedPath {
                given: path.to_path_buf(),
                canonical: self.walk(path, true, 0),
                is_symlink: unfollowed
                    .as_ref()
                    .is_some_and(|at| matches!(self.nodes.get(at), Some(Node::Link(_)))),
                present: unfollowed.is_some(),
            }
        }

        /// What the files port reports about the paths `check` asks about.
        fn facts(&self, check: &PathCheck) -> Vec<ResolvedPath> {
            check.paths().iter().map(|path| self.fact(path)).collect()
        }
    }

    /// A [`Layout`] on the real disk.
    struct Disk;

    impl Layout for Disk {
        fn folder(&mut self, path: &Path) {
            fs::create_dir_all(path).unwrap();
        }

        fn file(&mut self, path: &Path) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }

        #[cfg(unix)]
        fn link(&mut self, at: &Path, target: &Path) {
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(target, at).unwrap();
        }

        // Windows symlinks need developer mode: the tests that link are Unix-only.
        #[cfg(not(unix))]
        fn link(&mut self, _: &Path, _: &Path) {
            unreachable!("only Unix tests make links")
        }
    }

    /// The decision on the call `call` makes in a place `prepare` set up, from the facts of
    /// a [`Tree`] with no workflow configuration; the same place on the real disk, resolved
    /// through the files fake, must come to the same decision.
    fn decided(
        prepare: impl Fn(&mut dyn Layout, &Place),
        call: impl Fn(&Place) -> PathCheck,
    ) -> PathDecision {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let in_memory = Place {
            work: Path::new(root).join("w"),
            outside: Path::new(root).join("o"),
        };
        let mut tree = Tree::default();
        tree.folder(&in_memory.work);
        tree.folder(&in_memory.outside);
        prepare(&mut tree, &in_memory);
        let check = call(&in_memory);
        let decision = check.check(&tree.facts(&check), &SessionContext::default());

        let (work, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let on_disk = Place {
            work: work.path().to_path_buf(),
            outside: outside.path().to_path_buf(),
        };
        prepare(&mut Disk, &on_disk);
        let check = call(&on_disk);
        assert_eq!(check.decide(disk_answers(check.queries())), decision);
        decision
    }

    /// What the files fake answers to `queries` from the real disk.
    fn disk_answers(queries: Vec<FilesQuery>) -> Vec<Result<FilesAnswer, FilesError>> {
        use crate::files::{Files as _, FilesEvent, FilesRequestId};
        let files = crate::testing::MemoryFiles::on_disk();
        files.query(FilesRequestId(1), queries);
        let FilesEvent::Answered { answers, .. } = files.answer().pop().unwrap();
        answers
    }

    /// An edit of the `relative` paths of the working folder.
    fn edit(relative: &[&str]) -> impl Fn(&Place) -> PathCheck {
        let relative: Vec<String> = relative.iter().map(|path| (*path).to_owned()).collect();
        move |place| {
            let locations: Vec<PathBuf> =
                relative.iter().map(|path| place.work.join(path)).collect();
            PathCheck::new(ToolKind::Edit, &locations, &place.work, &[])
        }
    }

    /// Whether an edit of the `relative` paths in a working folder `prepare` set up is
    /// allowed.
    fn edit_allowed(relative: &[&str], prepare: impl Fn(&mut dyn Layout, &Place)) -> bool {
        decided(prepare, edit(relative)).writes_in_workflow_folders
    }

    fn nothing(_: &mut dyn Layout, _: &Place) {}

    #[test]
    fn an_orchestrator_edits_inside_the_workflow_folders() {
        for paths in [
            &["docs/superpowers/specs/2026-11-15-x-design.md"][..],
            &["docs/superpowers/plans/x.md", ".superpowers/notes/a.md"],
            &[".superpowers/brainstorm/a/b/c.html"],
            &["_bmad-output/planning-artifacts/prd.md"],
            &["docs/superpowers"],
            &["docs/./superpowers/plans/x.md"],
        ] {
            assert!(edit_allowed(paths, nothing), "{paths:?}");
        }
        assert!(edit_allowed(
            &["docs/superpowers/plans/x.md"],
            |layout, place| {
                layout.file(&place.work.join("docs/superpowers/plans/x.md"));
            }
        ));
    }

    #[test]
    fn a_step_back_counts_only_after_a_missing_folder() {
        let work = Path::new(if cfg!(windows) { r"C:\w" } else { "/w" });
        let mut tree = Tree::default();
        tree.folder(&work.join("docs/plans"));
        let at = |relative: &str| {
            let path = work.join(relative);
            let check = PathCheck::new(ToolKind::Edit, std::slice::from_ref(&path), work, &[]);
            let facts = tree.facts(&check);
            steps_back_below_missing(&path, Facts(&facts))
        };

        assert_eq!(at("docs/plans/../x.md"), Some(false));
        assert_eq!(at("docs/plans/new.md"), Some(false));
        assert_eq!(at("docs/plans/new/../x.md"), Some(true));
        // Once a folder is missing, a `..` any number of names later still counts.
        assert_eq!(at("docs/new/deeper/../x.md"), Some(true));
        // A part the facts do not cover is not guessed.
        assert_eq!(
            steps_back_below_missing(&work.join("docs/x.md"), Facts(&[])),
            None
        );
    }

    #[test]
    fn a_relative_path_is_never_resolved() {
        assert_eq!(resolved(Path::new("."), Facts(&[])), None);
        // Not even when the facts know where it leads.
        let fact = ResolvedPath {
            given: PathBuf::from("x"),
            canonical: Some(PathBuf::from("/w/x")),
            is_symlink: false,
            present: true,
        };
        assert_eq!(resolved(Path::new("x"), Facts(&[fact])), None);
    }

    #[test]
    fn a_parent_step_counts_where_it_leads() {
        let plans = |layout: &mut dyn Layout, place: &Place| {
            layout.folder(&place.work.join("docs/superpowers/plans"));
        };

        assert!(edit_allowed(&["docs/superpowers/plans/../x.md"], plans));
        assert!(!edit_allowed(&["docs/superpowers/plans/../../a.md"], plans));
        assert!(!edit_allowed(
            &["docs/superpowers/plans/../../../src/x.rs"],
            plans
        ));
        // A step back below the deepest existing folder cannot be resolved.
        assert!(!edit_allowed(
            &["docs/superpowers/plans/new/../x.md"],
            plans
        ));
    }

    #[test]
    fn an_edit_outside_the_workflow_folders_is_not_allowed() {
        for paths in [
            &["src/main.rs"][..],
            &["docs/superpowers/plans/x.md", "src/main.rs"],
            &["docs/superpowers-x/a.md"],
            &["docs/a.md"],
            &["docs/superpowers/../../src/main.rs"],
            &["_bmad/config.toml"],
            &[],
        ] {
            assert!(!edit_allowed(paths, nothing), "{paths:?}");
        }
        let relative = decided(nothing, |place| {
            PathCheck::new(
                ToolKind::Edit,
                &[PathBuf::from("docs/superpowers/plans/x.md")],
                &place.work,
                &[],
            )
        });
        assert!(!relative.writes_in_workflow_folders);
        let no_working_folder = decided(nothing, |place| {
            PathCheck::new(
                ToolKind::Edit,
                &[place.work.join("docs/superpowers/plans/x.md")],
                &place.work.join("missing"),
                &[],
            )
        });
        assert!(!no_working_folder.writes_in_workflow_folders);
    }

    #[test]
    fn only_an_edit_is_allowed_in_the_workflow_folders() {
        for kind in [
            ToolKind::Delete,
            ToolKind::Move,
            ToolKind::Execute,
            ToolKind::Read,
            ToolKind::Other,
        ] {
            let decision = decided(nothing, |place| {
                let locations = [place.work.join("docs/superpowers/plans/x.md")];
                PathCheck::new(kind, &locations, &place.work, &[])
            });
            assert!(!decision.writes_in_workflow_folders, "{kind:?}");
        }
    }

    #[test]
    fn only_a_read_or_an_edit_naming_paths_needs_facts() {
        let work = PathBuf::from("/w");
        let at = [work.join("a.md")];
        for kind in [ToolKind::Read, ToolKind::Edit] {
            assert!(
                PathCheck::new(kind, &at, &work, &[]).needs_facts(),
                "{kind:?}"
            );
            assert!(
                !PathCheck::new(kind, &[], &work, &[]).needs_facts(),
                "{kind:?}"
            );
        }
        for kind in [
            ToolKind::Delete,
            ToolKind::Move,
            ToolKind::Execute,
            ToolKind::Search,
            ToolKind::Think,
            ToolKind::Fetch,
            ToolKind::SwitchMode,
            ToolKind::Other,
        ] {
            assert!(
                !PathCheck::new(kind, &at, &work, &[]).needs_facts(),
                "{kind:?}"
            );
        }
    }

    // The paths are absolute only where a root needs no drive.
    #[cfg(unix)]
    #[test]
    fn the_check_asks_about_every_part_of_each_absolute_path_once() {
        let work = PathBuf::from("/w");
        let check = PathCheck::new(
            ToolKind::Edit,
            &[
                PathBuf::from("/w/docs/../x.md"),
                PathBuf::from("relative.md"),
                PathBuf::from("/w/x.md"),
            ],
            &work,
            &[Path::new("/t/api"), Path::new("/w")],
        );

        assert_eq!(
            check.paths(),
            [
                "/w",
                "/t/api",
                "/w/docs/../x.md",
                "/w/docs/..",
                "/w/docs",
                "/",
                "/w/x.md"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            check.queries()[1..],
            [FilesQuery::Read(PathBuf::from("/w/_bmad/config.toml"))]
        );
    }

    #[test]
    fn a_failed_resolution_lets_nothing_through() {
        let check = PathCheck::new(
            ToolKind::Edit,
            &[PathBuf::from("/w/docs/superpowers/x.md")],
            Path::new("/w"),
            &[],
        );
        let unavailable = || Err(FilesError::Unavailable("gone".into()));

        for answers in [
            vec![unavailable(), unavailable()],
            vec![Ok(FilesAnswer::Read(None)), Ok(FilesAnswer::Read(None))],
            Vec::new(),
        ] {
            assert_eq!(check.decide(answers), PathDecision::default());
        }
    }

    #[test]
    fn the_configuration_read_with_the_facts_names_bmads_folder() {
        let work = Path::new(if cfg!(windows) { r"C:\w" } else { "/w" });
        let mut tree = Tree::default();
        tree.folder(work);
        let check = PathCheck::new(ToolKind::Edit, &[work.join("out/prd.md")], work, &[]);
        let resolved = || Ok(FilesAnswer::Resolved(tree.facts(&check)));
        let config = "[core]\noutput_folder = \"out\"\n";

        let configured = check.decide(vec![resolved(), Ok(FilesAnswer::Read(Some(config.into())))]);
        let missing = check.decide(vec![resolved(), Ok(FilesAnswer::Read(None))]);
        let unreadable = check.decide(vec![resolved(), Err(FilesError::Unavailable("x".into()))]);

        assert!(configured.writes_in_workflow_folders);
        assert!(!missing.writes_in_workflow_folders);
        assert!(!unreadable.writes_in_workflow_folders);
    }

    /// A read of the paths `locations` give in a place whose worktrees `worktrees` give.
    fn read_of(
        locations: impl Fn(&Place) -> Vec<PathBuf>,
        worktrees: impl Fn(&Place) -> Vec<PathBuf>,
    ) -> impl Fn(&Place) -> PathCheck {
        move |place| {
            let worktrees = worktrees(place);
            let worktrees: Vec<&Path> = worktrees.iter().map(PathBuf::as_path).collect();
            PathCheck::new(ToolKind::Read, &locations(place), &place.work, &worktrees)
        }
    }

    #[test]
    fn a_read_counts_inside_only_when_every_path_resolves_into_a_worktree() {
        let prepare = |layout: &mut dyn Layout, place: &Place| {
            layout.file(&place.work.join("api/src/main.rs"));
            layout.file(&place.work.join("api-x/a.rs"));
            layout.file(&place.outside.join("secret"));
        };
        let api = |place: &Place| vec![place.work.join("api")];
        let inside = |relative: &'static [&'static str]| {
            decided(
                prepare,
                read_of(
                    move |place| relative.iter().map(|path| place.work.join(path)).collect(),
                    api,
                ),
            )
            .reads_in_worktrees
        };

        assert!(inside(&[
            "api/src/main.rs",
            "api/src",
            "api/src/../src/main.rs"
        ]));
        for relative in [
            &["api/.."][..],
            &["api-x/a.rs"],
            &["api/src/missing.rs"],
            &[],
        ] {
            assert!(!inside(relative), "{relative:?}");
        }
        let outside = decided(
            prepare,
            read_of(|place| vec![place.outside.join("secret")], api),
        );
        assert!(!outside.reads_in_worktrees);
        let relative = decided(
            prepare,
            read_of(|_| vec![PathBuf::from("api/src/main.rs")], api),
        );
        assert!(!relative.reads_in_worktrees);
        let no_worktree = decided(
            prepare,
            read_of(
                |place| vec![place.work.join("api/src/main.rs")],
                |_| Vec::new(),
            ),
        );
        assert!(!no_worktree.reads_in_worktrees);
    }

    // Windows symlinks need developer mode, which a runner or user may not have.
    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_caught_from_resolved_facts() {
        let link_folder = |layout: &mut dyn Layout, place: &Place| {
            layout.file(&place.outside.join("secret"));
            layout.link(&place.work.join("docs/superpowers"), &place.outside);
        };
        assert!(!edit_allowed(&["docs/superpowers/secret"], link_folder));
        assert!(!edit_allowed(&["docs/superpowers/new.md"], link_folder));

        let dangling = |layout: &mut dyn Layout, place: &Place| {
            layout.folder(&place.work.join(".superpowers"));
            layout.link(
                &place.work.join(".superpowers/a.md"),
                &place.outside.join("missing"),
            );
        };
        assert!(!edit_allowed(&[".superpowers/a.md"], dangling));

        let linked_worktree = |layout: &mut dyn Layout, place: &Place| {
            layout.file(&place.outside.join("secret"));
            layout.folder(&place.work.join("api"));
            layout.link(&place.work.join("api/link"), &place.outside);
        };
        for relative in ["api/link/secret", "api/link/../secret", "api/link"] {
            let decision = decided(
                linked_worktree,
                read_of(
                    move |place| vec![place.work.join(relative)],
                    |place| vec![place.work.join("api")],
                ),
            );
            assert!(!decision.reads_in_worktrees, "{relative}");
        }
    }

    // Windows symlinks need developer mode, which a runner or user may not have.
    #[cfg(unix)]
    #[test]
    fn a_working_folder_reached_through_a_link_still_counts() {
        let linked = |layout: &mut dyn Layout, place: &Place| {
            layout.link(&place.outside.join("project"), &place.work);
        };
        let through_link = decided(linked, |place| {
            let cwd = place.outside.join("project");
            PathCheck::new(
                ToolKind::Edit,
                &[cwd.join("docs/superpowers/plans/x.md")],
                &cwd,
                &[],
            )
        });
        let real_path = decided(linked, |place| {
            PathCheck::new(
                ToolKind::Edit,
                &[place.work.join("docs/superpowers/plans/x.md")],
                &place.outside.join("project"),
                &[],
            )
        });

        assert!(through_link.writes_in_workflow_folders);
        assert!(real_path.writes_in_workflow_folders);
    }
}
