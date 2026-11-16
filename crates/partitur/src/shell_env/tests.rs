#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::*;

/// Held by every test that writes an executable or starts a process. A process forked while
/// another test still has its freshly written script open for writing keeps that file
/// descriptor until it execs, and running the script then fails with "text file busy".
/// Any test in this crate that writes or spawns executables must take it.
static PROCESSES: Mutex<()> = Mutex::new(());

fn processes() -> MutexGuard<'static, ()> {
    PROCESSES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn only_a_macos_start_without_a_terminal_wants_the_shell() {
    assert!(wanted(true, false));
    assert!(!wanted(true, true));
    assert!(!wanted(false, false));
    assert!(!wanted(false, true));
}

#[test]
fn print_env_writes_one_nul_terminated_pair_per_variable() {
    let mut out = Vec::new();
    print_env(&mut out, pairs(&[("A", "1"), ("B", "")])).unwrap();
    assert_eq!(out, b"A=1\0B=\0");
}

#[test]
fn print_env_then_parse_round_trips() {
    let vars = pairs(&[
        ("PATH", "/usr/bin:/bin"),
        ("MULTI", "one\ntwo"),
        ("EQUALS", "a=b=c"),
    ]);
    let mut out = Vec::new();
    out.extend_from_slice(START_MARKER.as_bytes());
    print_env(&mut out, vars.clone()).unwrap();
    out.extend_from_slice(END_MARKER.as_bytes());
    assert_eq!(parse(&out).unwrap(), vars);
}

#[test]
fn text_around_the_markers_is_ignored() {
    assert_eq!(
        parse(b"banner\n__PARTITUR_ENV_START__A=1\0__PARTITUR_ENV_END__bye").unwrap(),
        pairs(&[("A", "1")])
    );
}

#[test]
fn an_end_marker_before_the_start_marker_does_not_end_it() {
    assert_eq!(
        parse(b"__PARTITUR_ENV_END____PARTITUR_ENV_START__A=1\0__PARTITUR_ENV_END__").unwrap(),
        pairs(&[("A", "1")])
    );
}

#[test]
fn the_dropped_names_are_dropped() {
    let output = b"__PARTITUR_ENV_START__PWD=/p\0OLDPWD=/o\0SHLVL=2\0_=/bin/env\0\
PARTITUR_EXE=/x\0KEPT=1\0__PARTITUR_ENV_END__";
    assert_eq!(parse(output).unwrap(), pairs(&[("KEPT", "1")]));
}

#[test]
fn empty_entries_entries_without_a_value_and_non_utf8_entries_are_skipped() {
    let output = b"__PARTITUR_ENV_START__\0A=1\0\0NOEQUALS\0B=\xff\0C=3\0__PARTITUR_ENV_END__";
    assert_eq!(parse(output).unwrap(), pairs(&[("A", "1"), ("C", "3")]));
}

#[test]
fn no_markers_is_an_error() {
    assert!(matches!(parse(b"A=1\0"), Err(ShellEnvError::NoMarkers)));
    assert!(matches!(
        parse(b"__PARTITUR_ENV_START__A=1\0"),
        Err(ShellEnvError::NoMarkers)
    ));
    assert!(matches!(
        parse(b"A=1\0__PARTITUR_ENV_END__"),
        Err(ShellEnvError::NoMarkers)
    ));
}

#[test]
fn fallback_appends_only_existing_missing_folders() {
    let home = Path::new("/home/u");
    let exists = |folder: &Path| folder != Path::new("/opt/homebrew/bin");
    assert_eq!(
        fallback(Some("/usr/local/bin:/usr/bin"), Some(home), exists),
        pairs(&[("PATH", "/usr/local/bin:/usr/bin:/home/u/.local/bin")])
    );
    assert_eq!(
        fallback(Some("/usr/bin"), Some(home), |_| false),
        pairs(&[("PATH", "/usr/bin")])
    );
}

#[test]
fn fallback_without_a_path_starts_from_the_system_folders() {
    assert_eq!(
        fallback(None, Some(Path::new("/home/u")), |_| true),
        pairs(&[(
            "PATH",
            "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin:/home/u/.local/bin"
        )])
    );
}

#[test]
fn fallback_without_a_home_leaves_out_the_folder_under_it() {
    assert_eq!(
        fallback(Some("/usr/bin"), None, |_| true),
        pairs(&[("PATH", "/usr/bin:/opt/homebrew/bin:/usr/local/bin")])
    );
}

#[test]
fn fallback_on_an_empty_path_adds_no_empty_entry() {
    assert_eq!(
        fallback(Some(""), Some(Path::new("/home/u")), |folder| {
            folder == Path::new("/usr/local/bin")
        }),
        pairs(&[("PATH", "/usr/local/bin")])
    );
}

#[test]
fn the_errors_say_what_went_wrong() {
    assert_eq!(
        ShellEnvError::Spawn(std::io::Error::other("boom")).to_string(),
        "the login shell could not be started: boom"
    );
    assert_eq!(
        ShellEnvError::Timeout.to_string(),
        "the login shell did not answer in time"
    );
    assert_eq!(
        ShellEnvError::NoMarkers.to_string(),
        "the login shell printed no environment"
    );
}

#[test]
fn a_shell_that_cannot_be_started_is_a_spawn_error() {
    let _processes = processes();
    let result = from_shell(Path::new("/no/such/shell"), Path::new("/x"), LIMIT);
    assert!(matches!(result, Err(ShellEnvError::Spawn(_))), "{result:?}");
}

#[test]
fn start_without_the_shell_is_ready_at_once_with_nothing() {
    let env = start(false);
    assert_eq!(env.get().as_deref(), Some(&[][..]));
}

#[test]
fn the_login_shell_is_named() {
    assert!(!login_shell().as_os_str().is_empty());
}

#[cfg(unix)]
mod processes {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    /// Writes an executable `/bin/sh` script named `name` running `body`.
    fn script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_shell_printing_markers_around_the_environment_gives_its_variables() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let shell = script(
            &dir,
            "shell",
            r"printf 'banner\n__PARTITUR_ENV_START__A=1\0B=two\0PWD=/p\0__PARTITUR_ENV_END__bye'",
        );
        assert_eq!(
            from_shell(&shell, Path::new("/x"), LIMIT).unwrap(),
            pairs(&[("A", "1"), ("B", "two")])
        );
    }

    #[test]
    fn a_shell_printing_junk_only_has_no_markers() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let shell = script(&dir, "shell", "echo junk");
        assert!(matches!(
            from_shell(&shell, Path::new("/x"), LIMIT),
            Err(ShellEnvError::NoMarkers)
        ));
    }

    #[test]
    fn a_shell_that_does_not_answer_is_stopped_at_the_limit() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let shell = script(&dir, "shell", "exec sleep 30");
        let limit = Duration::from_millis(300);
        let started = Instant::now();
        let result = from_shell(&shell, Path::new("/x"), limit);
        let took = started.elapsed();
        assert!(matches!(result, Err(ShellEnvError::Timeout)), "{result:?}");
        assert!(took >= limit, "{took:?}");
        assert!(took < limit + Duration::from_secs(1), "{took:?}");
    }

    #[test]
    fn a_shell_that_hangs_while_output_keeps_coming_is_stopped_at_the_limit() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        // A startup file that hangs while a background process it started keeps writing to
        // stdout. The writer ends at the first write once the lookup has closed the output, and
        // the shell itself is the `sleep`, which is killed.
        let shell = script(&dir, "shell", "while echo x; do :; done &\nexec sleep 30");
        let limit = Duration::from_millis(300);
        let started = Instant::now();
        let result = from_shell(&shell, Path::new("/x"), limit);
        let took = started.elapsed();
        assert!(matches!(result, Err(ShellEnvError::Timeout)), "{result:?}");
        assert!(took < limit + Duration::from_secs(1), "{took:?}");
    }

    #[test]
    fn a_background_process_holding_stdout_does_not_hold_up_the_lookup() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        // Like an rc file that starts an agent with `&`: the background process inherits
        // stdout and keeps it open long after the shell has printed the end marker.
        let shell = script(
            &dir,
            "shell",
            r"sleep 5 &
printf '__PARTITUR_ENV_START__A=1\0__PARTITUR_ENV_END__'",
        );
        let started = Instant::now();
        let result = from_shell(&shell, Path::new("/x"), Duration::from_secs(5));
        let took = started.elapsed();
        assert_eq!(result.unwrap(), pairs(&[("A", "1")]));
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[test]
    fn the_command_works_in_a_real_posix_shell() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let exe = script(
            &dir,
            "partitur",
            r#"if [ "$1" = --print-env ]; then printf 'A=1\0'; fi"#,
        );
        assert_eq!(
            from_shell(Path::new("/bin/sh"), &exe, LIMIT).unwrap(),
            pairs(&[("A", "1")])
        );
    }

    #[test]
    fn the_lookup_takes_the_shells_environment_when_it_answers() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let exe = script(
            &dir,
            "partitur",
            r#"if [ "$1" = --print-env ]; then printf 'A=1\0'; fi"#,
        );
        assert_eq!(
            lookup(
                Path::new("/bin/sh"),
                Ok(exe),
                Some("/usr/bin"),
                Some(Path::new("/home/u")),
                LIMIT
            ),
            pairs(&[("A", "1")])
        );
    }

    #[test]
    fn the_lookup_falls_back_to_the_folders_when_the_shell_fails() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        let shell = script(&dir, "shell", "echo junk");
        // Both system folders are on the path already, so only the one under home is added,
        // whichever of them this machine has.
        let path = "/opt/homebrew/bin:/usr/local/bin";
        assert_eq!(
            lookup(
                &shell,
                Ok(PathBuf::from("/x")),
                Some(path),
                Some(home),
                LIMIT
            ),
            vec![(
                "PATH".to_owned(),
                format!("{path}:{}", home.join(".local/bin").display())
            )]
        );
    }

    #[test]
    fn start_with_the_shell_sets_its_environment_in_the_background() {
        let _processes = processes();
        let dir = TempDir::new().unwrap();
        let shell = script(
            &dir,
            "shell",
            r"printf '__PARTITUR_ENV_START__A=1\0B=two\0__PARTITUR_ENV_END__'",
        );
        let env = start_with(true, shell, LIMIT);
        assert_eq!(*env.wait(), *pairs(&[("A", "1"), ("B", "two")]));
    }

    #[test]
    fn the_lookup_falls_back_when_the_executable_is_unknown() {
        let path = "/opt/homebrew/bin:/usr/local/bin";
        assert_eq!(
            lookup(
                Path::new("/bin/sh"),
                Err(std::io::Error::other("no exe")),
                Some(path),
                Some(Path::new("/no/home")),
                LIMIT,
            ),
            pairs(&[("PATH", path)])
        );
    }
}
