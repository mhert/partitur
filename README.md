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

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check
ICED_TEST_BACKEND=tiny-skia cargo test --workspace --all-features
```

## License

MIT, as declared in the crate manifests.
