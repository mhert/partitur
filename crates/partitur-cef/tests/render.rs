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
    let tests = tests();
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
        run(&tests, &host, &mut events)
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

/// Every test, by name.
fn tests() -> [(&'static str, Test); TESTS] {
    [
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
        (
            "a_clicked_link_is_handed_on_and_the_widget_stays",
            a_clicked_link_is_handed_on_and_the_widget_stays,
        ),
        (
            "a_navigation_without_a_click_is_stopped_quietly",
            a_navigation_without_a_click_is_stopped_quietly,
        ),
        (
            "a_link_to_a_new_window_is_handed_on",
            a_link_to_a_new_window_is_handed_on,
        ),
        (
            "a_window_opened_on_a_click_is_handed_on",
            a_window_opened_on_a_click_is_handed_on,
        ),
        ("the_wheel_scrolls_the_widget", the_wheel_scrolls_the_widget),
        (
            "a_nested_frame_navigates_as_the_policy_lets_it",
            a_nested_frame_navigates_as_the_policy_lets_it,
        ),
        (
            "what_the_widget_leaves_unpainted_is_transparent",
            what_the_widget_leaves_unpainted_is_transparent,
        ),
        (
            "a_precise_wheel_scrolls_at_once_by_its_amount",
            a_precise_wheel_scrolls_at_once_by_its_amount,
        ),
        (
            "a_touchpad_stroke_scrolls_the_widget_by_all_its_amounts",
            a_touchpad_stroke_scrolls_the_widget_by_all_its_amounts,
        ),
        (
            "a_nested_frame_cannot_load_the_proxy_page",
            a_nested_frame_cannot_load_the_proxy_page,
        ),
        (
            "a_page_follows_the_theme_it_is_given",
            a_page_follows_the_theme_it_is_given,
        ),
        (
            "a_theme_given_before_the_browser_is_up_is_kept",
            a_theme_given_before_the_browser_is_up_is_kept,
        ),
    ]
}

/// How many tests there are.
const TESTS: usize = 19;

/// Runs `tests` one after the other and returns how many failed.
fn run(
    tests: &[(&str, Test)],
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> usize {
    let mut failed = 0;
    for &(name, test) in tests {
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
    OpenLink { view: ViewId, url: String },
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
        WidgetHostEvent::OpenLink { view, url } => Some(Seen::OpenLink { view, url }),
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
        | Seen::OpenLink { view, .. }
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

/// The RGBA of the pixel at `x`, `y` of a frame event's picture.
fn pixel_at(event: &Seen, x: u32, y: u32) -> Option<[u8; 4]> {
    let Seen::Frame {
        picture: Picture {
            width,
            height,
            rgba,
        },
        ..
    } = event
    else {
        return None;
    };
    if x >= *width || y >= *height {
        return None;
    }
    let at = ((y * width + x) * 4) as usize;
    rgba.get(at..at + 4)?.try_into().ok()
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

/// Waits until the page of `view` said `{ready: true}` and has painted, in either order.
fn ready_and_painted(
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
) -> Result<(), String> {
    let (mut ready, mut painted) = (false, false);
    wait_for(events, view, "the page to run and paint", |event| {
        ready |= is_message(event, view, "ready");
        painted |= matches!(event, Seen::Frame { .. });
        ready && painted
    })
    .map(drop)
}

/// A page whose element `#target` fills the view, which echoes every message the host posts and
/// says `{ready: true}` once it runs; `body` is the element, `script` runs before the echo.
fn clickable(body: &str, script: &str) -> String {
    format!(
        "<style>#target {{ position: fixed; inset: 0; display: block; }}</style>{body}\
         <script>{script}\
         addEventListener('message', function (event) {{ \
             if (event.source === parent) parent.postMessage({{echo: event.data}}, '*'); \
         }});\
         parent.postMessage({{ready: true}}, '*');</script>"
    )
}

/// Opens `view` on `html`, waits until it runs and has painted, and then clicks its middle with
/// the left button until `wanted` accepts an event, gathering every event of the view for
/// `linger` after that. Each click waits a while for its outcome, as the first may come before
/// the page is laid out.
fn click_until(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
    html: &str,
    mut wanted: impl FnMut(&Seen) -> bool,
) -> Result<Vec<Seen>, String> {
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    ready_and_painted(events, view)?;
    let (x, y) = (100.0, 50.0);
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        host.input(view, WidgetInput::Move { x, y })
            .map_err(|e| e.to_string())?;
        host.input(view, WidgetInput::Press { x, y, button: 0 })
            .map_err(|e| e.to_string())?;
        host.input(view, WidgetInput::Release { x, y, button: 0 })
            .map_err(|e| e.to_string())?;
        let round = Instant::now() + Duration::from_secs(2);
        while Instant::now() < round {
            match next(events) {
                Ok(Some(event)) if event_view(&event) == view => {
                    let done = wanted(&event);
                    seen.push(event);
                    if done {
                        return Ok(seen);
                    }
                }
                Ok(_) | Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
                Err(TryRecvError::Closed) => return Err("the event stream ended".into()),
            }
        }
    }
    Err(format!(
        "nothing wanted within {PATIENCE:?}; saw {}",
        summary(&seen)
    ))
}

/// That the page of `view` still answers: a post comes back as its echo, and the view never
/// failed among `seen` nor while waiting.
fn still_answers(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
    seen: &[Seen],
) -> Result<(), String> {
    if let Some(failed) = seen
        .iter()
        .find(|event| matches!(event, Seen::Failed { .. }))
    {
        return Err(format!("the widget failed: {failed:?}"));
    }
    host.post(view, r#"{"still":"here"}"#.into())
        .map_err(|e| e.to_string())?;
    wait_for(
        events,
        view,
        "the echo of a later post",
        message_containing("\"still\":\"here\""),
    )?;
    Ok(())
}

fn is_link_to(event: &Seen, wanted: &str) -> bool {
    matches!(event, Seen::OpenLink { url, .. } if url == wanted)
}

fn a_clicked_link_is_handed_on_and_the_widget_stays(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(9);
    let html = clickable("<a id=\"target\" href=\"https://example.com/x\">x</a>", "");
    let outcome = click_until(host, events, view, &html, |event| {
        matches!(event, Seen::OpenLink { .. } | Seen::Failed { .. })
    })
    .and_then(|seen| {
        if seen
            .iter()
            .any(|event| is_link_to(event, "https://example.com/x"))
        {
            Ok(seen)
        } else {
            Err(format!("no link handed on: {}", summary(&seen)))
        }
    })
    .and_then(|seen| still_answers(host, events, view, &seen));
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}

fn a_navigation_without_a_click_is_stopped_quietly(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(10);
    // The page leaves for elsewhere on its own, and opens a window, then says it is still there.
    let html = clickable(
        "",
        "setTimeout(function () { location.href = 'https://example.com/scripted'; }, 100);\
         setTimeout(function () { window.open('https://example.com/unasked'); }, 200);\
         setTimeout(function () { parent.postMessage({stayed: true}, '*'); }, 1500);",
    );
    host.open(view, page(&html, CSP))
        .map_err(|e| e.to_string())?;
    let outcome = gather(
        events,
        "the page saying it stayed",
        |event| is_message(event, view, "stayed"),
        Duration::from_secs(1),
    )
    .and_then(|seen| {
        let mine: Vec<&Seen> = seen.iter().filter(|e| event_view(e) == view).collect();
        if let Some(link) = mine
            .iter()
            .find(|event| matches!(event, Seen::OpenLink { .. }))
        {
            return Err(format!("a link was handed on without a click: {link:?}"));
        }
        Ok(seen)
    })
    .and_then(|seen| still_answers(host, events, view, &seen));
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}

fn a_link_to_a_new_window_is_handed_on(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(11);
    let html = clickable(
        "<a id=\"target\" href=\"https://example.com/blank\" target=\"_blank\">x</a>",
        "",
    );
    let outcome = click_until(host, events, view, &html, |event| {
        is_link_to(event, "https://example.com/blank")
    })
    .and_then(|seen| still_answers(host, events, view, &seen));
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}

fn a_window_opened_on_a_click_is_handed_on(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(12);
    let html = clickable(
        "<div id=\"target\"></div>",
        "document.getElementById('target').addEventListener('click', function () { \
             window.open('https://example.com/opened'); });",
    );
    let outcome = click_until(host, events, view, &html, |event| {
        is_link_to(event, "https://example.com/opened")
    })
    .and_then(|seen| still_answers(host, events, view, &seen));
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}

fn the_wheel_scrolls_the_widget(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(13);
    let html = "<body style=\"margin:0\"><div style=\"height:3000px\"></div><script>\
        addEventListener('scroll', function () { \
            parent.postMessage({scrolledTo: scrollY}, '*'); });\
        parent.postMessage({ready: true}, '*');\
        </script>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    ready_and_painted(events, view)?;
    let (x, y) = (100.0, 50.0);
    let deadline = Instant::now() + PATIENCE;
    let mut scrolled = false;
    while !scrolled && Instant::now() < deadline {
        host.input(view, WidgetInput::Move { x, y })
            .map_err(|e| e.to_string())?;
        host.input(
            view,
            WidgetInput::Wheel {
                x,
                y,
                dx: 0.0,
                dy: -120.0,
                precise: false,
            },
        )
        .map_err(|e| e.to_string())?;
        let round = Instant::now() + Duration::from_secs(1);
        while !scrolled && Instant::now() < round {
            match next(events) {
                Ok(Some(event)) => scrolled = is_message(&event, view, "scrolledTo"),
                Ok(None) | Err(TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Closed) => return Err("the event stream ended".into()),
            }
        }
    }
    host.close(view).map_err(|e| e.to_string())?;
    if scrolled {
        Ok(())
    } else {
        Err(format!(
            "the widget's document did not scroll within {PATIENCE:?}"
        ))
    }
}

fn a_nested_frame_navigates_as_the_policy_lets_it(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(14);
    // The frame the widget nests shows one page, which on its own goes on to a second; the
    // widget relays what the second says.
    let csp = "default-src 'none'; script-src 'unsafe-inline'; frame-src data:";
    let html = "<body><script>\
        addEventListener('message', function (event) { \
            if (event.source !== parent) parent.postMessage({relayed: event.data}, '*'); \
        });\
        var second = 'data:text/html,' + encodeURIComponent(\
            \"<script>parent.postMessage({secondPage: true}, '*')<\\/script>\");\
        var first = 'data:text/html,' + encodeURIComponent(\
            '<script>location.href = ' + JSON.stringify(second) + '<\\/script>');\
        var frame = document.createElement('iframe');\
        frame.src = first;\
        document.body.appendChild(frame);\
        </script>";
    host.open(view, page(html, csp))
        .map_err(|e| e.to_string())?;
    let outcome = wait_for(
        events,
        view,
        "the nested frame's second page",
        message_containing("secondPage"),
    );
    host.close(view).map_err(|e| e.to_string())?;
    outcome.map(drop)
}

fn what_the_widget_leaves_unpainted_is_transparent(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(15);
    // A red square in the corner, a half-transparent red one beside it and nothing else, so
    // the transcript shows through the rest in either theme.
    let html = "<body style=\"margin:0\">\
        <div style=\"position:absolute;left:0;top:0;width:20px;height:20px;background:#f00\"></div>\
        <div style=\"position:absolute;left:40px;top:0;width:20px;height:20px;\
            background:rgba(255,0,0,0.5)\"></div>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    let outcome = wait_for(events, view, "a frame with the red square", |event| {
        is_red_at(event, 5, 5)
    });
    host.close(view).map_err(|e| e.to_string())?;
    let frame = outcome?;
    match pixel_at(&frame, 150, 50) {
        Some([_, _, _, 0]) => {}
        other => return Err(format!("the unpainted part is {other:?}, not transparent")),
    }
    // Straight alpha, as the UI draws it: the colour is red at full strength.
    match pixel_at(&frame, 45, 5) {
        Some([red, 0..=5, 0..=5, 120..=135]) if red >= 250 => Ok(()),
        other => Err(format!("the half-transparent red is {other:?}")),
    }
}

fn a_precise_wheel_scrolls_at_once_by_its_amount(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(16);
    let html = "<body style=\"margin:0\"><div style=\"height:3000px\"></div><script>\
        addEventListener('scroll', function () { \
            parent.postMessage({scrolledTo: scrollY}, '*'); });\
        parent.postMessage({ready: true}, '*');\
        </script>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    ready_and_painted(events, view)?;
    let (x, y) = (100.0, 50.0);
    let deadline = Instant::now() + PATIENCE;
    let mut offsets = Vec::new();
    // Turned until the page scrolls, as the first turns may come before it is laid out; then
    // its offsets for a moment longer.
    while offsets.is_empty() && Instant::now() < deadline {
        host.input(view, WidgetInput::Move { x, y })
            .map_err(|e| e.to_string())?;
        host.input(
            view,
            WidgetInput::Wheel {
                x,
                y,
                dx: 0.0,
                dy: -50.0,
                precise: true,
            },
        )
        .map_err(|e| e.to_string())?;
        let round = Instant::now() + Duration::from_millis(500);
        while Instant::now() < round {
            match next(events) {
                Ok(Some(Seen::Message { view: of, json })) if of == view => offsets.push(json),
                Ok(_) | Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
                Err(TryRecvError::Closed) => return Err("the event stream ended".into()),
            }
        }
    }
    host.close(view).map_err(|e| e.to_string())?;
    // Animated, the page would pass through offsets between the turns'.
    let exact = |json: &String| {
        json.strip_prefix(r#"{"scrolledTo":"#)
            .and_then(|rest| rest.strip_suffix('}'))
            .and_then(|offset| offset.parse::<u32>().ok())
            .is_some_and(|offset| offset > 0 && offset % 50 == 0)
    };
    if !offsets.is_empty() && offsets.iter().all(exact) {
        Ok(())
    } else {
        Err(format!("the page scrolled through {offsets:?}"))
    }
}

/// The offsets the page of `view` reported scrolling to within `within`, in the order it did.
fn scroll_offsets(
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
    within: Duration,
) -> Result<Vec<f64>, String> {
    let until = Instant::now() + within;
    let mut offsets = Vec::new();
    while Instant::now() < until {
        match next(events) {
            Ok(Some(Seen::Message { view: of, json })) if of == view => {
                if let Some(offset) = json
                    .strip_prefix(r#"{"scrolledTo":"#)
                    .and_then(|rest| rest.strip_suffix('}'))
                    .and_then(|offset| offset.parse::<f64>().ok())
                {
                    offsets.push(offset);
                }
            }
            Ok(_) | Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
            Err(TryRecvError::Closed) => return Err("the event stream ended".into()),
        }
    }
    Ok(offsets)
}

/// Sends `view` a touchpad's stroke at `(x, y)`: `turns` precise moves of `dy` pixels each, 8
/// milliseconds apart, with no move of the pointer between them and no phase of a gesture, as
/// the UI hands them on.
fn stroke(
    host: &CefWidgetHost,
    view: ViewId,
    (x, y): (f32, f32),
    turns: u32,
    dy: f32,
) -> Result<(), String> {
    for _ in 0..turns {
        host.input(
            view,
            WidgetInput::Wheel {
                x,
                y,
                dx: 0.0,
                dy,
                precise: true,
            },
        )
        .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(8));
    }
    Ok(())
}

// A touchpad on Wayland comes as many small precise amounts with no gesture phases. The widget's
// document, in its frame under the proxy page, scrolls by all of them, and a second stroke after
// a pause scrolls it on.
fn a_touchpad_stroke_scrolls_the_widget_by_all_its_amounts(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(17);
    let html = "<body style=\"margin:0\"><div style=\"height:3000px\"></div><script>\
        addEventListener('scroll', function () { \
            parent.postMessage({scrolledTo: scrollY}, '*'); });\
        parent.postMessage({ready: true}, '*');\
        </script>";
    host.open(view, page(html, CSP))
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    ready_and_painted(events, view)?;
    let at = (100.0, 50.0);
    host.input(view, WidgetInput::Move { x: at.0, y: at.1 })
        .map_err(|e| e.to_string())?;
    // Stroked until the page scrolls, as the first strokes may come before it is laid out.
    let deadline = Instant::now() + PATIENCE;
    let mut start = None;
    while start.is_none() && Instant::now() < deadline {
        stroke(host, view, at, 1, -10.0)?;
        start = scroll_offsets(events, view, Duration::from_millis(500))?
            .last()
            .copied();
    }
    let outcome = (|| {
        let start = start.ok_or("the page never scrolled")?;
        let mut reached = Vec::new();
        for _ in 0..2 {
            stroke(host, view, at, 20, -3.0)?;
            let offsets = scroll_offsets(events, view, Duration::from_millis(700))?;
            reached.push(offsets.last().copied());
        }
        let wanted = [Some(start + 60.0), Some(start + 120.0)];
        if reached == wanted {
            Ok(())
        } else {
            Err(format!(
                "from {start} the strokes reached {reached:?}, not {wanted:?}"
            ))
        }
    })();
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}

// A widget whose own policy lets it frame the proxy's scheme still cannot nest the proxy page:
// that page would share the proxy's origin and could rewrite the page above the widget. Had it
// loaded, it would load the widget's document once more, which then speaks up from deep down.
fn a_nested_frame_cannot_load_the_proxy_page(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(18);
    let csp = "default-src 'none'; script-src 'unsafe-inline'; \
               frame-src partitur-widget: partitur-app:; connect-src partitur-widget:";
    let html = "<body><script>\
        if (parent !== top) { parent.postMessage({nestedAgain: true}, '*'); } else {\
          var frame = document.createElement('iframe');\
          frame.src = 'partitur-widget://' + location.host + '/';\
          frame.onload = function () { parent.postMessage({nestedLoaded: true}, '*'); };\
          document.body.appendChild(frame);\
          parent.postMessage({outer: true}, '*');\
        }\
        </script>";
    host.open(view, page(html, csp))
        .map_err(|e| e.to_string())?;
    let seen = gather(
        events,
        "the outer widget",
        |event| is_message(event, view, "outer"),
        Duration::from_secs(3),
    );
    host.close(view).map_err(|e| e.to_string())?;
    let seen = seen?;
    if let Some(event) = seen
        .iter()
        .find(|event| is_message(event, view, "nestedAgain"))
    {
        return Err(format!(
            "the nested proxy page loaded the widget again: {event:?}"
        ));
    }
    Ok(())
}

/// A page whose body is black under a dark `prefers-color-scheme` and white otherwise.
const THEMED: &str = "<style>\
    body { margin: 0; background: #ffffff; }\
    @media (prefers-color-scheme: dark) { body { background: #000000; } }\
    </style><body>";

/// Waits for a frame of `view` whose centre is `rgb`, opaque.
fn centre_is(
    events: &mut UnboundedReceiver<WidgetHostEvent>,
    view: ViewId,
    what: &str,
    rgb: [u8; 3],
) -> Result<(), String> {
    let [red, green, blue] = rgb;
    wait_for(events, view, what, |event| {
        pixel_at(event, 100, 50) == Some([red, green, blue, 255])
    })
    .map(drop)
}

fn a_page_follows_the_theme_it_is_given(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(18);
    let page = WidgetPage {
        theme: WidgetTheme::Dark,
        ..page(THEMED, CSP)
    };
    host.open(view, page).map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    let outcome = centre_is(events, view, "a black frame", [0, 0, 0]).and_then(|()| {
        host.set_theme(view, WidgetTheme::Light)
            .map_err(|e| e.to_string())?;
        centre_is(events, view, "a white frame", [255, 255, 255])
    });
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}

fn a_theme_given_before_the_browser_is_up_is_kept(
    host: &CefWidgetHost,
    events: &mut UnboundedReceiver<WidgetHostEvent>,
) -> Result<(), String> {
    let view = ViewId(19);
    // Sent right behind the open, so it reaches the view before CEF has created its browser.
    host.open(view, page(THEMED, CSP))
        .map_err(|e| e.to_string())?;
    host.set_theme(view, WidgetTheme::Dark)
        .map_err(|e| e.to_string())?;
    host.resize(view, 200, 100, 1.0)
        .map_err(|e| e.to_string())?;
    let outcome = centre_is(events, view, "a black frame", [0, 0, 0]);
    host.close(view).map_err(|e| e.to_string())?;
    outcome
}
