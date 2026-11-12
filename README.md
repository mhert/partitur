# Partitur

Partitur is a desktop application for driving AI coding agents on an application, whether
that application is one repository or several. Its main window is the tree of agent
sessions rather than an editor: you prompt those sessions, watch what they do, and see what
shaped them (instruction files, model, effort, backend). It talks to Claude Code over the
Agent Client Protocol (ACP v2) through the `claude-agent-acp` adapter.

## Build and run

```sh
cargo run -p partitur
```

- `PARTITUR_OPEN=<folder>` opens that folder as a project at startup, as if it had been
  picked in the folder dialog.
- `RUST_LOG` filters the log written to stderr; the default is `partitur=info`.

## Keyboard

- `Tab` switches to the next tab; `Ctrl 1`, `Ctrl 2` and `Ctrl 3` select Projects, Session
  and Env.
- `Ctrl K` opens the jump panel: type to filter the sessions of all open projects, move with
  the arrow keys, `Enter` opens the highlighted session, `Escape` closes the panel. Archived
  sessions are listed only while its `include archived` box is checked.
- On the Projects tab the arrow keys move the highlighted row, `Enter` opens its session,
  `N` opens the new-session sheet for its project, `A` archives its session and `D` deletes it.
- In the archive sheet `Enter` archives and `Escape` cancels, and in the delete sheet `Enter`
  deletes and `Escape` cancels; no other key reaches the window while either is open.
- In the new-session sheet `Enter` creates the session and `Escape` cancels; no other key
  reaches the window while it is open.
- In the prompt `Enter` sends and `Shift Enter` breaks the line.
- On the Session tab, with the cursor in no field, `Enter` approves an orchestrator's
  proposed split.

## Where things live

- Configuration: `$XDG_CONFIG_HOME/partitur/config.toml` (usually
  `~/.config/partitur/config.toml`).
- Projects, sessions and transcripts: `$XDG_DATA_HOME/partitur` (usually
  `~/.local/share/partitur`).
- A project's settings: `projects/<project-id>/settings.toml` in that data folder. Partitur
  writes it, all in comments, the first time the project's settings are read; see
  [Settings](#settings).

## Configuration

The file names the agent backends and the one new sessions run on. Each session can be
moved to another backend from the picker next to Model and Effort.

```toml
default = "claude-main"

[backends.claude-main]
command = "claude-agent-acp"
env = { CLAUDE_CONFIG_DIR = "~/.claude-main" }

[backends.claude-work]
command = "claude-agent-acp"
args = []
env = { CLAUDE_CONFIG_DIR = "~/.claude-work" }
thinking = "summarized"
```

- Each key under `backends` is a backend's name: non-empty, without `/` or `\`.
- `command` is required. `args` (default none), `env` (default none) and `thinking`
  (`summarized` or `omitted`, default `summarized`) are optional.
- An `env` value that is `~` or starts with `~/` has that `~` replaced by the home
  directory; nothing else is expanded. The adapter inherits Partitur's environment plus these
  variables.
- `default` is required when more than one backend is configured; with one it may be left
  out. It is the backend of a project's first session; later ones start from the project's
  newest session.
- Without a configuration file there is one backend, `claude`, running `claude-agent-acp`.
- A backend whose command is `claude-agent-acp` runs the adapter through `npx` when it is
  not installed. Any other missing command stops the start with an error naming its
  backend.
- A top-level `worktree_path` sets where new worktrees go, and a top-level `link_opener` what
  a clicked link opens with; see [Settings](#settings).
- The old single `[backend]` block is no longer accepted; unknown keys are errors.

### Settings

Some settings can be given at several levels, and the most specific level that sets one wins:
`config.toml` (global), then the project, then one of its repositories, then a folder inside a
repository. A setting is only allowed at the levels listed for it; anywhere else it is an
error.

| Setting         | Allowed at                  | Default                             |
| --------------- | --------------------------- | ----------------------------------- |
| `worktree_path` | global, project, repository | `{repo_path}/../{repo}-wt/{branch}` |
| `link_opener`   | global, project, repository | `xdg-open`                          |

`worktree_path` is the pattern of the folder a Worktree session or child gets:

- `{repo_path}` is the repository's folder, `{repo}` its name, `{project}` the project's name
  and `{branch}` the new branch's name. Any other `{name}`, and a `{` that is never closed,
  is an error.
- A leading `~` or `~/` stands for the home folder (without one, the pattern is an error);
  any other `~` stays as it is. A pattern that is still relative then is taken relative to
  the repository's folder, so `../trees/{branch}` puts the worktrees in `trees` beside it.
  `.` and `..` in the result are resolved without touching the disk, so the default puts the
  worktrees next to the repository, in `<repo>-wt`. A branch name with `/` makes nested
  folders.

`link_opener` is the command a link clicked in a transcript is opened with:

- The value is split on whitespace into a program and its arguments, and the link is added as
  the last argument, so `"firefox -P work"` runs `firefox -P work <link>`. No shell is
  involved and there is no quoting. A value without a program is an error.
- A repository session (also a worktree session or a child) takes it from its repository,
  then the project, then `config.toml`; a project session from the project, then
  `config.toml`.
- The project's settings are read again on every click. The program is started in the
  background; when it cannot be started, or the settings cannot be read, the error bar says
  why and nothing opens.

Both settings are given the same way:

- The global level is a top-level key of `config.toml`; an invalid value stops the start
  with an error naming the file:

  ```toml
  worktree_path = "~/worktrees/{repo}/{branch}"
  link_opener = "firefox -P work"
  ```

- The project and repository levels live in the project's `settings.toml`, in these sections:
  `[project]`, `[repository.<name>]` and `[repository.<name>.folder."<path>"]`. Every value
  is a string. Neither setting can be set for a folder, so no setting is allowed there yet.

  ```toml
  [project]
  worktree_path = "~/worktrees/{project}/{repo}/{branch}"
  link_opener = "firefox -P work"

  [repository.api]
  worktree_path = "{repo_path}/../api-trees/{branch}"
  link_opener = "chromium --new-window"
  ```

- The file is read again whenever a new-session sheet opens, whenever a proposed split is
  checked or approved and whenever a link is clicked, so an edit applies without
  restarting. A mistake
  in it (naming the section it is in) stops the sheet or card from creating anything and is
  shown there until the file is fixed.

## Starting a session

`+ new session` in the tree, or `N` on the Projects tab, opens the new-session sheet for a
project or for one of its repositories. The sheet picks the session's settings before it
exists.

- Model and Effort come from the agent itself: Partitur starts it in the background as soon
  as the sheet opens, and both pickers say `loading…` until it answers.
- The settings start from the project's newest session, and from the default backend when
  the project has none. The remembered model is switched on in the agent as soon as it
  answers, so the Effort list is that model's. Picking a model refreshes the Effort list the
  same way, and picking another backend restarts the agent there.
- Effort keeps the remembered or picked value wherever the model's Effort list offers it,
  and takes the agent's own effort otherwise.
- `Auto-allow tool calls` grants every permission request of the session without asking.
- `Create session` (or `Enter`) adds the session to the tree, selects it and focuses its
  prompt. `Cancel`, `Escape` and a click on the backdrop close the sheet and leave nothing
  behind.
- The `Branch & worktree` box decides how the session uses git. Its `mode` is one of:
  - `Checked-out` works in the repository as it is now. The box says which branch that is
    (`branch  <name>  works in the repository as it is now`, `(detached)` without one) and
    runs no git command.
  - `New branch` creates a branch and switches the repository itself to it; uncommitted
    changes come along, as with `git switch -c`. The box notes this.
  - `Worktree` creates the branch in a new worktree and runs the agent there; the box shows
    the folder under `worktree at`, with the home folder written as `~`.
- `branch name` and `base branch` apply to the two modes that create a branch. The name is
  trimmed and must be a valid git branch name that does not exist in the repository yet. The
  base is picked from the repository's local branches. A Worktree must not exist as a folder
  already. What is wrong shows under the box and keeps `Create session` from working:
  `enter a branch name`, `"<name>" is not a valid git branch name`,
  `branch <name> already exists in <repo>`, `<path> already exists`.
- `Fetch <remote> and start from <remote>/<base>` fetches the base's upstream remote (else
  `origin`) first, if that remote has the base, and starts the branch from `<remote>/<base>`.
  The local base branch is never moved. Without such a remote branch the box says
  `<base> has no remote counterpart` and the fetch cannot be checked. A fetch that takes more
  than a minute is stopped and fails the create.
- The box starts from the project's newest session: its mode and fetch choice. Without one it
  starts `Checked-out` with fetching on. The name always starts empty, and the base starts at
  the branch checked out now.
- A repository's checked-out folder is shared. While a live (not archived) session works in
  it, `New branch` there is refused with `<repo> is in use by "<session title>" — use a
  worktree`, since switching the branch would pull it from under that session.
- Git runs in the background. The branches are read when the sheet opens (the rows say
  `loading…`, and `reading the branches of <repo>…` stops `Create session` meanwhile;
  `Checked-out` can be created at once). `Create session` then makes the branch or worktree
  in the background: the sheet freezes, the button says `creating…` and `Cancel` stays
  available. The session is created only once git succeeded; when a step fails, nothing is
  created and the sheet says why. `Cancel` meanwhile closes the sheet and undoes what git
  made (the worktree and branch, or the switch back to the previous branch).
- Only one changing git request runs per repository at a time. While another one (including
  the undo of a cancelled create) is still running there, `Worktree` and `New branch` for that
  repository are refused with `<repo> is busy with another branch change`.
- A project folder that is not a repository offers only `Checked-out`, and says so. So does a
  repository whose git state cannot be read.
- On an orchestrator's sheet the box sets the defaults for its children: `Checked-out` means
  `children work in their repositories as they are now`; the other modes show the common
  base branch (the branches every repository has; `per child` when there are none), whether
  to `Fetch first and start from each repository's remote base`, and a branch name that may
  stay empty because each child can be named on the split card. There is no `worktree at`
  line.
- Git work still running when the window closes finishes, but no session is recorded for it.
- A session is titled `untitled` until the agent titles it: Claude Code sends a short title
  after the first prompt, and Partitur takes it. A title you typed, or one an orchestrator
  gave a child, is never replaced; a child whose title the split card derived from its prompt
  and you left alone takes the agent's title. The first prompt is kept as it was typed, with
  the skill it starts with, and does not change the title.

## Orchestrating a project

A project session in a project with at least two repositories besides the project folder
itself is an orchestrator: it plans the work across the repositories and hands it to child
sessions, one per repository, instead of changing anything itself.

- Partitur gives an orchestrator five tools through its own MCP server `partitur`:
  `propose_children` proposes child sessions (a repository, a prompt and a title each),
  `send_to_child` sends a follow-up prompt to one of its children (queued while the child is
  mid-turn), `list_children` lists its children with their title, repository, session id and
  status, `cancel_child` stops a child's current turn (its result then reaches the
  orchestrator as a child result marked cancelled) and `get_child_result` returns a child's
  last final message and its status. Calls to these tools are allowed without asking.
- A proposal appears in the orchestrator's transcript as the `Proposed split` card: a table
  with one row per child (repository, title and prompt) and a muted `skipped` row for every
  repository of the project that gets no child. A click on a child's row turns it into a
  form for its title, prompt and auto-allow toggle, with a button that removes the child; a
  click on a skipped row adds a child for that repository. `Edit split` opens every form at
  once and offers a picker that adds a further child for any repository. `Approve split`
  (or `Enter`, while the cursor is in no field) starts the children as edited; `Reject`
  sends the orchestrator the optional rejection note written next to it (approving carries
  no note). An orchestrator whose turn ends with its split undecided waits for you (`asks
  you` in the tree) until you decide; it still takes your prompts and child results
  meanwhile, and each turn they start ends waiting for you again.
- Every child has branch settings of its own, with the same modes, rules and messages as the
  new-session sheet, in its form on the card; the row shows its mode and branch name. They
  start from the defaults of the orchestrator's sheet, and a dim `from orchestrator` marks
  each value that still equals them. The orchestrator's branch name is only a default: a
  child in `Worktree` or `New branch` mode needs a name of its own, and no two children may
  use one name or one worktree folder in a repository. A `New branch` child needs its
  repository's checked-out folder to itself, as in the new-session sheet, and also against
  the other children (`<repo> is in use by another child — use a worktree`).
- The card checks each child in its own repository, in the background. A row says
  `loading…` until git has described its repository, and then names what keeps the child from
  starting. `Approve split` (and `Enter`) does nothing while a child is loading or has such a
  problem.
- Approving is all or nothing. The git work of every child runs first, in the background:
  `Approve split` says `approving…` and the card is locked, while the rest of the window keeps
  working. Only once all of it succeeded are the children started. When a step fails, what
  the approval made is undone, the proposal stays undecided and the card is editable again
  with the reason below it.
- Approving starts the children under the orchestrator in the tree and tells the
  orchestrator their session ids, and for each child its mode, the branch it made and the
  folder it works in (`<title> · <repository> · session <id> · worktree <branch> · in
  <folder>`); rejecting tells it the note. Children run on the
  orchestrator's backend with its model and effort, and their auto-allow toggle starts at the
  orchestrator's.
- When a child's turn ends, its final message reaches the orchestrator as a prompt marked
  `child result · <repository> · <title>`, followed by the child's session id. Results that
  arrive while the orchestrator is busy wait, and are delivered one per turn in the order they
  arrived.
- From the decision on, the orchestrator's transcript is a timeline: your decision, each
  child's result (`<repository> reported back:` over the child's message), what the
  orchestrator answered, each prompt it sent a child with `send_to_child` (with a link that
  opens the child), your own prompts and the prompts Partitur wrote, with the orchestrator's
  thoughts and tool calls in between. Its last line names the children it waits for and the
  ones that ask you. Above the transcript a strip shows one card per child (repository,
  title, status, context usage) that opens the child.
- `Stop children`, under an orchestrator's prompt while a child is in a turn, stops the turn
  of every child, as `Stop` does for one session: a child that waits for a permission answer
  has the request cancelled and its turn stopped. Each stopped child reports its turn to the
  orchestrator as cancelled.
- An orchestrator may inspect but never change: its edit, delete, move and execute permission
  requests are denied, with a notice in its transcript. Read and search requests are asked
  like in any other session, or granted when its auto-allow is on, except that a read (not a
  search) whose files all lie inside the worktrees of its children (symbolic links resolved)
  is granted at once, so it can look at what its children made without asking. The denial
  applies only to tool calls that ask Partitur for permission, so calls your own Claude Code
  allow-rules already permit (the settings under the Claude config folder in use) are not
  denied. Neither are third-party MCP tools, which report the kind `other`.
- A session waiting for you marks every session above it in the tree with `child asks you`.

A headless end-to-end check drives one orchestration against the real adapter, in a
throwaway project with the repositories `api` and `web` and a throwaway data folder. It needs
a logged-in Claude Code; the adapter inherits the environment, so `CLAUDE_CONFIG_DIR` picks the
login. `PARTITUR_E2E_COMMAND` overrides the adapter command (default `claude-agent-acp`, run
through `npx` when it is not installed).

```sh
CLAUDE_CONFIG_DIR=~/.claude-main cargo run -p partitur --example orchestrate
```

It prints every state transition on one line, approves the proposal as proposed, and exits
with status 0 once the orchestrator has both children's results, or 1 after 300 seconds or a
failure. It switches auto-allow on for the orchestrator and for every child, so a headless run
never waits on a permission (the orchestrator's changing calls are still denied). The
temporary folders are removed afterwards, but the adapter keeps the sessions' history for the
temporary project under the Claude config folder it ran with (`$CLAUDE_CONFIG_DIR/projects`).

## Interrupted sessions

When Partitur closes while a session runs or waits for you to answer a permission request,
that turn never ends. On the next start the session shows `interrupted`, and its transcript
says `Partitur closed before this turn ended`. The mark stays until the session's next turn:
a prompt resumes the session as it would an idle one. A session whose turn finished or failed
is not interrupted, and neither is an orchestrator whose turn ended on a proposal you had not
decided yet. A stop counts only once the agent confirmed it: quitting right after pressing
stop can still leave the session interrupted.

## Archiving sessions

A finished session can leave the tree without losing anything. The highlighted row, and the
row under the pointer, end in an `Archive` button; `A` does the same for the highlighted row.
A sheet asks for confirmation first, and only on confirmation does anything change.

- `Done`, `Failed`, `Interrupted` and idle sessions archive directly. A session that runs or
  waits for you offers `Stop and archive`: confirming stops its turn first, the way the stop
  button does.
- An orchestrator archives together with its children. A child cannot be archived on its own;
  its button cannot be pressed and says to archive the orchestrator.
- Nothing is deleted: the transcript and the Env data stay. Archived sessions move under an
  `archived · N` row at the end of their project, collapsed at first, which lists each with
  the date it was archived. The row under the pointer ends in a `Restore` and a `Delete`
  button.
- An archived session opens read-only: the Session and Env tabs show a banner with a
  `Restore` button, and the Session tab has no composer.
- Restoring puts the session, and its children, back in the tree with the status they had.
  A child's result that came in while its orchestrator was archived is sent to it then, as
  long as Partitur kept running in between; like the results held for a closed project,
  it is not kept across a restart.
- The status line counts live sessions only and says how many are archived; the jump panel
  leaves archived sessions out unless `include archived` is checked.

Partitur does not archive anything on its own.

## Deleting sessions

A session you no longer need can be deleted for good. Next to `Archive`, the highlighted row
and the row under the pointer end in a `Delete` button; `D` does the same for the highlighted
row. An archived row shows its `Delete` button beside `Restore` while the pointer is over
it, and `D` works on the highlighted archived row too. A
sheet asks for confirmation first, and only on confirmation does anything change.

- A session that runs or waits for you offers `Stop and delete`: confirming ends its turn as
  its connection closes.
- An orchestrator is deleted together with its children. A child cannot be deleted on its
  own; its button cannot be pressed and says to delete the orchestrator.
- For every session of the family that made a branch (in `Worktree` or `New branch` mode),
  the sheet shows a block headed `<repository> · <title>` with two boxes, both unchecked:
  `Remove worktree <path>` while git still lists the worktree, and `Delete branch <name>`
  while the branch exists. The blocks are read in the background and say `checking…` until
  git answers; a block whose repository is busy with another branch change says so. Neither
  shows a box, and deleting while a block is still checking removes nothing for it. A box
  checked on a block whose repository turned busy meanwhile removes nothing and is reported
  as kept. A block with nothing left to remove is hidden.
- The sheet warns before you check anything: the worktree box says how many uncommitted
  files the worktree has, and the branch box says `not merged` when the branch is not merged
  into the base it started from. A branch that is checked out in another worktree cannot be
  checked, nor one still held by a worktree whose folder is gone (`its worktree folder is
  gone; run git worktree prune`), and the branch of a `Worktree` session can only go
  together with its worktree.
- Checked boxes are forced: the worktree is removed with `git worktree remove --force`, so
  its uncommitted files are lost, and the branch with `git branch -D`, so unmerged commits
  are lost. The main checkout is never removed.
- The removal runs in the background after the sheet closed; the project row says
  `cleaning up…` meanwhile. Each repository is read again first, and a worktree or branch that
  no longer is what the sheet showed is kept and reported: a worktree git no longer lists, with
  something else checked out or with another number of uncommitted files, a branch that is
  gone, checked out elsewhere, or merged when the sheet showed it so and no longer. Two limits
  remain: a branch the sheet showed as not merged is deleted even if more commits arrived on
  it since, and the agent of a running session may still write into its worktree between
  the fresh reading and the removal.
- A removal that fails or keeps something is reported in the error bar. The session is
  deleted regardless.
- Deleting removes Partitur's records of the sessions and their transcripts, so the Session
  and Env tabs have nothing left to show. The agent's own session data, such as Claude Code's
  files under its config folder, is left alone.
- It cannot be undone. To get a session out of sight and keep it, archive it instead.

Partitur does not delete anything on its own, and it removes a worktree or branch only for a
box you checked.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check
ICED_TEST_BACKEND=tiny-skia cargo test --workspace --all-features
```

## License

MIT, as declared in the crate manifests.
