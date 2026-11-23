#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use directories::BaseDirs;
use partitur_backend_acp::{ADAPTER_COMMAND, AcpConfig, ThinkingDisplay};
use partitur_core::{
    BackendId, BackendInfo, LinkOpenerError, PatternError, ProcessEnv, SettingError,
};
use partitur_iced::WindowFrame;
use tempfile::TempDir;

use super::lookup::env_name_eq;
use super::resolve::{
    Resolved, resolve_backends, resolve_backends_with, resolve_each, with_launcher,
};
use super::*;

const TWO_BACKENDS: &str = r#"
default = "claude-main"

[backends.claude-main]
command = "claude-agent-acp"
env = { CLAUDE_CONFIG_DIR = "~/.claude-main" }

[backends.claude-work]
command = "/opt/acp/bin/agent"
args = ["--verbose"]
env = { CLAUDE_CONFIG_DIR = "~/.claude-work", AGENT_LOG = "1" }
thinking = "omitted"
"#;

fn parse(content: &str) -> Config {
    toml::from_str(content).unwrap()
}

#[test]
fn the_window_frame_defaults_to_partiturs_own() {
    assert_eq!(window_frame(&parse("")), WindowFrame::Partitur);
}

#[test]
fn the_window_frame_can_be_the_systems() {
    assert_eq!(
        window_frame(&parse(r#"window_frame = "system""#)),
        WindowFrame::System
    );
    assert_eq!(
        window_frame(&parse(r#"window_frame = "partitur""#)),
        WindowFrame::Partitur
    );
}

#[test]
fn an_unknown_window_frame_is_an_error() {
    assert!(toml::from_str::<Config>(r#"window_frame = "fancy""#).is_err());
}

fn config_file(content: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, content).unwrap();
    (dir, path)
}

fn id(name: &str) -> BackendId {
    BackendId::try_from(name).unwrap()
}

fn entry(command: &str, args: &[&str]) -> BackendConfig {
    BackendConfig {
        command: PathBuf::from(command),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        env: BTreeMap::new(),
        thinking: ThinkingDisplay::Omitted,
    }
}

/// A configuration of the given entries without a `default`.
fn config_of(entries: &[(&str, BackendConfig)]) -> Config {
    Config {
        default: None,
        worktree_path: None,
        link_opener: None,
        window_frame: None,
        backends: entries
            .iter()
            .map(|(name, entry)| (id(name), entry.clone()))
            .collect(),
    }
}

fn home() -> PathBuf {
    PathBuf::from("/home/me")
}

/// `rest` under [`home`], joined as the expansion joins it: with `\` on Windows.
fn under_home(rest: &str) -> String {
    home().join(rest).display().to_string()
}

/// `folders` as one `PATH` value, with the platform's separator.
fn path_of(folders: &[&str]) -> String {
    env::join_paths(folders).unwrap().into_string().unwrap()
}

#[test]
fn the_launcher_wraps_the_adapter_command() {
    let acp = AcpConfig {
        command: PathBuf::from("/n/npx.cmd"),
        args: vec!["--yes".to_owned(), "pkg".to_owned()],
        env: vec![("CLAUDE_CONFIG_DIR".to_owned(), "/c".to_owned())],
        thinking_display: ThinkingDisplay::Omitted,
        instructions_dir: Some(PathBuf::from("/i")),
    };

    let wrapped = with_launcher(acp, Path::new("/l/partitur.exe"));

    assert_eq!(wrapped.command, PathBuf::from("/l/partitur.exe"));
    assert_eq!(
        wrapped.args,
        ["--job-run", "--", "/n/npx.cmd", "--yes", "pkg"]
    );
    assert_eq!(
        wrapped.env,
        [("CLAUDE_CONFIG_DIR".to_owned(), "/c".to_owned())]
    );
    assert_eq!(wrapped.thinking_display, ThinkingDisplay::Omitted);
    assert_eq!(wrapped.instructions_dir, Some(PathBuf::from("/i")));
}

#[test]
fn only_windows_runs_adapters_through_the_launcher() {
    let exe = PathBuf::from("/l/partitur.exe");
    assert_eq!(launcher(true, Some(exe.clone())), Some(exe.clone()));
    assert_eq!(launcher(false, Some(exe)), None);
    assert_eq!(launcher(true, None), None);
}

/// Resolves `config` as the start does, with no launcher and no instructions folder.
fn resolve_plain(
    config: &Config,
    lookup: &Lookup,
    home: &Path,
    base: &[(String, String)],
    bundle: Option<&BundledAdapter>,
) -> Result<Resolved, ConfigError> {
    resolve_backends_with(config, lookup, home, base, bundle, &Launch::default())
}

/// A backend `a` and a backend `b` whose commands have a folder, so both are found, and,
/// with `missing`, a backend `c` whose command is found nowhere.
fn launch_fixture(missing: bool) -> Config {
    let found = [("a", entry("/x/a", &["a"])), ("b", entry("/x/b", &["a"]))];
    let mut config = if missing {
        let [a, b] = found;
        config_of(&[a, b, ("c", entry("partitur-no-such-adapter", &[]))])
    } else {
        config_of(&found)
    };
    config.default = Some(id("a"));
    config
}

const MISSING_C: &str = "the adapter command partitur-no-such-adapter of backend c is not on PATH";

#[test]
fn resolved_backends_carry_the_instructions_dir() {
    let data = Path::new("/data");
    let dir = instructions_dir(data);
    assert_eq!(dir, data.join("agent-instructions"));
    let launch = Launch {
        launcher: None,
        instructions: Some(dir.clone()),
    };
    let base = pairs(&[("PATH", "/nowhere")]);
    let instructed_at_start =
        resolve_backends(&launch_fixture(false), &base, None, &launch).unwrap();
    let plain = resolve_backends(&launch_fixture(false), &base, None, &Launch::default()).unwrap();
    for id in [id("a"), id("b")] {
        assert_eq!(
            instructed_at_start.backends[&id]
                .instructions_dir
                .as_deref(),
            Some(dir.as_path())
        );
        assert_eq!(plain.backends[&id].instructions_dir, None);
    }
    assert_eq!(
        command_line(&instructed_at_start.backends[&id("a")]),
        command_line(&plain.backends[&id("a")])
    );
    assert_eq!(instructed_at_start.infos, plain.infos);
    assert_eq!(instructed_at_start.default, id("a"));

    let instructed_later = resolution(&launch_fixture(true), &base, None, &launch);
    assert_eq!(
        instructed_later[&id("a")]
            .as_ref()
            .map(|acp| acp.instructions_dir.clone()),
        Ok(Some(dir))
    );
    assert_eq!(
        instructed_later[&id("c")].as_ref().err(),
        Some(&MISSING_C.to_owned())
    );
}

/// The command and arguments `acp` starts.
fn command_line(acp: &AcpConfig) -> (PathBuf, Vec<String>) {
    (acp.command.clone(), acp.args.clone())
}

fn wrapped_line(command: &str) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/l/partitur.exe"),
        ["--job-run", "--", command, "a"]
            .map(str::to_owned)
            .to_vec(),
    )
}

fn through_launcher() -> Launch {
    Launch {
        launcher: Some(PathBuf::from("/l/partitur.exe")),
        instructions: None,
    }
}

#[test]
fn a_launcher_wraps_every_backend_resolved_at_the_start_but_not_its_listing() {
    let base = pairs(&[("PATH", "/nowhere")]);
    let resolved =
        || resolve_backends(&launch_fixture(false), &base, None, &Launch::default()).unwrap();

    let wrapped =
        resolve_backends(&launch_fixture(false), &base, None, &through_launcher()).unwrap();

    assert_eq!(
        command_line(&wrapped.backends[&id("a")]),
        wrapped_line("/x/a")
    );
    assert_eq!(
        command_line(&wrapped.backends[&id("b")]),
        wrapped_line("/x/b")
    );
    assert_eq!(wrapped.backends.len(), 2);
    assert_eq!(wrapped.infos, resolved().infos);
    assert_eq!(wrapped.default, id("a"));
    let unwrapped = resolved();
    assert_eq!(
        command_line(&unwrapped.backends[&id("a")]),
        (PathBuf::from("/x/a"), vec!["a".to_owned()])
    );
    assert_eq!(unwrapped.backends.len(), 2);
}

#[test]
fn a_launcher_wraps_every_backend_of_a_resolution_and_keeps_its_errors() {
    let base = pairs(&[("PATH", "/nowhere")]);
    let resolution = |launch: &Launch| resolution(&launch_fixture(true), &base, None, launch);

    let wrapped = resolution(&through_launcher());

    assert_eq!(
        wrapped[&id("a")].as_ref().map(command_line),
        Ok(wrapped_line("/x/a"))
    );
    assert_eq!(
        wrapped[&id("c")].as_ref().err(),
        Some(&MISSING_C.to_owned())
    );
    assert_eq!(wrapped.len(), 3);
    let unwrapped = resolution(&Launch::default());
    assert_eq!(
        unwrapped[&id("a")].as_ref().map(command_line),
        Ok((PathBuf::from("/x/a"), vec!["a".to_owned()]))
    );
    assert_eq!(
        unwrapped[&id("c")].as_ref().err(),
        Some(&MISSING_C.to_owned())
    );
}

#[test]
fn a_top_level_worktree_path_becomes_the_global_layer() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "worktree_path = \"~/wt/{branch}\"\n[backends.a]\ncommand=\"x\"",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();

    let layer = global_settings(&config, &path).unwrap();

    assert_eq!(layer.worktree_path().unwrap().as_str(), "~/wt/{branch}");
}

#[test]
fn an_unknown_placeholder_in_config_stops_the_start() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "worktree_path = \"~/wt/{nope}\"\n[backends.a]\ncommand=\"x\"",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();

    let error = global_settings(&config, &path).unwrap_err();

    assert!(matches!(
        &error,
        ConfigError::Setting { path: p, key: "worktree_path", source: SettingError::Pattern(PatternError::UnknownPlaceholder(name)) }
            if *p == path && name == "nope"
    ));
    assert!(
        error
            .to_string()
            .starts_with("invalid worktree_path in the config file")
    );
}

#[test]
fn without_the_key_the_global_layer_is_empty() {
    let layer = global_settings(&Config::builtin(), Path::new("config.toml")).unwrap();

    assert_eq!(layer.worktree_path(), None);
    assert_eq!(layer.link_opener(), None);
}

#[test]
fn a_top_level_link_opener_becomes_the_global_layer() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "link_opener = \"firefox -P work\"\n[backends.a]\ncommand=\"x\"",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();

    let layer = global_settings(&config, &path).unwrap();

    assert_eq!(layer.link_opener().unwrap().to_string(), "firefox -P work");
    assert_eq!(layer.worktree_path(), None);
}

#[test]
fn a_link_opener_without_a_program_stops_the_start() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "worktree_path = \"~/wt/{branch}\"\nlink_opener = \" \"\n[backends.a]\ncommand=\"x\"",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();

    let error = global_settings(&config, &path).unwrap_err();

    assert!(matches!(
        &error,
        ConfigError::Setting { path: p, key: "link_opener", source: SettingError::LinkOpener(LinkOpenerError::Empty) }
            if *p == path
    ));
    assert!(
        error
            .to_string()
            .starts_with("invalid link_opener in the config file")
    );
}

#[test]
fn a_missing_config_is_the_builtin_claude_backend() {
    let dir = TempDir::new().unwrap();

    let config = Config::load(&dir.path().join("config.toml")).unwrap();

    assert_eq!(config, Config::builtin());
    assert_eq!(config.default, Some(id("claude")));
    assert_eq!(
        config.backends,
        BTreeMap::from([(
            id("claude"),
            BackendConfig {
                command: PathBuf::from("claude-agent-acp"),
                args: Vec::new(),
                env: BTreeMap::new(),
                thinking: ThinkingDisplay::Summarized,
            }
        )])
    );
}

#[test]
fn the_backends_table_parses_with_optional_fields() {
    let (_dir, path) = config_file(TWO_BACKENDS);

    let config = Config::load(&path).unwrap();

    assert_eq!(config.default, Some(id("claude-main")));
    assert_eq!(
        config.backends,
        BTreeMap::from([
            (
                id("claude-main"),
                BackendConfig {
                    command: PathBuf::from("claude-agent-acp"),
                    args: Vec::new(),
                    env: BTreeMap::from([(
                        "CLAUDE_CONFIG_DIR".to_owned(),
                        "~/.claude-main".to_owned()
                    )]),
                    thinking: ThinkingDisplay::Summarized,
                }
            ),
            (
                id("claude-work"),
                BackendConfig {
                    command: PathBuf::from("/opt/acp/bin/agent"),
                    args: vec!["--verbose".to_owned()],
                    env: BTreeMap::from([
                        ("AGENT_LOG".to_owned(), "1".to_owned()),
                        ("CLAUDE_CONFIG_DIR".to_owned(), "~/.claude-work".to_owned()),
                    ]),
                    thinking: ThinkingDisplay::Omitted,
                }
            ),
        ])
    );
}

#[test]
fn copilot_profiles_parse_beside_claude() {
    let (_dir, path) = config_file(
        r#"
default = "claude-main"

[backends.claude-main]
command = "claude-agent-acp"

[backends.copilot-main]
command = "copilot"
args = ["--acp"]
env = { COPILOT_HOME = "~/.copilot-main" }

[backends.copilot-work]
command = "copilot"
args = ["--acp"]
env = { COPILOT_HOME = "~/.copilot-work", COPILOT_GITHUB_TOKEN = "t" }
"#,
    );
    let bin = TempDir::new().unwrap();
    touch(&bin, &["claude-agent-acp", "copilot"]);

    let config = Config::load(&path).unwrap();
    let resolved = resolve_plain(
        &config,
        &plain(&[bin.path().to_path_buf()]),
        &home(),
        &[],
        None,
    )
    .unwrap();

    assert_eq!(resolved.backends.len(), 3);
    assert_eq!(resolved.default, id("claude-main"));
    let main = &resolved.backends[&id("copilot-main")];
    assert_eq!(main.command, PathBuf::from("copilot"));
    assert_eq!(main.args, vec!["--acp".to_owned()]);
    assert_eq!(
        main.env,
        vec![("COPILOT_HOME".to_owned(), under_home(".copilot-main"))]
    );
    let work = &resolved.backends[&id("copilot-work")];
    assert_eq!(work.args, vec!["--acp".to_owned()]);
    assert_eq!(
        work.env,
        vec![
            ("COPILOT_GITHUB_TOKEN".to_owned(), "t".to_owned()),
            ("COPILOT_HOME".to_owned(), under_home(".copilot-work")),
        ]
    );
}

#[test]
fn one_backend_needs_no_default_two_do() {
    let (_dir, path) = config_file("[backends.solo]\ncommand = \"/opt/agent\"\n");
    let config = Config::load(&path).unwrap();
    assert_eq!(config.default, None);
    let resolved = resolve_plain(&config, &Lookup::default(), &home(), &[], None).unwrap();
    assert_eq!(resolved.default, id("solo"));

    let (_dir, path) =
        config_file("[backends.a]\ncommand = \"/opt/a\"\n[backends.b]\ncommand = \"/opt/b\"\n");
    let error = Config::load(&path).unwrap_err();
    assert!(matches!(&error, ConfigError::MissingDefault { path: named } if *named == path));
    assert_eq!(
        error.to_string(),
        format!(
            "the config file {} configures several backends but names no default",
            path.display()
        )
    );

    let (_dir, path) = config_file(
        "default = \"b\"\n[backends.a]\ncommand = \"/opt/a\"\n[backends.b]\ncommand = \"/opt/b\"\n",
    );
    let resolved = resolve_plain(
        &Config::load(&path).unwrap(),
        &Lookup::default(),
        &home(),
        &[],
        None,
    )
    .unwrap();
    assert_eq!(resolved.default, id("b"));
}

#[test]
fn an_unknown_default_names_the_file() {
    for content in [
        "default = \"gone\"\n[backends.solo]\ncommand = \"/opt/agent\"\n",
        "default = \"gone\"\n[backends.a]\ncommand = \"/opt/a\"\n[backends.b]\ncommand = \"/opt/b\"\n",
    ] {
        let (_dir, path) = config_file(content);

        let error = Config::load(&path).unwrap_err();

        assert!(
            matches!(&error, ConfigError::UnknownDefault { path: named, default } if *named == path && *default == id("gone")),
            "{content}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "the config file {} names gone as the default backend, which it does not configure",
                path.display()
            )
        );
    }
}

#[test]
fn an_empty_table_is_an_error() {
    for content in ["", "[backends]\n", "default = \"claude\"\n[backends]\n"] {
        let (_dir, path) = config_file(content);

        let error = Config::load(&path).unwrap_err();

        assert!(
            matches!(&error, ConfigError::NoBackends { path: named } if *named == path),
            "{content:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("the config file {} configures no backend", path.display())
        );
    }
}

#[test]
fn the_old_single_backend_block_is_rejected() {
    let (_dir, path) = config_file("[backend]\ncommand = \"claude-agent-acp\"\n");

    let error = Config::load(&path).unwrap_err();

    assert!(matches!(&error, ConfigError::Parse { path: named, .. } if *named == path));
}

#[test]
fn an_invalid_name_a_missing_command_or_an_unknown_key_is_a_parse_error() {
    for content in [
        "[backends.\"a/b\"]\ncommand = \"/opt/agent\"\n",
        "[backends.\"\"]\ncommand = \"/opt/agent\"\n",
        "default = \"a/b\"\n[backends.a]\ncommand = \"/opt/agent\"\n",
        "[backends.a]\nargs = []\n",
        "[backends.a]\ncommand = \"/opt/agent\"\ncomand = \"x\"\n",
        "[backends.a]\ncommand = \"/opt/agent\"\nthinking = \"verbose\"\n",
        "[backends.a]\ncommand = \"/opt/agent\"\nenv = { A = 1 }\n",
        "colour = \"blue\"\n[backends.a]\ncommand = \"/opt/agent\"\n",
    ] {
        let (_dir, path) = config_file(content);

        let error = Config::load(&path).unwrap_err();

        assert!(
            matches!(&error, ConfigError::Parse { path: named, .. } if *named == path),
            "{content}"
        );
        assert!(error.to_string().contains(&path.display().to_string()));
    }
}

#[test]
fn malformed_toml_is_a_parse_error_with_the_path() {
    let (_dir, path) = config_file("[backends\ncommand = ");

    let error = Config::load(&path).unwrap_err();

    assert!(matches!(&error, ConfigError::Parse { path: named, .. } if *named == path));
    assert!(error.to_string().contains(&path.display().to_string()));
}

#[test]
fn an_unreadable_config_is_an_io_error_with_the_path() {
    let dir = TempDir::new().unwrap();

    let error = Config::load(dir.path()).unwrap_err();

    assert!(matches!(&error, ConfigError::Io { path, .. } if path == dir.path()));
    assert!(
        error
            .to_string()
            .contains(&dir.path().display().to_string())
    );
}

#[test]
fn tilde_expands_only_at_the_start_of_a_value() {
    let mut work = entry("/opt/agent", &[]);
    work.env = BTreeMap::from([
        ("A_DIR".to_owned(), "~/x".to_owned()),
        ("B_HOME".to_owned(), "~".to_owned()),
        ("C_PATH".to_owned(), "/opt/~x".to_owned()),
        ("D_USER".to_owned(), "~user".to_owned()),
        ("E_NESTED".to_owned(), "~/a/~/b".to_owned()),
        ("F_SLASHES".to_owned(), "~//x".to_owned()),
        ("G_BACKSLASH".to_owned(), r"~\bin".to_owned()),
        ("H_INNER".to_owned(), r"a~\b".to_owned()),
    ]);

    let resolved = resolve_plain(
        &config_of(&[("work", work)]),
        &Lookup::default(),
        &home(),
        &[],
        None,
    )
    .unwrap();

    let (x, nested, bin) = (under_home("x"), under_home("a/~/b"), under_home("bin"));
    let expected: Vec<(String, String)> = [
        ("A_DIR", x.as_str()),
        ("B_HOME", "/home/me"),
        ("C_PATH", "/opt/~x"),
        ("D_USER", "~user"),
        ("E_NESTED", nested.as_str()),
        ("F_SLASHES", x.as_str()),
        ("G_BACKSLASH", bin.as_str()),
        ("H_INNER", r"a~\b"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    assert_eq!(resolved.backends[&id("work")].env, expected);
}

#[test]
fn thinking_is_applied_after_env() {
    let mut work = entry("/opt/agent", &[]);
    work.env = BTreeMap::from([
        (
            "CLAUDE_CODE_THINKING_DISPLAY".to_owned(),
            "summarized".to_owned(),
        ),
        ("AGENT_LOG".to_owned(), "1".to_owned()),
    ]);

    let resolved = resolve_plain(
        &config_of(&[("work", work)]),
        &Lookup::default(),
        &home(),
        &[],
        None,
    )
    .unwrap();

    // The configured variables go first, in name order; the adapter's transport sets
    // `CLAUDE_CODE_THINKING_DISPLAY` from `thinking_display` after them, so `thinking`
    // wins over a variable of the same name.
    let acp = &resolved.backends[&id("work")];
    assert_eq!(
        acp.env,
        [
            ("AGENT_LOG".to_owned(), "1".to_owned()),
            (
                "CLAUDE_CODE_THINKING_DISPLAY".to_owned(),
                "summarized".to_owned()
            ),
        ]
    );
    assert_eq!(acp.thinking_display, ThinkingDisplay::Omitted);
}

#[test]
fn the_npx_fallback_applies_per_default_command_entry() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("claude-agent-acp")).unwrap();
    fs::create_dir(dir.path().join("my-adapter")).unwrap();
    let path = [dir.path().to_path_buf()];
    let fallback = entry("claude-agent-acp", &["-v"]);

    let resolved = resolve_plain(
        &config_of(&[("main", fallback.clone())]),
        &plain(&path),
        &home(),
        &[],
        None,
    )
    .unwrap();
    let acp = &resolved.backends[&id("main")];
    assert_eq!(acp.command, PathBuf::from("npx"));
    assert_eq!(
        acp.args,
        [
            "--yes",
            "@agentclientprotocol/claude-agent-acp@0.108.0",
            "-v"
        ]
    );
    assert_eq!(acp.thinking_display, ThinkingDisplay::Omitted);
    // The list shows what the user configured, not the fallback.
    assert_eq!(resolved.infos[0].command, "claude-agent-acp");

    let mut both = config_of(&[("main", fallback), ("custom", entry("my-adapter", &[]))]);
    both.default = Some(id("main"));
    let error = resolve_plain(&both, &plain(&path), &home(), &[], None).unwrap_err();
    assert!(matches!(
        &error,
        ConfigError::CommandNotFound { backend, command }
            if *backend == id("custom") && *command == Path::new("my-adapter")
    ));
    assert_eq!(
        error.to_string(),
        "the adapter command my-adapter of backend custom is not on PATH"
    );
}

#[test]
fn a_command_found_on_the_path_runs_as_configured() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("claude-agent-acp"), "").unwrap();
    fs::write(dir.path().join("my-adapter"), "").unwrap();
    let path = [PathBuf::from("/nowhere"), dir.path().to_path_buf()];
    let mut config = config_of(&[
        ("main", entry("claude-agent-acp", &["-v"])),
        ("custom", entry("my-adapter", &[])),
    ]);
    config.default = Some(id("custom"));

    let resolved = resolve_plain(&config, &plain(&path), &home(), &[], None).unwrap();

    let main = &resolved.backends[&id("main")];
    assert_eq!(main.command, PathBuf::from("claude-agent-acp"));
    assert_eq!(main.args, ["-v"]);
    assert!(main.env.is_empty());
    assert_eq!(
        resolved.backends[&id("custom")].command,
        PathBuf::from("my-adapter")
    );
    assert_eq!(resolved.default, id("custom"));
}

#[test]
fn a_command_with_a_folder_runs_as_configured_even_when_missing() {
    for command in ["/opt/missing/agent", "bin/agent"] {
        let resolved = resolve_plain(
            &config_of(&[("a", entry(command, &[]))]),
            &Lookup::default(),
            &home(),
            &[],
            None,
        )
        .unwrap();

        let acp = &resolved.backends[&id("a")];
        assert_eq!(acp.command, PathBuf::from(command));
        assert!(acp.args.is_empty());
    }
}

#[test]
fn the_list_is_sorted_by_id_and_shows_the_configured_command() {
    let (_dir, path) = config_file(TWO_BACKENDS);
    let config = Config::load(&path).unwrap();

    let resolved = resolve_plain(&config, &Lookup::default(), &home(), &[], None).unwrap();

    assert_eq!(
        resolved.infos,
        [
            BackendInfo {
                id: id("claude-main"),
                name: "claude-main".into(),
                command: "claude-agent-acp".into(),
            },
            BackendInfo {
                id: id("claude-work"),
                name: "claude-work".into(),
                command: "/opt/acp/bin/agent".into(),
            },
        ]
    );
    let mut keys: Vec<_> = resolved.backends.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, [id("claude-main"), id("claude-work")]);
    assert_eq!(resolved.default, id("claude-main"));
    let work = &resolved.backends[&id("claude-work")];
    assert_eq!(work.args, ["--verbose"]);
    assert_eq!(
        work.env,
        [
            ("AGENT_LOG".to_owned(), "1".to_owned()),
            ("CLAUDE_CONFIG_DIR".to_owned(), under_home(".claude-work")),
        ]
    );
}

#[test]
fn a_configuration_built_in_code_without_a_usable_default_is_refused() {
    let mut unknown = config_of(&[("a", entry("/opt/a", &[]))]);
    unknown.default = Some(id("gone"));
    let several = config_of(&[("a", entry("/opt/a", &[])), ("b", entry("/opt/b", &[]))]);

    for config in [config_of(&[]), unknown, several] {
        let error = resolve_plain(&config, &Lookup::default(), &home(), &[], None).unwrap_err();

        assert!(matches!(error, ConfigError::NoDefault), "{config:?}");
        assert_eq!(
            error.to_string(),
            "the configuration names no default backend it configures"
        );
    }
}

#[test]
fn each_resolved_backend_is_logged_at_info() {
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("log");
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(Arc::new(fs::File::create(&log).unwrap()))
        .finish();
    let mut config = config_of(&[
        ("main", entry("claude-agent-acp", &[])),
        ("work", entry("/opt/agent", &[])),
    ]);
    config.default = Some(id("main"));

    // While only one subscriber exists, tracing decides whether a log call is enabled
    // from the subscriber of whichever thread hits the call first. Other tests resolve
    // backends on other threads without one, and could cache it as disabled for good. A
    // second, silent subscriber makes tracing ask every live subscriber instead.
    let _silent = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    tracing::subscriber::with_default(subscriber, || {
        resolve_plain(&config, &Lookup::default(), &home(), &[], None).unwrap();
    });

    let written = fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines.len(), 2, "{written}");
    assert!(lines[0].contains("INFO"), "{written}");
    assert!(lines[0].contains("backend=main"), "{written}");
    assert!(lines[0].contains("command=claude-agent-acp"), "{written}");
    assert!(lines[0].contains("npx=true"), "{written}");
    assert!(lines[1].contains("backend=work"), "{written}");
    assert!(lines[1].contains("command=/opt/agent"), "{written}");
    assert!(lines[1].contains("npx=false"), "{written}");
}

fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn the_base_env_comes_before_the_configured_env() {
    let mut work = entry("/opt/agent", &[]);
    work.env = BTreeMap::from([
        ("A_DIR".to_owned(), "~/a".to_owned()),
        ("PATH".to_owned(), "/configured".to_owned()),
    ]);
    let base = pairs(&[("PATH", "/shell/bin"), ("Z_TOKEN", "t")]);

    let resolved = resolve_plain(
        &config_of(&[("work", work)]),
        &Lookup::default(),
        &home(),
        &base,
        None,
    )
    .unwrap();

    // Later entries win, so a configured variable overrides the shell's of the same name.
    assert_eq!(
        resolved.backends[&id("work")].env,
        pairs(&[
            ("PATH", "/shell/bin"),
            ("Z_TOKEN", "t"),
            ("A_DIR", &under_home("a")),
            ("PATH", "/configured"),
        ])
    );
}

fn plain(folders: &[PathBuf]) -> Lookup {
    Lookup {
        folders: folders.to_vec(),
        ..Lookup::default()
    }
}

fn windows(dir: &TempDir) -> Lookup {
    let base = pairs(&[("Path", &dir.path().display().to_string())]);
    Lookup::from_env(&base, None, None, true)
}

fn touch(dir: &TempDir, names: &[&str]) {
    for name in names {
        fs::write(dir.path().join(name), "").unwrap();
    }
}

#[test]
fn the_lookup_prefers_the_base_path() {
    let (first, last) = (path_of(&["/a", "/b"]), path_of(&["/c", "/d"]));
    let base = pairs(&[("HOME", "/h"), ("PATH", &first), ("PATH", &last)]);

    let lookup = Lookup::from_env(&base, Some("/process".into()), None, false);

    // The last `PATH` is the one a child gets.
    assert_eq!(lookup.folders, [PathBuf::from("/c"), PathBuf::from("/d")]);
    assert!(lookup.extensions.is_empty());
    assert!(!lookup.full_path);
}

#[test]
fn the_lookup_falls_back_to_the_process_path() {
    let base = pairs(&[("HOME", "/h")]);

    assert_eq!(
        Lookup::from_env(&base, Some(path_of(&["/p", "/q"]).into()), None, false).folders,
        [PathBuf::from("/p"), PathBuf::from("/q")]
    );
    assert!(
        Lookup::from_env(&base, None, None, false)
            .folders
            .is_empty()
    );
}

#[test]
fn env_name_eq_folds_case_only_when_asked() {
    assert!(env_name_eq("Path", "PATH", true));
    assert!(!env_name_eq("Path", "PATH", false));
    assert!(env_name_eq("PATH", "PATH", false));
    assert!(!env_name_eq("PATHS", "PATH", true));
}

#[test]
fn the_windows_lookup_reads_path_in_any_case() {
    let base = pairs(&[("Path", "/win")]);

    let lookup = Lookup::from_env(&base, Some("/process".into()), None, true);

    assert_eq!(lookup.folders, [PathBuf::from("/win")]);
    assert!(lookup.full_path);
}

#[test]
fn another_system_ignores_a_path_of_another_case() {
    let base = pairs(&[("Path", "/win")]);

    let lookup = Lookup::from_env(&base, Some("/process".into()), None, false);

    assert_eq!(lookup.folders, [PathBuf::from("/process")]);
}

#[test]
fn pathext_defaults_and_splits() {
    let default = [".COM", ".EXE", ".BAT", ".CMD"];
    assert_eq!(Lookup::from_env(&[], None, None, true).extensions, default);
    assert_eq!(
        Lookup::from_env(&[], None, Some(".EXE;;.CMD".into()), true).extensions,
        [".EXE", ".CMD"]
    );
    for empty in ["", ";", ";;"] {
        assert_eq!(
            Lookup::from_env(&pairs(&[("PATHEXT", empty)]), None, None, true).extensions,
            default
        );
        assert_eq!(
            Lookup::from_env(&[], None, Some(empty.into()), true).extensions,
            default
        );
    }
    let base = pairs(&[("PathExt", ".A"), ("PATHEXT", ".B;.C")]);
    assert_eq!(
        Lookup::from_env(&base, None, Some(".EXE".into()), true).extensions,
        [".B", ".C"]
    );
    assert!(
        Lookup::from_env(&base, None, Some(".EXE".into()), false)
            .extensions
            .is_empty()
    );
}

#[test]
fn the_windows_lookup_prefers_the_cmd_shim_over_the_sh_script() {
    let dir = TempDir::new().unwrap();
    touch(&dir, &["npx", "npx.cmd", "npx.ps1"]);
    let config = config_of(&[("claude", entry("claude-agent-acp", &["-v"]))]);

    let resolved = resolve_plain(&config, &windows(&dir), &home(), &[], None).unwrap();

    let acp = &resolved.backends[&id("claude")];
    assert_eq!(acp.command, dir.path().join("npx.cmd"));
    assert_eq!(
        acp.args,
        [
            "--yes",
            "@agentclientprotocol/claude-agent-acp@0.108.0",
            "-v"
        ]
    );
}

#[test]
fn without_npx_the_windows_fallback_stays_the_bare_name() {
    let dir = TempDir::new().unwrap();
    touch(&dir, &["npx"]);
    let config = config_of(&[("claude", entry("claude-agent-acp", &[]))]);

    let resolved = resolve_plain(&config, &windows(&dir), &home(), &[], None).unwrap();

    assert_eq!(
        resolved.backends[&id("claude")].command,
        PathBuf::from("npx")
    );
}

#[test]
fn a_bare_command_found_through_pathext_runs_by_its_full_path() {
    let dir = TempDir::new().unwrap();
    touch(&dir, &["claude-agent-acp", "claude-agent-acp.cmd"]);
    let config = config_of(&[("claude", entry("claude-agent-acp", &[]))]);

    let resolved = resolve_plain(&config, &windows(&dir), &home(), &[], None).unwrap();

    let acp = &resolved.backends[&id("claude")];
    assert_eq!(acp.command, dir.path().join("claude-agent-acp.cmd"));
    assert!(acp.args.is_empty());
}

#[test]
fn the_extensions_are_tried_in_order_and_in_lowercase() {
    let dir = TempDir::new().unwrap();
    touch(&dir, &["tool.bat", "tool.exe"]);
    let lookup = Lookup {
        folders: vec![PathBuf::from("/nowhere"), dir.path().to_path_buf()],
        extensions: vec![".BAT".to_owned(), ".EXE".to_owned()],
        full_path: true,
    };

    assert_eq!(
        lookup.find(Path::new("tool")),
        Some(dir.path().join("tool.bat"))
    );
    assert_eq!(lookup.find(Path::new("other")), None);
}

#[test]
fn the_lowercase_extension_is_tried_before_the_listed_one() {
    let dir = TempDir::new().unwrap();
    // On a file system that ignores case both names are the one file, and the found path
    // is spelled as it was tried; here they are two, so the order shows.
    touch(&dir, &["tool.CMD", "tool.cmd"]);
    let lookup = Lookup {
        folders: vec![dir.path().to_path_buf()],
        extensions: vec![".CMD".to_owned()],
        full_path: true,
    };

    assert_eq!(
        lookup.find(Path::new("tool")),
        Some(dir.path().join("tool.cmd"))
    );
}

#[test]
fn a_command_with_an_extension_is_tried_as_written() {
    let dir = TempDir::new().unwrap();
    touch(&dir, &["tool.exe"]);
    let config = config_of(&[("a", entry("tool.exe", &[]))]);

    let resolved = resolve_plain(&config, &windows(&dir), &home(), &[], None).unwrap();

    assert_eq!(
        resolved.backends[&id("a")].command,
        dir.path().join("tool.exe")
    );
}

#[test]
fn an_extensionless_file_alone_is_not_found_on_windows() {
    let dir = TempDir::new().unwrap();
    touch(&dir, &["mytool"]);
    let config = config_of(&[("a", entry("mytool", &[]))]);

    let error = resolve_plain(&config, &windows(&dir), &home(), &[], None).unwrap_err();

    assert!(matches!(error, ConfigError::CommandNotFound { .. }));
}

#[test]
fn resolve_each_reports_a_missing_command_per_backend() {
    let mut config = config_of(&[
        ("found", entry("/opt/agent", &[])),
        ("missing", entry("my-adapter", &[])),
    ]);
    config.default = Some(id("found"));
    let base = pairs(&[("X", "y")]);

    let each = resolve_each(
        &config,
        &Lookup::default(),
        &home(),
        &base,
        None,
        &Launch::default(),
    );

    assert_eq!(each.len(), 2);
    let found = each[&id("found")].as_ref().unwrap();
    assert_eq!(found.command, PathBuf::from("/opt/agent"));
    assert_eq!(found.env, base);
    assert!(matches!(
        &each[&id("missing")],
        Err(ConfigError::CommandNotFound { backend, command })
            if *backend == id("missing") && *command == Path::new("my-adapter")
    ));
}

#[test]
fn the_deferred_resolution_names_a_missing_command_and_keeps_the_rest() {
    let mut config = config_of(&[
        ("found", entry("/opt/agent", &[])),
        ("missing", entry("partitur-no-such-adapter", &[])),
    ]);
    config.default = Some(id("found"));
    let base = pairs(&[("PATH", "/nowhere"), ("X", "y")]);

    let resolution = resolution(&config, &base, None, &Launch::default());

    assert_eq!(resolution.len(), 2);
    assert_eq!(
        resolution[&id("found")].as_ref().unwrap().env,
        pairs(&[("PATH", "/nowhere"), ("X", "y")])
    );
    assert_eq!(
        resolution[&id("missing")].as_ref().unwrap_err(),
        "the adapter command partitur-no-such-adapter of backend missing is not on PATH"
    );
}

#[test]
fn the_deferred_resolution_looks_commands_up_on_the_base_path() {
    let dir = TempDir::new().unwrap();
    // Windows finds a bare name only with an extension, and runs it by its full path.
    let file = if cfg!(windows) {
        "my-adapter.cmd"
    } else {
        "my-adapter"
    };
    fs::write(dir.path().join(file), "").unwrap();
    let mut adapter = entry("my-adapter", &[]);
    adapter.env = BTreeMap::from([("A_HOME".to_owned(), "~".to_owned())]);
    let config = config_of(&[("a", adapter)]);
    let path = dir.path().display().to_string();
    let base = pairs(&[("PATH", &path)]);

    let resolution = resolution(&config, &base, None, &Launch::default());

    let acp = resolution[&id("a")].as_ref().unwrap();
    let command = if cfg!(windows) {
        dir.path().join(file)
    } else {
        PathBuf::from(file)
    };
    assert_eq!(acp.command, command);
    // `~` is the user's home folder, as on a start from a terminal.
    let home = BaseDirs::new().unwrap().home_dir().display().to_string();
    assert_eq!(acp.env, pairs(&[("PATH", &path), ("A_HOME", &home)]));
}

#[test]
fn asking_the_shell_always_defers_the_resolution() {
    let missing = config_of(&[("a", entry("partitur-no-such-adapter", &[]))]);
    let known = ProcessEnv::ready(Vec::new());

    // Even with the environment known already, a start that asks the shell resolves
    // nothing yet, so a missing command cannot stop it.
    assert!(
        resolve_at_start(&missing, true, &known, None, &Launch::default())
            .unwrap()
            .is_none()
    );
    let (pending, _setter) = ProcessEnv::pending();
    assert!(
        resolve_at_start(&missing, true, &pending, None, &Launch::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_resolution_is_deferred_when_the_shell_is_asked_or_on_windows() {
    assert!(!defers_resolution(false, false));
    assert!(defers_resolution(true, false));
    assert!(defers_resolution(false, true));
    assert!(defers_resolution(true, true));
}

#[test]
fn without_the_shell_the_backends_resolve_at_the_start() {
    let base = ProcessEnv::ready(pairs(&[("X", "y")]));
    let found = config_of(&[("a", entry("/opt/agent", &[]))]);

    let resolved = resolve_at_start(&found, false, &base, None, &Launch::default())
        .unwrap()
        .unwrap();
    assert_eq!(resolved.backends[&id("a")].env, pairs(&[("X", "y")]));

    let missing = config_of(&[("a", entry("partitur-no-such-adapter", &[]))]);
    assert!(matches!(
        resolve_at_start(&missing, false, &base, None, &Launch::default()),
        Err(ConfigError::CommandNotFound { .. })
    ));
}

const ENTRY_FILE: &str = "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js";

/// Writes the empty files `names` under `root`, with their folders.
fn lay(root: &Path, names: &[&str]) {
    for name in names {
        let file = root.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "").unwrap();
    }
}

/// The default adapter command as the only backend.
fn default_adapter(args: &[&str]) -> Config {
    config_of(&[("main", entry(ADAPTER_COMMAND, args))])
}

/// A bundle at `root` with the entry file, and `node` as its own Node.js when given.
fn bundle_in(root: &Path, node: Option<&str>) -> BundledAdapter {
    lay(root, &[ENTRY_FILE]);
    let node = node.map(|name| {
        lay(root, &[name]);
        root.join(name)
    });
    BundledAdapter {
        entry: root.join(ENTRY_FILE),
        node,
    }
}

/// Asserts that both resolve `main` to the same command, arguments and environment.
fn assert_same(a: &Resolved, b: &Resolved) {
    let (a, b) = (&a.backends[&id("main")], &b.backends[&id("main")]);
    assert_eq!(a.command, b.command);
    assert_eq!(a.args, b.args);
    assert_eq!(a.env, b.env);
}

#[test]
fn bundled_adapter_found_beside_linux_exe() {
    let tmp = TempDir::new().unwrap();
    let adapter = "usr/lib/partitur/adapter";
    lay(
        tmp.path(),
        &[&format!("{adapter}/{ENTRY_FILE}"), "usr/bin/partitur"],
    );

    let found = bundled_adapter(&tmp.path().join("usr/bin/partitur"), false, false).unwrap();

    assert!(
        found
            .entry
            .ends_with(format!("usr/lib/partitur/adapter/{ENTRY_FILE}"))
    );
    assert!(found.entry.is_file());
    assert_eq!(found.node, None);

    lay(tmp.path(), &[&format!("{adapter}/node/bin/node")]);
    let found = bundled_adapter(&tmp.path().join("usr/bin/partitur"), false, false).unwrap();
    assert!(found.node.unwrap().ends_with("adapter/node/bin/node"));
}

#[test]
fn bundled_adapter_found_in_macos_resources() {
    let tmp = TempDir::new().unwrap();
    lay(
        tmp.path(),
        &[
            &format!("Contents/Resources/adapter/{ENTRY_FILE}"),
            "Contents/MacOS/partitur",
            "Contents/Resources/adapter/node/bin/node",
        ],
    );
    let exe = tmp.path().join("Contents/MacOS/partitur");

    let found = bundled_adapter(&exe, false, true).unwrap();

    assert!(
        found
            .entry
            .ends_with(format!("Contents/Resources/adapter/{ENTRY_FILE}"))
    );
    assert!(
        found
            .node
            .unwrap()
            .ends_with("Resources/adapter/node/bin/node")
    );
    // The same layout is no Linux bundle.
    assert_eq!(bundled_adapter(&exe, false, false), None);
}

#[test]
fn bundled_adapter_found_beside_windows_exe() {
    let tmp = TempDir::new().unwrap();
    lay(
        tmp.path(),
        &[
            &format!("adapter/{ENTRY_FILE}"),
            "adapter/node/node.exe",
            "adapter/node/bin/node",
        ],
    );
    let exe = tmp.path().join("partitur.exe");

    let found = bundled_adapter(&exe, true, false).unwrap();

    assert!(found.entry.ends_with(format!("adapter/{ENTRY_FILE}")));
    assert!(found.node.unwrap().ends_with("adapter/node/node.exe"));
    assert_eq!(bundled_adapter(&exe, false, false), None);
}

#[test]
fn bundled_adapter_missing_entry_is_none() {
    let tmp = TempDir::new().unwrap();
    lay(
        tmp.path(),
        &["usr/lib/partitur/adapter/node/bin/node", "usr/bin/partitur"],
    );

    assert_eq!(
        bundled_adapter(&tmp.path().join("usr/bin/partitur"), false, false),
        None
    );
    assert_eq!(bundled_adapter(Path::new("/"), false, false), None);
}

#[test]
fn bundled_adapter_wins_over_path_and_npx() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    touch(&bin, &["claude", "claude-agent-acp", "node"]);
    let bundle = bundle_in(tmp.path(), Some("own-node"));

    let resolved = resolve_plain(
        &default_adapter(&["-v"]),
        &plain(&[bin.path().to_path_buf()]),
        &home(),
        &pairs(&[("X", "y")]),
        Some(&bundle),
    )
    .unwrap();

    let acp = &resolved.backends[&id("main")];
    assert_eq!(acp.command, tmp.path().join("own-node"));
    assert_eq!(
        acp.args,
        [bundle.entry.display().to_string(), "-v".to_owned()]
    );
    assert_eq!(
        acp.env,
        pairs(&[
            ("X", "y"),
            (
                "CLAUDE_CODE_EXECUTABLE",
                &bin.path().join("claude").display().to_string()
            ),
        ])
    );
}

#[test]
fn bundled_adapter_keeps_the_configured_env_before_the_claude_variable() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    touch(&bin, &["claude"]);
    let bundle = bundle_in(tmp.path(), Some("own-node"));
    let mut configured = entry(ADAPTER_COMMAND, &[]);
    configured.env.insert("A".to_owned(), "b".to_owned());

    let resolved = resolve_plain(
        &config_of(&[("main", configured)]),
        &plain(&[bin.path().to_path_buf()]),
        &home(),
        &pairs(&[("X", "y")]),
        Some(&bundle),
    )
    .unwrap();

    let env = &resolved.backends[&id("main")].env;
    assert_eq!(env[..2], pairs(&[("X", "y"), ("A", "b")]));
    assert_eq!(env[2].0, "CLAUDE_CODE_EXECUTABLE");
}

#[test]
fn bundled_adapter_uses_system_node_without_bundled_node() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    touch(&bin, &["claude", "node"]);
    let bundle = bundle_in(tmp.path(), None);

    let resolved = resolve_plain(
        &default_adapter(&[]),
        &plain(&[bin.path().to_path_buf()]),
        &home(),
        &[],
        Some(&bundle),
    )
    .unwrap();

    let acp = &resolved.backends[&id("main")];
    assert_eq!(acp.command, bin.path().join("node"));
    assert_eq!(acp.args, [bundle.entry.display().to_string()]);
}

#[test]
fn bundled_adapter_skipped_without_claude() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    touch(&bin, &["node"]);
    let bundle = bundle_in(tmp.path(), Some("own-node"));
    let lookup = plain(&[bin.path().to_path_buf()]);
    let config = default_adapter(&["-v"]);

    let with = resolve_plain(&config, &lookup, &home(), &[], Some(&bundle)).unwrap();
    let without = resolve_plain(&config, &lookup, &home(), &[], None).unwrap();

    assert_same(&with, &without);
    assert_eq!(with.backends[&id("main")].command, PathBuf::from("npx"));
}

#[test]
fn bundled_adapter_skipped_without_any_node() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    touch(&bin, &["claude"]);
    let bundle = bundle_in(tmp.path(), None);
    let lookup = plain(&[bin.path().to_path_buf()]);
    let config = default_adapter(&[]);

    let with = resolve_plain(&config, &lookup, &home(), &[], Some(&bundle)).unwrap();
    let without = resolve_plain(&config, &lookup, &home(), &[], None).unwrap();

    assert_same(&with, &without);
}

#[test]
fn custom_command_ignores_bundle() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    touch(&bin, &["claude", "node"]);
    let bundle = bundle_in(tmp.path(), Some("own-node"));

    let resolved = resolve_plain(
        &config_of(&[("main", entry("/opt/my-acp", &["-x"]))]),
        &plain(&[bin.path().to_path_buf()]),
        &home(),
        &[],
        Some(&bundle),
    )
    .unwrap();

    let acp = &resolved.backends[&id("main")];
    assert_eq!(acp.command, PathBuf::from("/opt/my-acp"));
    assert_eq!(acp.args, ["-x"]);
    assert!(acp.env.is_empty());
}

#[test]
fn the_deferred_resolution_uses_the_bundle_too() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    // This goes through the process's own lookup, which on Windows only finds a name
    // with one of `PATHEXT`'s extensions.
    touch(
        &bin,
        &[if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        }],
    );
    let bundle = bundle_in(tmp.path(), Some("own-node"));
    let path = bin.path().display().to_string();
    let base = pairs(&[("PATH", &path)]);

    let deferred = resolution(
        &default_adapter(&[]),
        &base,
        Some(&bundle),
        &Launch::default(),
    );

    let acp = deferred[&id("main")].as_ref().unwrap();
    assert_eq!(acp.command, tmp.path().join("own-node"));
    assert_eq!(acp.args, [bundle.entry.display().to_string()]);

    let start = ProcessEnv::ready(base);
    let resolved = resolve_at_start(
        &default_adapter(&[]),
        false,
        &start,
        Some(&bundle),
        &Launch::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        resolved.backends[&id("main")].command,
        tmp.path().join("own-node")
    );
}

#[test]
fn listing_names_every_backend_and_the_default() {
    let (_dir, path) = config_file(TWO_BACKENDS);
    let config = Config::load(&path).unwrap();

    let (infos, default) = listing(&config).unwrap();

    assert_eq!(
        infos,
        [
            BackendInfo {
                id: id("claude-main"),
                name: "claude-main".into(),
                command: "claude-agent-acp".into(),
            },
            BackendInfo {
                id: id("claude-work"),
                name: "claude-work".into(),
                command: "/opt/acp/bin/agent".into(),
            },
        ]
    );
    assert_eq!(default, id("claude-main"));
    assert!(matches!(
        listing(&config_of(&[])),
        Err(ConfigError::NoDefault)
    ));
}

#[test]
fn resolving_against_this_process_keeps_a_command_with_a_folder() {
    let resolved = resolve_backends(
        &config_of(&[("a", entry("/opt/missing/agent", &[]))]),
        &[],
        None,
        &Launch::default(),
    )
    .unwrap();

    assert_eq!(
        resolved.backends[&id("a")].command,
        PathBuf::from("/opt/missing/agent")
    );
    assert_eq!(resolved.default, id("a"));
}

/// Everything `acp` starts its adapter with.
type Started = (
    PathBuf,
    Vec<String>,
    Vec<(String, String)>,
    ThinkingDisplay,
    Option<PathBuf>,
);

fn started(acp: &AcpConfig) -> Started {
    (
        acp.command.clone(),
        acp.args.clone(),
        acp.env.clone(),
        acp.thinking_display,
        acp.instructions_dir.clone(),
    )
}

/// Every backend of `backends` with what it starts, in id order.
fn started_in_order(backends: &HashMap<BackendId, AcpConfig>) -> Vec<(BackendId, Started)> {
    let mut started: Vec<_> = backends
        .iter()
        .map(|(id, acp)| (id.clone(), self::started(acp)))
        .collect();
    started.sort_by(|a, b| a.0.cmp(&b.0));
    started
}

/// The configurations the tests above resolve, each with a missing command or a default
/// they lack in some of them.
fn config_fixtures() -> Vec<Config> {
    let two = parse(TWO_BACKENDS);
    let copilot = parse(
        r#"
default = "claude-main"

[backends.claude-main]
command = "claude-agent-acp"

[backends.copilot-main]
command = "copilot"
args = ["--acp"]
env = { COPILOT_HOME = "~/.copilot-main" }
"#,
    );
    let mut found_and_missing = config_of(&[
        ("found", entry("/opt/agent", &[])),
        ("missing", entry("partitur-no-such-adapter", &[])),
    ]);
    found_and_missing.default = Some(id("found"));
    let mut two_missing = config_of(&[
        ("a", entry("partitur-no-such-a", &[])),
        ("b", entry("partitur-no-such-b", &[])),
    ]);
    two_missing.default = Some(id("b"));
    let mut unknown_default = config_of(&[("a", entry("/opt/a", &[]))]);
    unknown_default.default = Some(id("gone"));
    vec![
        two,
        copilot,
        found_and_missing,
        two_missing,
        unknown_default,
        config_of(&[]),
        config_of(&[("a", entry("/opt/a", &[])), ("b", entry("/opt/b", &[]))]),
        config_of(&[("a", entry("/opt/missing/agent", &["-x"]))]),
        default_adapter(&["-v"]),
        Config::builtin(),
    ]
}

#[test]
fn eager_and_deferred_resolution_agree() {
    let bin = TempDir::new().unwrap();
    touch(
        &bin,
        &if cfg!(windows) {
            ["copilot.exe", "claude.exe"]
        } else {
            ["copilot", "claude"]
        },
    );
    let bundle_root = TempDir::new().unwrap();
    let bundle = bundle_in(bundle_root.path(), Some("own-node"));
    let path = bin.path().display().to_string();
    let base = pairs(&[("PATH", &path), ("X", "y")]);

    let launches = [
        Launch::default(),
        Launch {
            launcher: Some(PathBuf::from("/l/partitur.exe")),
            instructions: Some(PathBuf::from("/data/agent-instructions")),
        },
    ];
    for (bundle, launch) in [None, Some(&bundle)]
        .into_iter()
        .flat_map(|bundle| launches.iter().map(move |launch| (bundle, launch)))
    {
        for config in config_fixtures() {
            let eager = resolve_backends(&config, &base, bundle, launch)
                .map(|resolved| started_in_order(&resolved.backends))
                .map_err(|error| error.to_string());
            let mut deferred: Vec<_> = resolution(&config, &base, bundle, launch)
                .into_iter()
                .collect();
            deferred.sort_by(|a, b| a.0.cmp(&b.0));
            let deferred = listing(&config)
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    deferred
                        .into_iter()
                        .map(|(id, result)| result.map(|acp| (id, started(&acp))))
                        .collect::<Result<Vec<_>, _>>()
                });

            assert_eq!(eager, deferred, "{config:?} {launch:?}");
        }
    }
}
