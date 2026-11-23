//! The adapter bundle a release ships beside the executable: where its adapter package and the
//! Node.js it may bring sit for each platform's layout ([`bundled_adapter`]), and how it runs
//! with the Node.js and the Claude Code executable found for it.

use std::path::{Path, PathBuf};

use super::Lookup;

/// The adapter package a release ships beside the executable, with the Node.js it may bring.
///
/// Found by [`bundled_adapter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledAdapter {
    /// The adapter's `dist/index.js`.
    pub entry: PathBuf,
    /// The bundled Node.js executable, when the release ships one.
    pub node: Option<PathBuf>,
}

/// The adapter bundle a release ships for the executable `exe`, if it is there.
///
/// The adapter folder sits relative to the executable's folder: `../lib/partitur/adapter` on
/// Linux, `../Resources/adapter` in a macOS bundle (`macos`) and `adapter` on Windows
/// (`windows`). Inside it the entry is `node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js`
/// and the Node.js is `node/bin/node` (`node/node.exe` on Windows) when that file exists. Without
/// the entry file the result is `None`.
#[must_use]
pub fn bundled_adapter(exe: &Path, windows: bool, macos: bool) -> Option<BundledAdapter> {
    let folder = exe.parent()?;
    let adapter = if windows {
        folder.join("adapter")
    } else if macos {
        folder.parent()?.join("Resources/adapter")
    } else {
        folder.parent()?.join("lib/partitur/adapter")
    };
    let entry = adapter.join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js");
    if !entry.is_file() {
        return None;
    }
    let node = adapter.join(if windows {
        "node/node.exe"
    } else {
        "node/bin/node"
    });
    let node = node.is_file().then_some(node);
    Some(BundledAdapter { entry, node })
}

/// How the bundled adapter runs: with which Node.js and Claude Code executable.
pub(super) struct BundledLaunch {
    pub(super) entry: PathBuf,
    pub(super) node: PathBuf,
    pub(super) claude: PathBuf,
}

/// The launch of `bundle`: its own Node.js, else the one `lookup` finds, and the `claude`
/// `lookup` finds. Without either, `None`, with the reason logged.
pub(super) fn bundled_launch(bundle: &BundledAdapter, lookup: &Lookup) -> Option<BundledLaunch> {
    let Some(node) = bundle
        .node
        .clone()
        .or_else(|| lookup.find(Path::new("node")))
    else {
        tracing::info!("not using the bundled adapter: no Node.js found");
        return None;
    };
    let Some(claude) = lookup.find(Path::new("claude")) else {
        tracing::info!("not using the bundled adapter: no Claude Code found");
        return None;
    };
    Some(BundledLaunch {
        entry: bundle.entry.clone(),
        node,
        claude,
    })
}
