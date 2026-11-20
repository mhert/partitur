//! Renders real widget pages in CEF and checks what comes back through the widget host port.
//!
//! The binary has its own `main` because CEF starts its helper processes by running this same
//! binary again; `partitur_cef::subprocess` hands those over to CEF before anything else runs.
//! CEF can be started only once per process, so one host serves every test, each on a view of its
//! own. The tests need a display: without `DISPLAY` or `WAYLAND_DISPLAY` they are skipped with a
//! printed reason, and CI runs them under `xvfb-run`.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use partitur_cef::CefWidgetHost;
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{ViewId, WidgetHost, WidgetHostEvent, WidgetPage};

/// How long a test waits for what it expects before it fails.
const PATIENCE: Duration = Duration::from_secs(20);

/// The policy the test pages run under: inline script and style, nothing else.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'";

fn main() -> ExitCode {
    if let Some(code) = partitur_cef::subprocess() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1)); // coverage: runs in a CEF helper process
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        println!("skipping the CEF render tests: neither DISPLAY nor WAYLAND_DISPLAY is set");
        return ExitCode::SUCCESS;
    }
    let (host, mut events) = match partitur_cef::start() {
        Ok(started) => started,
        Err(error) => {
            println!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let tests: [(&str, Test); 2] = [
        ("widget_posts_and_paints", widget_posts_and_paints),
        (
            "widget_cannot_fetch_file_urls",
            widget_cannot_fetch_file_urls,
        ),
    ];
    let mut failed = 0;
    for (name, test) in tests {
        match test(&host, &mut events) {
            Ok(()) => println!("test {name} ... ok"),
            Err(error) => {
                println!("test {name} ... FAILED: {error}");
                failed += 1;
            }
        }
    }
    drop(host);
    println!(
        "test result: {} passed; {failed} failed",
        tests.len() - failed
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

type Test = fn(&CefWidgetHost, &mut UnboundedReceiver<WidgetHostEvent>) -> Result<(), String>;

fn page(html: &str, csp: &str) -> WidgetPage {
    WidgetPage {
        html: html.into(),
        csp: csp.into(),
        theme: WidgetTheme::Light,
    }
}

/// Waits for an event of `view` that `wanted` accepts, failing with `what` after [`PATIENCE`].
fn wait_for(
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
    what: &str,
    mut wanted: impl FnMut(&WidgetHostEvent) -> bool,
) -> Result<WidgetHostEvent, String> {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(WidgetHostEvent::Failed {
                view: failed,
                error,
            }) if failed == view => {
                return Err(format!("the view failed while waiting for {what}: {error}"));
            }
            Ok(event) => {
                if event_view(&event) == view && wanted(&event) {
                    return Ok(event);
                }
            }
            Err(TryRecvError::Closed) => {
                return Err(format!("the event stream ended before {what}"));
            }
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err(format!("no {what} within {PATIENCE:?}"))
}

fn event_view(event: &WidgetHostEvent) -> ViewId {
    match event {
        WidgetHostEvent::Frame { view, .. }
        | WidgetHostEvent::Message { view, .. }
        | WidgetHostEvent::Failed { view, .. } => *view,
    }
}

fn is_red_at(event: &WidgetHostEvent, x: u32, y: u32) -> bool {
    let WidgetHostEvent::Frame {
        width,
        height,
        rgba,
        ..
    } = event
    else {
        return false;
    };
    if x >= *width || y >= *height {
        return false;
    }
    let at = ((y * width + x) * 4) as usize;
    matches!(rgba.get(at..at + 4), Some(&[red, green, blue, 255]) if red >= 250 && green <= 5 && blue <= 5)
}

fn message_containing(needle: &str) -> impl FnMut(&WidgetHostEvent) -> bool + '_ {
    move |event| matches!(event, WidgetHostEvent::Message { json, .. } if json.contains(needle))
}

fn widget_posts_and_paints(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(1);
    let html = "<script>\
        addEventListener('message', function (event) { \
            if (event.source === parent) parent.postMessage({echo: event.data}, '*'); \
        });\
        parent.postMessage({jsonrpc:\"2.0\",id:1,method:\"ui/initialize\",params:{}},\"*\")\
        </script><body style=\"background:#f00\">";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 2.0)
        .map_err(|e| e.to_string())?;

    wait_for(
        events,
        view,
        "the ui/initialize message",
        message_containing("\"method\":\"ui/initialize\""),
    )?;
    let frame = wait_for(events, view, "a red 400x200 frame", |event| {
        matches!(
            event,
            WidgetHostEvent::Frame {
                width: 400,
                height: 200,
                ..
            }
        ) && is_red_at(event, 5, 5)
    })?;
    drop(frame);

    host.post(
        view,
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"echo-me"}}"#.into(),
    )
    .map_err(|e| e.to_string())?;
    wait_for(
        events,
        view,
        "the echoed reply",
        message_containing("\"protocolVersion\":\"echo-me\""),
    )?;
    host.close(view).map_err(|e| e.to_string())
}

fn widget_cannot_fetch_file_urls(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(2);
    // The policy allows `file:` on purpose, so that it is the host, not the page's own policy,
    // that keeps the file out of reach.
    let csp = "default-src 'none'; script-src 'unsafe-inline'; connect-src file:";
    let html = "<script>\
        fetch('file:///etc/hostname')\
          .then(function (response) { return response.text(); })\
          .then(function (text) { parent.postMessage({fetched: text}, '*'); },\
                function (error) { parent.postMessage({fetchFailed: String(error)}, '*'); });\
        </script>";
    host.open(view, page(html, csp))
        .map_err(|e| e.to_string())?;
    let outcome = wait_for(events, view, "the fetch outcome", |event| {
        matches!(event, WidgetHostEvent::Message { json, .. }
            if json.contains("fetchFailed") || json.contains("fetched"))
    })?;
    host.close(view).map_err(|e| e.to_string())?;
    match outcome {
        WidgetHostEvent::Message { json, .. } if json.contains("fetchFailed") => Ok(()),
        other => Err(format!("the page read a file: {other:?}")),
    }
}
