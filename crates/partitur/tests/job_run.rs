//! The `--job-run` launcher, run as the built binary: it hands its standard streams to the
//! program it starts and exits with the program's code, also on Windows, where it holds the
//! job it put itself in until it exits; and there killing it ends every process the program
//! started.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// The launcher over `program` and `args`.
fn launcher(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_partitur"));
    command.args(["--job-run", "--", program]).args(args);
    command
}

/// A program that reads one line from stdin and writes it back to stdout.
fn echo_line() -> Command {
    if cfg!(windows) {
        launcher(
            "powershell",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::Out.WriteLine([Console]::In.ReadLine())",
            ],
        )
    } else {
        launcher("sh", &["-c", "read l; echo \"$l\""])
    }
}

#[test]
fn the_launcher_hands_its_stdio_to_the_program() {
    let mut child = echo_line()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"through the launcher\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();

    assert_eq!(line.trim_end(), "through the launcher");
    assert!(child.wait().unwrap().success());
}

/// A program that exits with `code`.
fn exit_with(code: u8) -> Command {
    let script = format!("exit {code}");
    if cfg!(windows) {
        launcher("cmd", &["/c", &script])
    } else {
        launcher("sh", &["-c", &script])
    }
}

#[test]
fn the_launcher_exits_with_the_program_s_code() {
    assert_eq!(exit_with(3).status().unwrap().code(), Some(3));
    assert_eq!(exit_with(0).status().unwrap().code(), Some(0));
}

#[test]
fn the_launcher_exits_with_127_when_the_program_cannot_start() {
    let status = launcher("/nonexistent/program", &[]).status().unwrap();
    assert_eq!(status.code(), Some(127));
}

#[cfg(windows)]
#[test]
fn killing_the_launcher_ends_the_whole_tree() {
    use std::time::{Duration, Instant};

    let dir = tempfile::TempDir::new().unwrap();
    let pid_file = dir.path().join("grandchild.pid");
    let script = format!(
        "$p = Start-Process ping -ArgumentList '-n','120','127.0.0.1' -PassThru -WindowStyle Hidden; \
         Set-Content -LiteralPath '{}' -Value $p.Id; \
         Start-Sleep -Seconds 120",
        pid_file.display()
    );
    let mut child = launcher(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
    .spawn()
    .unwrap();

    let started = Instant::now();
    let pid = loop {
        let written = std::fs::read_to_string(&pid_file).ok();
        if let Some(pid) = written.and_then(|text| text.trim().parse::<u32>().ok()) {
            break pid;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the grandchild never wrote its pid"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(running(pid), "the grandchild {pid} is not running");

    child.kill().unwrap();
    child.wait().unwrap();

    let killed = Instant::now();
    while running(pid) {
        assert!(
            killed.elapsed() < Duration::from_secs(10),
            "the grandchild {pid} outlived the launcher"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whether a process with `pid` is running, as `tasklist` sees it.
#[cfg(windows)]
fn running(pid: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    let pid = format!("\"{pid}\"");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.split(',').nth(1) == Some(pid.as_str()))
}
