# Partitur

Partitur is a desktop application for driving AI coding agents on an application, whether
that application is one repository or several. Its main window is the tree of agent
sessions rather than an editor: you prompt those sessions, watch what they do, and see what
shaped them (instruction files, model, effort, backend). It talks to Claude Code over the
Agent Client Protocol through the `claude-agent-acp` adapter.

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
  the arrow keys, `Enter` opens the highlighted session, `Escape` closes the panel.
- On the Projects tab the arrow keys move the highlighted row, `Enter` opens its session and
  `N` starts a new session in its project.
- In the prompt `Enter` sends and `Shift Enter` breaks the line.
- On the Session tab, with the cursor in no field, `Enter` approves an orchestrator's
  proposed split.

## Where things live

- Configuration: `$XDG_CONFIG_HOME/partitur/config.toml` (usually
  `~/.config/partitur/config.toml`).
- Projects, sessions and transcripts: `$XDG_DATA_HOME/partitur` (usually
  `~/.local/share/partitur`).

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
  out.
- Without a configuration file there is one backend, `claude`, running `claude-agent-acp`.
- A backend whose command is `claude-agent-acp` runs the adapter through `npx` when it is
  not installed. Any other missing command stops the start with an error naming its
  backend.
- The old single `[backend]` block is no longer accepted; unknown keys are errors.

## Orchestrating a project

A project session in a project with at least two repositories besides the project folder
itself is an orchestrator: it plans the work across the repositories and hands it to child
sessions, one per repository, instead of changing anything itself.

- Partitur gives an orchestrator two tools through its own MCP server `partitur`:
  `propose_children` proposes child sessions (a repository, a prompt and a title each), and
  `send_to_child` sends a follow-up prompt to one of its children (queued while the child is
  mid-turn). Calls to these tools are allowed without asking.
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
- Approving starts the children under the orchestrator in the tree and tells the
  orchestrator their session ids; rejecting tells it the note.
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
  like in any other session, or granted when its auto-allow is on. The denial applies only to
  tool calls that ask Partitur for permission, so calls your own Claude Code allow-rules
  already permit (the settings under the Claude config folder in use) are not denied. Neither
  are third-party MCP tools, which report the kind `other`.
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

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check
ICED_TEST_BACKEND=tiny-skia cargo test --workspace --all-features
```

## License

MIT, as declared in the crate manifests.
