# Configuration

## Where things live

- Configuration: `$XDG_CONFIG_HOME/partitur/config.toml` (usually
  `~/.config/partitur/config.toml`); on macOS
  `~/Library/Application Support/mhert.partitur/config.toml`; on Windows
  `%APPDATA%\mhert\partitur\config\config.toml`.
- Projects, sessions and transcripts: `$XDG_DATA_HOME/partitur` (usually
  `~/.local/share/partitur`); on macOS `~/Library/Application Support/mhert.partitur/`,
  the same folder as the configuration; on Windows `%APPDATA%\mhert\partitur\data\`.
  Uninstalling Partitur on Windows leaves both folders in place.
- The data folder's format: `format.json` in that folder records the version of the stored data.
  Partitur does not start on a folder in a newer format than this Partitur knows: it reports
  the format it found and leaves the data alone, so update Partitur instead.
- A session's files: `projects/<project-id>/<session-id>/` in that data folder, holding
  `<session-id>_meta.json` (the session's record) and `<session-id>_history.jsonl` (its
  transcript, including the workflow reports of its [workflow](workflows.md)).
- Partitur writes to the data folder in the background, one write after another in the order
  they happened, so the window never waits for the disk; closing the window finishes every
  write still pending. When a write fails, what it would have saved still shows in the window
  and the error bar says what could not be written; after a restart only what was saved is
  there.
- A project's settings: `projects/<project-id>/settings.toml` in that data folder. Partitur
  writes it, all in comments, when it first reads the project's settings; see
  [Settings](#settings).

## Command line

- `partitur` starts the application.
- `partitur --version` prints `partitur <version>` and exits. On Windows Partitur has no
  console, so it prints only when the output is redirected: `partitur --version | more`.
- `partitur --smoke` opens the window and exits with status 0 after its first frame. It
  works on throwaway configuration and data folders in a temporary directory, removed at
  the end, so it never reads or writes your own, and it ignores `PARTITUR_OPEN`. The release
  jobs run it to check that an installed package starts.
- Any other argument prints a usage line and exits with status 2.
- On Windows an error that stops the start, such as a `config.toml` that does not parse,
  and a usage error are also shown in a message dialog, as nothing sees stderr there. Under
  `--smoke` they are only written to stderr.

## The configuration file

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
- A bare `command` is looked up on `PATH`. On Windows `PATH` is read in any case (`Path`), the
  name is tried with each extension in `PATHEXT` (default `.COM;.EXE;.BAT;.CMD`) and never as
  written, so `npx` is `npx.cmd`, and the adapter runs by the found file's full path.
- On Windows each adapter runs under `partitur --job-run`, which ends the adapter's whole
  process tree, node and everything it started, when its session stops. The backend list
  still shows the configured command.
- An `env` value that is `~` or starts with `~/` or `~\` has that `~` replaced by the home
  directory; nothing else is expanded. The adapter inherits Partitur's environment plus
  these variables, which win over the login shell's on macOS (see below).
- `default` is required when more than one backend is configured; with one it may be left
  out. It is the backend of a project's first session; later ones start from the project's
  newest session.
- Without a configuration file there is one backend, `claude`, running `claude-agent-acp`.
- A backend whose command is `claude-agent-acp` runs, in this order: the adapter bundled with
  the release (beside the executable), when Claude Code is installed (on `PATH`, and on macOS
  in the login shell's), with the bundled Node.js or else the `node` on `PATH`; then the
  installed `claude-agent-acp` on `PATH`; then the adapter through `npx`. Any other missing command stops the start with an error naming its
  backend, except on Windows and on a start from the Finder or the Dock (see below). On
  Windows the backends are resolved once the window opens, so opening a session on that
  backend fails with the message that the command is not on `PATH`, and every other backend
  works.
- A top-level `worktree_path` sets where new worktrees go, and a top-level `link_opener` what
  a clicked link opens with; see [Settings](#settings).
- A top-level `window_frame` is `"partitur"` (the default) or `"system"`; any other value is
  an error. With `"partitur"` the toolbar is the window's title bar: drag its empty area to
  move the window, double-click it to maximize or restore it and right-click it for the
  window menu. On Linux and Windows it has its own minimize, maximize and close buttons and
  the window resizes from its edges; on macOS it keeps the native traffic lights at its left.
  The bar keeps working while a sheet or the jump panel is open. `"system"` keeps the
  desktop's own title bar above the toolbar, for example on a tiling window manager:

  ```toml
  window_frame = "system"
  ```
- The old single `[backend]` block is no longer accepted; unknown keys are errors.

## GitHub Copilot

GitHub Copilot CLI is a backend like any other: its entry runs `copilot --acp`. Claude and
Copilot backends mix in one file, and there may be any number of each. Every Copilot profile
keeps its configuration, login, sessions, skills and agents in its own `COPILOT_HOME`:

```toml
default = "claude-main"

[backends.claude-main]
command = "claude-agent-acp"
env = { CLAUDE_CONFIG_DIR = "~/.claude-main" }

[backends.copilot-main]
command = "copilot"
args = ["--acp"]
env = { COPILOT_HOME = "~/.copilot-main" }

[backends.copilot-work]
command = "copilot"
args = ["--acp"]
env = { COPILOT_HOME = "~/.copilot-work", COPILOT_GITHUB_TOKEN = "<token>" }
```

- Copilot is not bundled. Install it yourself: `npm install -g @github/copilot`,
  `brew install --cask copilot-cli` or `winget install GitHub.Copilot`. `copilot` is a plain
  command: it is looked up on `PATH` like any other (with `PATHEXT` on Windows, and on macOS
  on the login shell's `PATH`), and a missing one fails as any missing command does.
- `COPILOT_HOME` defaults to `~/.copilot`, so a backend without it in `env` shares that
  folder with the `copilot` you run in a terminal.
- Log each profile in outside Partitur, with its folder: `COPILOT_HOME=~/.copilot-work
  copilot login`. A session on a profile that is not logged in fails with Copilot's error.
- A profile may instead carry its own token in `COPILOT_GITHUB_TOKEN`, or use a provider with
  your own key through the `COPILOT_PROVIDER_*` variables; both go into its `env`.
- Copilot takes a token from `COPILOT_GITHUB_TOKEN` first, then `GH_TOKEN`, then
  `GITHUB_TOKEN`, and reads them from the inherited environment too. So a `GH_TOKEN` or
  `GITHUB_TOKEN` set in your shell can override a profile's login; a `COPILOT_GITHUB_TOKEN`
  in the profile's `env` wins over both.
- Copilot keeps the token of a login in the system's credential store (the Secret Service on
  Linux), and only the account records under `COPILOT_HOME`. Whether two `COPILOT_HOME`s keep
  two logins fully apart is not verified; a token per profile in `env` keeps them apart in any
  case.
- `thinking` applies to Claude Code only; on Copilot it has no effect.
- Partitur's instructions for its tools reach Claude Code through the system prompt. Copilot
  reads them from a folder instead: Partitur writes them to
  `agent-instructions/partitur.instructions.md` in its data folder and starts every adapter
  with `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` naming that folder. A value the backend's `env` or
  the inherited environment already sets is kept, with the folder appended after a comma.
  Copilot adds the file to the repository's own `AGENTS.md` or `CLAUDE.md`. When the file
  cannot be written, every session opened on a backend given that folder, Claude Code's
  included, says so in its transcript:
  `Partitur's instructions could not be written; workflows and questions will not work on
  agents that read them from a folder`.
- Copilot updates itself unless `COPILOT_AUTO_UPDATE=false` is set; Partitur leaves that to
  you.
- Partitur speaks version 1 of the Agent Client Protocol with Copilot, which Copilot offers as
  a public preview. Nothing in the entry says it is Copilot: Partitur finds out when the agent
  starts, from the name `Copilot` it gives in its `initialize` answer.
  [Sessions](sessions.md#github-copilot) describes what differs in a Copilot session. A
  Copilot that speaks version 2 is recognised by its name too and handled the same way, held
  prompts included, but for how its tool calls are titled and named, which concerns version 1
  only.
- Any other agent, also one speaking version 1, is not treated as Copilot: its prompts are
  sent as they come, its subagents' updates are shown, a `partitur-<tool>` title is not taken
  as Partitur's own tool, and its orchestrator starts once. Like every agent it is still kept
  asking (`allow_all` off where it offers that), and it gets what goes out before the agent
  says who it is: the `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` above and the request for Copilot's
  session events, which it ignores.

## The environment on macOS

An application started from the Finder or the Dock does not get the environment of your
shell, so its `PATH` lacks Homebrew, npm and `~/.local/bin`. Partitur then reads your login
shell's environment (`$SHELL`, else `/bin/zsh`, started as a login and interactive shell) in
the background while the window opens:

- Adapters, git and a configured `link_opener` get that environment, and wait for it on their
  own threads; the window never does. A session opened before it is known opens once it is.
- The lookup may take at most 5 seconds. A shell that takes longer, cannot be started or
  prints no environment is stopped, and Partitur keeps its own `PATH` (or the system folders
  without one) with `/opt/homebrew/bin`, `/usr/local/bin` and `~/.local/bin` added where
  they exist.
- The shell must accept `-l -i -c`, as sh-compatible shells and fish do. Others, such as csh
  and tcsh, do not, and Partitur takes the fallback folders above.
- The backends are resolved once the environment is known, on that `PATH`. A configured
  command that is missing then does not stop the start: opening a session on that backend
  fails with the message that the command is not on `PATH`, and every other backend works.
- Started from a terminal, Partitur already has the shell's environment and asks for nothing.
  On Linux it never asks.

## Settings

Some settings can be given at several levels, and the most specific level that sets one wins:
`config.toml` (global), then the project, then one of its repositories, then a folder inside a
repository. A setting is only allowed at the levels listed for it; anywhere else it is an
error.

| Setting         | Allowed at                  | Default                             |
| --------------- | --------------------------- | ----------------------------------- |
| `worktree_path` | global, project, repository | `{repo_path}/../{repo}-wt/{branch}` |
| `link_opener`   | global, project, repository | the system's default handler        |

`worktree_path` is the pattern of the folder a Worktree session or child gets:

- `{repo_path}` is the repository's folder, `{repo}` its name, `{project}` the project's name
  and `{branch}` the new branch's name. Any other `{name}`, and a `{` that is never closed,
  is an error.
- A leading `~`, `~/` or `~\` stands for the home folder (without one, the pattern is an
  error); any other `~` stays as it is. A pattern that is still relative then is taken
  relative to the repository's folder, so `../trees/{branch}` puts the worktrees in `trees`
  beside it.
  `.` and `..` in the result are resolved without touching the disk, so the default puts the
  worktrees next to the repository, in `<repo>-wt`. A branch name with `/` makes nested
  folders.

`link_opener` is the command a link clicked in a transcript is opened with:

- Unset, a link goes to the system's default handler (`open` on macOS, `xdg-open` on Linux).
- The value is split on whitespace into a program and its arguments, and the link is added as
  the last argument, so `"firefox -P work"` runs `firefox -P work <link>`. No shell is
  involved and there is no quoting. A value without a program is an error.
- A repository session (also a worktree session or a child) takes it from its repository,
  then the project, then `config.toml`; a project session from the project, then
  `config.toml`.
- The project's settings are the ones read when Partitur started or the project was first
  opened, so an edit to `link_opener` there takes effect after a restart. The program is
  started in the background; when it cannot be started, or the settings could not be read,
  the error bar says why and nothing opens.

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

- Partitur reads this file when it starts (or, for a project opened later, when the project
  is first opened); restart it after editing. A mistake in it (naming the section it is in)
  stops the new-session sheet or the split card from creating anything and is shown there
  until the file is fixed and Partitur restarted. A link click reports it too.
