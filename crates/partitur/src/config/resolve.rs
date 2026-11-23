//! The resolution of the configured backends into the adapter configurations they start with.
//!
//! Each backend's command is taken from the release's adapter bundle, found on the `PATH` of
//! the process environment, or, for the default adapter command found nowhere, run through
//! `npx`; `~` in its `env` is expanded to the home folder.
//!
//! Backends are resolved on top of a process environment, such as the login shell's: its
//! variables come before a backend's `env`, which wins over them, and a bare command is looked
//! up on its `PATH` (else this process's). [`resolve_backends`] stops at the first missing
//! command; [`resolution`] resolves each backend on its own, so a missing command fails only
//! the sessions of its backend. [`resolve_at_start`] picks between them as
//! [`defers_resolution`] decides: whenever the login shell is asked for the environment,
//! resolution waits for its answer, even if it came at once, and on Windows, where a start
//! that stopped could tell nobody, it is always deferred. Both paths take the adapter bundle
//! of the release ([`bundled_adapter`](super::bundled_adapter)), which the default adapter command prefers over `PATH`
//! and `npx` when Claude Code is installed. Every backend is then given Partitur's
//! instructions folder in the data folder ([`instructions_dir`], [`instructed`],
//! [`instructed_resolution`]), whose `partitur.instructions.md` agents such as GitHub Copilot
//! CLI read Partitur's tool instructions from. On Windows every adapter is started through
//! the binary's own `--job-run` launcher ([`launcher`], [`launched`], [`launched_resolution`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use partitur_backend_acp::{ADAPTER_COMMAND, ADAPTER_PACKAGE, AcpConfig, Resolution};
use partitur_core::{BackendId, BackendInfo, ProcessEnv};

use super::bundle::bundled_launch;
use super::lookup::process_lookup;
use super::{BackendConfig, BundledAdapter, Config, ConfigError, Lookup, listing};

/// The configured backends, ready to start.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// How to start each backend's adapter.
    pub backends: HashMap<BackendId, AcpConfig>,
    /// The backends as the core and the UI show them, sorted by id.
    pub infos: Vec<BackendInfo>,
    /// The backend new sessions run on; one of the keys of `backends`.
    pub default: BackendId,
}

/// Resolves every backend `config` names on top of the process environment `base`, looking
/// commands up on its `PATH` (else this process's) and expanding `~` to the user's home folder.
///
/// See [`resolve_backends_with`].
///
/// # Errors
///
/// Returns the errors [`resolve_backends_with`] does.
pub fn resolve_backends(
    config: &Config,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> Result<Resolved, ConfigError> {
    resolve_backends_with(config, &process_lookup(base), &home_folder(), base, bundle)
}

/// Whether the backends are resolved only after the start, with [`resolution`], so a missing
/// command fails only the sessions of its backend: when `shell_asked`, as the login shell's
/// environment comes later, and always when `windows`. There the program has no console, so
/// an error that stopped the start would reach nobody.
#[must_use]
pub fn defers_resolution(shell_asked: bool, windows: bool) -> bool {
    shell_asked || windows
}

/// The backends resolved at the start, or `None` when `deferred` (see [`defers_resolution`]):
/// then the backends are resolved with [`resolution`] once the process environment is known,
/// so a missing command fails only the sessions of its backend instead of the start. That
/// holds even when `env` is known already, as after a shell lookup that failed at once or
/// on Windows. Otherwise `env` is known from the start and this does not wait.
///
/// # Errors
///
/// Returns the errors [`resolve_backends`] does when the resolution is not deferred.
pub fn resolve_at_start(
    config: &Config,
    deferred: bool,
    env: &ProcessEnv,
    bundle: Option<&BundledAdapter>,
) -> Result<Option<Resolved>, ConfigError> {
    if deferred {
        return Ok(None);
    }
    resolve_backends(config, &env.wait(), bundle).map(Some)
}

/// What the adapter runtime starts sessions with once the process environment `base` is known:
/// every backend `config` names, resolved as [`resolve_backends`] does, a backend whose command
/// is missing carrying that error's message instead. Opening a session on such a backend then
/// fails with the message while every other backend works.
pub fn resolution(
    config: &Config,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> Resolution {
    resolve_each(config, &process_lookup(base), &home_folder(), base, bundle)
        .into_iter()
        .map(|(id, result)| (id, result.map_err(|error| error.to_string())))
        .collect()
}

/// The launcher every adapter is started through, the binary's own `current_exe` in its
/// `--job-run` mode, when `windows`: there that ends an adapter's whole process tree when its
/// session stops. Elsewhere, or without a known executable, adapters start directly.
#[must_use]
pub fn launcher(windows: bool, current_exe: Option<PathBuf>) -> Option<PathBuf> {
    current_exe.filter(|_| windows)
}

/// `acp` started through `launcher`: the launcher runs as `launcher --job-run -- <command>
/// <args…>`, with the environment and the thinking display unchanged.
#[must_use]
pub fn with_launcher(acp: AcpConfig, launcher: &Path) -> AcpConfig {
    let args = ["--job-run".to_owned(), "--".to_owned()]
        .into_iter()
        .chain([acp.command.to_string_lossy().into_owned()])
        .chain(acp.args)
        .collect();
    AcpConfig {
        command: launcher.to_path_buf(),
        args,
        ..acp
    }
}

/// `resolved` with every backend started through `launcher`, if there is one; the backend
/// list keeps showing the configured commands.
#[must_use]
pub fn launched(resolved: Resolved, launcher: Option<&Path>) -> Resolved {
    let Some(launcher) = launcher else {
        return resolved;
    };
    Resolved {
        backends: resolved
            .backends
            .into_iter()
            .map(|(id, acp)| (id, with_launcher(acp, launcher)))
            .collect(),
        ..resolved
    }
}

/// `resolution` with every resolved backend started through `launcher`, if there is one; a
/// backend that failed keeps its message.
#[must_use]
pub fn launched_resolution(resolution: Resolution, launcher: Option<&Path>) -> Resolution {
    let Some(launcher) = launcher else {
        return resolution;
    };
    resolution
        .into_iter()
        .map(|(id, result)| (id, result.map(|acp| with_launcher(acp, launcher))))
        .collect()
}

/// The folder under the data folder `data_dir` that Partitur's tool instructions are written
/// to, for agents that read them from a folder.
#[must_use]
pub fn instructions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-instructions")
}

/// `resolved` with every backend's adapter given the instructions folder `dir`.
#[must_use]
pub fn instructed(resolved: Resolved, dir: &Path) -> Resolved {
    Resolved {
        backends: resolved
            .backends
            .into_iter()
            .map(|(id, acp)| (id, with_instructions(acp, dir)))
            .collect(),
        ..resolved
    }
}

/// `resolution` with every resolved backend's adapter given the instructions folder `dir`; a
/// backend that failed keeps its message.
#[must_use]
pub fn instructed_resolution(resolution: Resolution, dir: &Path) -> Resolution {
    resolution
        .into_iter()
        .map(|(id, result)| (id, result.map(|acp| with_instructions(acp, dir))))
        .collect()
}

/// `acp` with the instructions folder `dir`.
fn with_instructions(acp: AcpConfig, dir: &Path) -> AcpConfig {
    AcpConfig {
        instructions_dir: Some(dir.to_path_buf()),
        ..acp
    }
}

/// Resolves each backend `config` names on its own, as [`resolve_backends_with`] does, so a
/// backend whose command is missing has its [`ConfigError::CommandNotFound`] and leaves the
/// others resolved.
pub fn resolve_each(
    config: &Config,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> HashMap<BackendId, Result<AcpConfig, ConfigError>> {
    resolve_in_order(config, lookup, home, base, bundle).collect()
}

/// Every backend `config` names with its resolution, in id order.
fn resolve_in_order<'a>(
    config: &'a Config,
    lookup: &'a Lookup,
    home: &'a Path,
    base: &'a [(String, String)],
    bundle: Option<&'a BundledAdapter>,
) -> impl Iterator<Item = (BackendId, Result<AcpConfig, ConfigError>)> + 'a {
    config.backends.iter().map(move |(id, entry)| {
        (
            id.clone(),
            resolve_entry(id, entry, lookup, home, base, bundle),
        )
    })
}

/// Resolves every backend `config` names, looking commands up with
/// `lookup` and expanding `~` to `home`.
///
/// A command with a folder in it runs as configured, as does a bare name found as a file in
/// one of the folders. The default adapter command found nowhere falls back to running the
/// pinned Claude Code adapter package through `npx`, with the configured arguments after the
/// package name. Each backend's environment is `base` followed by its `env` in name order, so
/// a configured variable wins over one of `base`, a value of `~` or starting with `~/` having
/// that `~` replaced by `home`; the adapter's transport sets the thinking variable after them.
/// Each resolved backend is logged with its command and whether the fallback applied.
///
/// A backend whose command is exactly the default adapter command first tries the `bundle`:
/// when there is one, and both a `node` (the bundled one, else one found by `lookup`) and
/// `claude` are found, it runs `node <bundled entry> <configured args…>` with the
/// `CLAUDE_CODE_EXECUTABLE` variable naming the found `claude` after its environment. Without
/// either, the resolution goes on as above with the `PATH` and then `npx`.
///
/// The backend list is [`listing`]'s.
///
/// # Errors
///
/// Returns [`ConfigError::CommandNotFound`] for the first backend, in id order, whose command
/// is a bare name other than the default adapter command found nowhere, and
/// [`ConfigError::NoDefault`] when `config` names no default it configures (which a loaded
/// configuration always does).
pub fn resolve_backends_with(
    config: &Config,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> Result<Resolved, ConfigError> {
    let (infos, default) = listing(config)?;
    let backends = resolve_in_order(config, lookup, home, base, bundle)
        .map(|(id, result)| result.map(|acp| (id, acp)))
        .collect::<Result<_, _>>()?;
    Ok(Resolved {
        backends,
        infos,
        default,
    })
}

/// How to start the adapter of the backend `id`, configured as `entry`.
fn resolve_entry(
    id: &BackendId,
    entry: &BackendConfig,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> Result<AcpConfig, ConfigError> {
    let bundled = bundle
        .filter(|_| entry.command == Path::new(ADAPTER_COMMAND))
        .and_then(|bundle| bundled_launch(bundle, lookup));
    let has_folder = entry
        .command
        .parent()
        .is_some_and(|folder| !folder.as_os_str().is_empty());
    let found = if has_folder {
        None
    } else {
        lookup.find(&entry.command)
    };
    let installed = has_folder || found.is_some();
    let npx = !installed && entry.command == Path::new(ADAPTER_COMMAND);
    let (command, args) = if let Some(launch) = &bundled {
        let args = std::iter::once(launch.entry.as_os_str().to_string_lossy().into_owned())
            .chain(entry.args.iter().cloned())
            .collect();
        (launch.node.clone(), args)
    } else if installed {
        let command = found
            .filter(|_| lookup.full_path)
            .unwrap_or_else(|| entry.command.clone());
        (command, entry.args.clone())
    } else if npx {
        let args = ["--yes", ADAPTER_PACKAGE]
            .into_iter()
            .map(str::to_owned)
            .chain(entry.args.iter().cloned())
            .collect();
        let command = lookup
            .find(Path::new("npx"))
            .filter(|_| lookup.full_path)
            .unwrap_or_else(|| PathBuf::from("npx"));
        (command, args)
    } else {
        return Err(ConfigError::CommandNotFound {
            backend: id.clone(),
            command: entry.command.clone(),
        });
    };
    tracing::info!(
        backend = %id,
        command = %entry.command.display(),
        npx,
        bundled = bundled.is_some(),
        "resolved a backend"
    );
    let claude = bundled.map(|launch| {
        (
            "CLAUDE_CODE_EXECUTABLE".to_owned(),
            launch.claude.to_string_lossy().into_owned(),
        )
    });
    let env = base
        .iter()
        .cloned()
        .chain(
            entry
                .env
                .iter()
                .map(|(name, value)| (name.clone(), expand_home(value, home))),
        )
        .chain(claude)
        .collect();
    Ok(AcpConfig {
        command,
        args,
        env,
        thinking_display: entry.thinking,
        instructions_dir: None,
    })
}

/// The user's home folder; without one a `~` stays as written.
fn home_folder() -> PathBuf {
    BaseDirs::new().map_or_else(|| PathBuf::from("~"), |dirs| dirs.home_dir().into())
}

/// `value` with a leading `~` (alone or followed by `/` or `\`) replaced by `home`; any other
/// value unchanged. Extra separators after the `~` are dropped, so the result stays under `home`
/// (joining an absolute rest would replace `home` altogether).
fn expand_home(value: &str, home: &Path) -> String {
    if value == "~" {
        home.display().to_string()
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        home.join(rest.trim_start_matches(['/', '\\']))
            .display()
            .to_string()
    } else {
        value.to_owned()
    }
}
