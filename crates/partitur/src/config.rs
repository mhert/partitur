//! The user's configuration file: which agent backends there are and how each one's adapter
//! is started.
//!
//! The file is TOML. Every entry of the `backends` table is one backend, named by its key:
//!
//! ```toml
//! default = "claude-main"            # the backend new sessions run on
//!
//! [backends.claude-main]
//! command = "claude-agent-acp"       # the ACP adapter; a bare name is looked up on PATH
//! env = { CLAUDE_CONFIG_DIR = "~/.claude-main" }
//!
//! [backends.claude-work]
//! command = "claude-agent-acp"
//! args = []                          # the adapter's arguments; none by default
//! env = { CLAUDE_CONFIG_DIR = "~/.claude-work" }
//! thinking = "summarized"            # or "omitted"; summarized by default
//! ```
//!
//! A GitHub Copilot CLI backend is an entry like any other, with `copilot --acp` as its
//! command; each profile keeps its own `COPILOT_HOME`, and Claude and Copilot backends mix in
//! one file:
//!
//! ```toml
//! [backends.copilot-main]
//! command = "copilot"
//! args = ["--acp"]
//! env = { COPILOT_HOME = "~/.copilot-main" }
//!
//! [backends.copilot-work]
//! command = "copilot"
//! args = ["--acp"]
//! env = { COPILOT_HOME = "~/.copilot-work", COPILOT_GITHUB_TOKEN = "…" }
//! ```
//!
//! A top-level `worktree_path` sets the global default for where new worktrees are created:
//!
//! ```toml
//! worktree_path = "~/worktrees/{repo}/{branch}"
//! ```
//!
//! An unknown `{placeholder}` in it stops the start with an error naming the file.
//!
//! A top-level `link_opener` sets the global command a clicked link is opened with; it is
//! split on whitespace and the link is appended as the last argument. Left out, links open
//! with the system's default handler (`open` on macOS, `xdg-open` on Linux); a value without
//! a program stops the start the same way:
//!
//! ```toml
//! link_opener = "firefox -P work"
//! ```
//!
//! A top-level `window_frame` says who draws the window's title bar: `"partitur"` (the
//! default) makes Partitur's toolbar the title bar, `"system"` keeps the desktop's own above
//! it; any other value stops the start:
//!
//! ```toml
//! window_frame = "system"
//! ```
//!
//! A name must be non-empty and contain no `/` or `\`. `command` is required; `default` may
//! be left out when only one backend is configured.
//!
//! Backends are resolved on top of a process environment, such as the login shell's, each
//! one's command looked up on its `PATH` and its `env` winning over it.
//!
//! The work is split in five parts: `file` holds the format, its loading and validation and
//! the errors; `settings` the global settings layer the file sets; `lookup` where a bare
//! command is looked up; `resolve` the resolution of the backends into adapter
//! configurations; and `bundle` the layout of the adapter bundle a release ships.

mod bundle;
mod file;
mod lookup;
mod resolve;
mod settings;

pub use bundle::{BundledAdapter, bundled_adapter};
pub use file::{BackendConfig, Config, ConfigError, listing, window_frame};
pub use lookup::Lookup;
pub use resolve::{Launch, defers_resolution, launcher, resolution, resolve_at_start};
pub use settings::global_settings;

#[cfg(test)]
mod tests;
