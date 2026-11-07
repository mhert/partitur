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
- A proposal appears in the orchestrator's transcript as an approval card. Each child's
  title, prompt and auto-allow toggle can be edited and children can be removed or added for
  any of the project's repositories; then the card is approved, or rejected with an optional
  note for the orchestrator. An orchestrator whose turn ends with its card undecided waits
  for you (`asks you` in the tree) and takes no prompt until you decide.
- Approving starts the children under the orchestrator in the tree and tells the
  orchestrator their session ids; rejecting tells it the note.
- When a child's turn ends, its final message reaches the orchestrator as a prompt marked
  `child result`. Results that arrive while the orchestrator is busy wait, and are delivered
  one per turn in the order they arrived.
- An orchestrator may inspect but never change: its edit, delete, move and execute permission
  requests are denied, with a notice in its transcript. Read and search requests are asked
  like in any other session, or granted when its auto-allow is on.
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
failure.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check
ICED_TEST_BACKEND=tiny-skia cargo test --workspace --all-features
```

## License

MIT, as declared in the crate manifests.
