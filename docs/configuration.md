# Configuration

## Where things live

- Configuration: `$XDG_CONFIG_HOME/partitur/config.toml` (usually
  `~/.config/partitur/config.toml`).
- Projects, sessions and transcripts: `$XDG_DATA_HOME/partitur` (usually
  `~/.local/share/partitur`).
- A session's files: `projects/<project-id>/<session-id>/` in that data folder, holding
  `<session-id>_meta.json` (the session's record) and `<session-id>_history.jsonl` (its
  transcript, including the workflow reports of its [workflow](workflows.md)).
- A project's settings: `projects/<project-id>/settings.toml` in that data folder. Partitur
  writes it, all in comments, the first time the project's settings are read; see
  [Settings](#settings).

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

## Settings

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
