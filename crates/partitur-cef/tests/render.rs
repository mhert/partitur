//! Renders real widget pages in CEF and checks what comes back through the widget host port.
//!
//! The binary has its own `main` because CEF starts its helper processes by running this same
//! binary again; `partitur_cef::subprocess` hands those over to CEF before anything else runs.
//! CEF can be started only once per process, so one host serves every test, each on a view of its
//! own. The tests need a display: without `DISPLAY` or `WAYLAND_DISPLAY` they are skipped with a
//! printed reason, and CI runs them under `xvfb-run`. On macOS, where CEF widgets are not
//! supported yet, they are skipped as well.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use partitur_cef::CefWidgetHost;
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    Picture, ViewId, WidgetCursor, WidgetHost, WidgetHostEvent, WidgetInput, WidgetPage,
};

/// How long a test waits for what it expects before it fails.
const PATIENCE: Duration = Duration::from_secs(20);

/// The policy the test pages run under: inline script and style, nothing else.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'";

fn main() -> ExitCode {
    if let Some(code) = partitur_cef::subprocess() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1)); // coverage: runs in a CEF helper process
    }
    if cfg!(target_os = "macos") {
        println!("skipping the CEF render tests: CEF widgets are not supported on macOS yet");
        return ExitCode::SUCCESS;
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        println!("skipping the CEF render tests: neither DISPLAY nor WAYLAND_DISPLAY is set");
        return ExitCode::SUCCESS;
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .without_time()
        .with_ansi(false)
        .init();
    let tests: [(&str, Test); 7] = [
        (
            "a_link_under_the_pointer_asks_for_the_hand",
            a_link_under_the_pointer_asks_for_the_hand,
        ),
        ("widget_posts_and_paints", widget_posts_and_paints),
        (
            "widget_has_cookies_and_storage_but_not_the_proxy",
            widget_has_cookies_and_storage_but_not_the_proxy,
        ),
        (
            "widget_cannot_fetch_file_urls",
            widget_cannot_fetch_file_urls,
        ),
        (
            "widget_cannot_reach_another_views_origins",
            widget_cannot_reach_another_views_origins,
        ),
        (
            "a_broken_nested_frame_leaves_the_widget_standing",
            a_broken_nested_frame_leaves_the_widget_standing,
        ),
        (
            "an_animating_widget_queues_one_frame",
            an_animating_widget_queues_one_frame,
        ),
    ];
    // The host lives in this block, so that CEF has shut down before the summary.
    let failed = {
        let (host, mut events) = match partitur_cef::start() {
            Ok(started) => started,
            Err(error) => {
                println!("{error}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "CEF runs {} the process sandbox",
            if host.sandboxed() { "with" } else { "without" }
        );
        run(tests, &host, &mut events)
    };
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

/// Runs `tests` one after the other and returns how many failed.
fn run(
    tests: [(&str, Test); 7],
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> usize {
    let mut failed = 0;
    for (name, test) in tests {
        match test(host, events) {
            Ok(()) => println!("test {name} ... ok"),
            Err(error) => {
                println!("test {name} ... FAILED: {error}");
                failed += 1;
            }
        }
    }
    failed
}

type Test = fn(&CefWidgetHost, &mut UnboundedReceiver<WidgetHostEvent>) -> Result<(), String>;

fn page(html: &str, csp: &str) -> WidgetPage {
    WidgetPage {
        html: html.into(),
        csp: csp.into(),
        theme: WidgetTheme::Light,
    }
}

/// What the host reported, with the picture a frame event announced taken from its slot.
#[derive(Debug)]
enum Seen {
    Frame { view: ViewId, picture: Picture },
    Cursor { view: ViewId, cursor: WidgetCursor },
    Message { view: ViewId, json: String },
    Failed { view: ViewId, error: String },
}

/// The next event, if one is there: `Ok(None)` while none is, and also for a frame event whose
/// picture an earlier one took already.
fn next(events: &mut UnboundedReceiver<WidgetHostEvent>) -> Result<Option<Seen>, TryRecvError> {
    Ok(match events.try_recv()? {
        WidgetHostEvent::Frame { view, latest } => {
            latest.take().map(|picture| Seen::Frame { view, picture })
        }
        WidgetHostEvent::Cursor { view, cursor } => Some(Seen::Cursor { view, cursor }),
        WidgetHostEvent::Message { view, json } => Some(Seen::Message { view, json }),
        WidgetHostEvent::OpenLink { .. } => None,
        WidgetHostEvent::Failed { view, error } => Some(Seen::Failed { view, error }),
    })
}

/// Waits for an event of `view` that `wanted` accepts, failing with `what` after [`PATIENCE`].
fn wait_for(
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
    what: &str,
    mut wanted: impl FnMut(&Seen) -> bool,
) -> Result<Seen, String> {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        match next(events) {
            Ok(Some(Seen::Failed {
                view: failed,
                error,
            })) if failed == view => {
                return Err(format!("the view failed while waiting for {what}: {error}"));
            }
            Ok(Some(event)) => {
                if event_view(&event) == view && wanted(&event) {
                    return Ok(event);
                }
            }
            Err(TryRecvError::Closed) => {
                return Err(format!("the event stream ended before {what}"));
            }
            Ok(None) | Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err(format!("no {what} within {PATIENCE:?}"))
}

/// Gathers every event until `done` accepts one, and then for `linger` longer, so that what should
/// not happen has had its chance to.
fn gather(
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    what: &str,
    mut done: impl FnMut(&Seen) -> bool,
    linger: Duration,
) -> Result<Vec<Seen>, String> {
    let mut deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    let mut finished = false;
    while Instant::now() < deadline {
        match next(events) {
            Ok(Some(event)) => {
                if !finished && done(&event) {
                    finished = true;
                    deadline = Instant::now() + linger;
                }
                seen.push(event);
            }
            Err(TryRecvError::Closed) => break,
            Ok(None) | Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    if finished {
        Ok(seen)
    } else {
        Err(format!(
            "no {what} within {PATIENCE:?}; saw {}",
            summary(&seen)
        ))
    }
}

/// The events in short: messages and failures in full, frames by view and size.
fn summary(events: &[Seen]) -> String {
    let parts: Vec<String> = events
        .iter()
        .map(|event| match event {
            Seen::Frame { view, picture } => {
                format!("Frame({}, {}x{})", view.0, picture.width, picture.height)
            }
            other => format!("{other:?}"),
        })
        .collect();
    parts.join(", ")
}

fn is_message(event: &Seen, of: ViewId, needle: &str) -> bool {
    matches!(event, Seen::Message { view, json } if *view == of && json.contains(needle))
}

fn event_view(event: &Seen) -> ViewId {
    match event {
        Seen::Frame { view, .. }
        | Seen::Cursor { view, .. }
        | Seen::Message { view, .. }
        | Seen::Failed { view, .. } => *view,
    }
}

fn is_red_at(event: &Seen, x: u32, y: u32) -> bool {
    let Seen::Frame {
        picture: Picture {
            width,
            height,
            rgba,
        },
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

fn message_containing(needle: &str) -> impl FnMut(&Seen) -> bool + '_ {
    move |event| matches!(event, Seen::Message { json, .. } if json.contains(needle))
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
            Seen::Frame {
                picture: Picture {
                    width: 400,
                    height: 200,
                    ..
                },
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

fn a_link_under_the_pointer_asks_for_the_hand(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(8);
    // The link fills the view, so the pointer is over it wherever it is.
    let html = "<a href=\"https://example.com/\" \
        style=\"position:fixed;inset:0;display:block\">example</a>\
        <script>parent.postMessage({linkShown: true}, '*');</script>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    wait_for(events, view, "the link", message_containing("linkShown"))?;
    let deadline = Instant::now() + PATIENCE;
    let mut x = 50.0;
    let mut hand = false;
    while !hand && Instant::now() < deadline {
        // A move every round, as the pointer may come before the page is laid out.
        host.input(view, WidgetInput::Move { x, y: 50.0 })
            .map_err(|e| e.to_string())?;
        x = if x > 50.0 { 50.0 } else { 60.0 };
        let round = Instant::now() + Duration::from_millis(200);
        while Instant::now() < round {
            match next(events) {
                Ok(Some(Seen::Cursor {
                    view: of,
                    cursor: WidgetCursor::Pointer,
                })) if of == view => {
                    hand = true;
                    break;
                }
                Ok(_) | Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
                Err(TryRecvError::Closed) => return Err("the event stream ended".into()),
            }
        }
    }
    host.close(view).map_err(|e| e.to_string())?;
    if hand {
        Ok(())
    } else {
        Err(format!("no hand over the link within {PATIENCE:?}"))
    }
}

fn widget_has_cookies_and_storage_but_not_the_proxy(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(7);
    // Every step reports its own outcome, so that one that throws does not hide the others. The
    // cookie only has to be readable and writable: like any cross-site frame's, it is not kept.
    let html = "<script>\
        var out = {};\
        try { document.cookie = 'probe=1'; out.cookie = typeof document.cookie; }\
        catch (error) { out.cookieError = String(error); }\
        try { localStorage.setItem('probe', 'stored'); out.storage = localStorage.getItem('probe'); }\
        catch (error) { out.storageError = String(error); }\
        try { out.proxyTitle = String(parent.document.title); }\
        catch (error) { out.proxyBlocked = error.name; }\
        parent.postMessage({storageOutcome: out}, '*');\
        </script>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    let outcome = wait_for(
        events,
        view,
        "the storage outcome",
        message_containing("storageOutcome"),
    )?;
    host.close(view).map_err(|e| e.to_string())?;
    let Seen::Message { json, .. } = outcome else {
        return Err(format!("not a message: {outcome:?}"));
    };
    let expected = r#"{"storageOutcome":{"cookie":"string","storage":"stored","proxyBlocked":"SecurityError"}}"#;
    if json == expected {
        Ok(())
    } else {
        Err(format!("the widget saw {json}, not {expected}"))
    }
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
        matches!(event, Seen::Message { json, .. }
            if json.contains("fetchFailed") || json.contains("fetched"))
    })?;
    host.close(view).map_err(|e| e.to_string())?;
    match outcome {
        Seen::Message { json, .. } if json.contains("fetchFailed") => Ok(()),
        other => Err(format!("the page read a file: {other:?}")),
    }
}

fn widget_cannot_reach_another_views_origins(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let (a, b) = (ViewId(3), ViewId(4));
    // B's document speaks up only when it runs inside somebody else's frame.
    let b_html = "<script>if (parent !== top) parent.postMessage({iAmB: true}, '*');</script>";
    host.open(b, page(b_html, CSP)).map_err(|e| e.to_string())?;
    // A's own policy allows both Partitur schemes, so that only the host stands in the way.
    let a_csp = "default-src 'none'; script-src 'unsafe-inline'; \
                 connect-src partitur-widget:; frame-src partitur-app:";
    let a_html = "<script>\
        addEventListener('message', function (event) { \
            if (event.source !== parent) parent.postMessage({relayed: event.data}, '*'); \
        });\
        fetch('partitur-widget://view-4/bridge', {method: 'POST', mode: 'no-cors', body: 'x'})\
          .then(function () { parent.postMessage({crossPosted: true}, '*'); },\
                function (error) { parent.postMessage({crossFailed: String(error)}, '*'); });\
        </script><iframe src=\"partitur-app://view-4/\"></iframe>";
    host.open(a, page(a_html, a_csp))
        .map_err(|e| e.to_string())?;
    let seen = gather(
        events,
        "the outcome of A's post to B",
        |event| is_message(event, a, "crossPosted") || is_message(event, a, "crossFailed"),
        Duration::from_secs(2),
    )?;
    host.close(a).map_err(|e| e.to_string())?;
    host.close(b).map_err(|e| e.to_string())?;
    if !seen.iter().any(|event| is_message(event, a, "crossFailed")) {
        return Err(format!(
            "A's post to B's bridge went through: {}",
            summary(&seen)
        ));
    }
    if let Some(event) = seen
        .iter()
        .find(|event| matches!(event, Seen::Message { view, .. } if *view == b))
    {
        return Err(format!("a message arrived for B: {event:?}"));
    }
    if let Some(event) = seen.iter().find(|event| is_message(event, a, "iAmB")) {
        return Err(format!("A loaded B's document: {event:?}"));
    }
    Ok(())
}

fn a_broken_nested_frame_leaves_the_widget_standing(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(5);
    // One nested frame names a host that does not resolve, the other is blocked by the page's own
    // policy; both fail below the widget's document.
    let csp = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
               frame-src https://partitur.invalid";
    let html = "<body style=\"background:#f00\">\
        <iframe src=\"https://partitur.invalid/\"></iframe>\
        <iframe src=\"https://example.com/\"></iframe>\
        <script>addEventListener('load', function () { \
            parent.postMessage({nestedDone: true}, '*'); });</script>";
    host.open(view, page(html, csp))
        .map_err(|e| e.to_string())?;
    let seen = gather(
        events,
        "the widget's load",
        |event| is_message(event, view, "nestedDone"),
        Duration::from_secs(2),
    )?;
    host.close(view).map_err(|e| e.to_string())?;
    if let Some(event) = seen
        .iter()
        .find(|event| matches!(event, Seen::Failed { view: failed, .. } if *failed == view))
    {
        return Err(format!("the widget failed for a nested frame: {event:?}"));
    }
    if !seen
        .iter()
        .any(|event| event_view(event) == view && is_red_at(event, 5, 5))
    {
        return Err(format!(
            "the widget painted no red frame: {}",
            summary(&seen)
        ));
    }
    Ok(())
}

fn an_animating_widget_queues_one_frame(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(6);
    // A new colour every animation frame, so CEF paints the view as often as it can.
    let html = "<body><script>\
        var shade = 0;\
        function step() { shade = (shade + 1) % 256; \
            document.body.style.background = 'rgb(' + shade + ',0,0)'; \
            requestAnimationFrame(step); }\
        requestAnimationFrame(step);\
        parent.postMessage({animating: true}, '*');\
        </script>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    wait_for(
        events,
        view,
        "the animation's start",
        message_containing("animating"),
    )?;
    // Nobody takes the pictures for a second, while the page paints dozens of them.
    std::thread::sleep(Duration::from_secs(1));
    let mut waiting = Vec::new();
    while let Ok(event) = events.try_recv() {
        waiting.push(event);
    }
    host.close(view).map_err(|e| e.to_string())?;
    let frames: Vec<&WidgetHostEvent> = waiting
        .iter()
        .filter(|event| matches!(event, WidgetHostEvent::Frame { view: of, .. } if *of == view))
        .collect();
    let [WidgetHostEvent::Frame { latest, .. }] = frames.as_slice() else {
        return Err(format!(
            "{} frame events of the view waited, not one",
            frames.len()
        ));
    };
    if latest.take().is_none() {
        return Err("the waiting frame event announced no picture".into());
    }
    Ok(())
}
