//! The widget engine helper: the process in which CEF renders Partitur's MCP App widgets.
//!
//! Partitur starts it and drives it over its standard input and output in the
//! `partitur_widgets::protocol`; its standard error carries its log, which Partitur hands on to
//! its own. It first hands over to CEF when it was started as one of CEF's own helper processes,
//! which run this same executable again with a `--type=` argument. Otherwise it writes its log to
//! standard error, without colours or times (Partitur's log adds its own), at the info level, and
//! runs the engine (`partitur_cef::serve`) on its main thread until Partitur shuts it down or
//! its input ends. CEF runs in the OS process sandbox unless it was given
//! [`NO_SANDBOX`], and probes the sandbox first unless it was given [`NO_PROBE`] too. The exit
//! code is the engine's: `0`, `1` when CEF did not start, or
//! `partitur_widgets::protocol::PROBE_FAILED` when the probe failed.
//!
//! On Windows it is a GUI program, so it never opens a console window; its piped standard
//! streams work all the same.

#![windows_subsystem = "windows"]

use std::process::ExitCode;

use partitur_widgets::protocol::{NO_PROBE, NO_SANDBOX};

fn main() -> ExitCode {
    if let Some(code) = partitur_cef::subprocess() {
        return exit_code(code);
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .without_time()
        .with_ansi(false)
        .init();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let code = partitur_cef::serve(
        !args.iter().any(|arg| arg == NO_SANDBOX),
        !args.iter().any(|arg| arg == NO_PROBE),
    );
    if code != 0 {
        tracing::info!("the widget engine ends with exit code {code}");
    }
    exit_code(code)
}

/// `code` as the process's exit code; one that does not fit a byte becomes 1.
fn exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
