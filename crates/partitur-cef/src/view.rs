//! One windowless CEF browser per widget view, and the commands that drive them.
//!
//! [`Views`] lives on CEF's UI thread and carries out every [`Command`] there: it opens a browser
//! on the view's proxy page, resizes it, hides it, forwards pointer input and delivers messages
//! into the page, and closes it again. CEF reports back through the handlers of each view's
//! client: a paint, at most 60 a second over a transparent background, copies only the rows of
//! its dirty rectangles, in BGRA with premultiplied alpha as CEF paints them, into the picture
//! the view's [`LatestFrame`] keeps, and wakes the writer of the helper's standard output only
//! when the writer took what was painted before, so a fast painting page never queues more than
//! one frame of its view and no paint converts or allocates; a change of the pointer's look
//! becomes a [`FromEngine::Cursor`], with the looks the UI has no match for as the default arrow;
//! a load error of the proxy page or of the widget's document, or a renderer that stopped,
//! becomes [`FromEngine::Failed`] (frames the widget nests may fail on their own), and the
//! creation, load and close of a browser come back to [`Views`] as commands, through the queue
//! that every command takes to the UI thread.
//!
//! While the sandbox probe runs, the one view open is the probe's, and none of its events goes
//! out: a message from its page or its failure goes back to the loop as [`Command::Probed`], and
//! everything else it reports is dropped.
//!
//! A view's size is logical pixels at a device scale factor; CEF learns the scale through the
//! screen info and paints at logical size times scale. A wheel turn's amounts are logical pixels
//! too. CEF on Windows reads them otherwise: as `WHEEL_DELTA` (120) units of a notch, each
//! notch scrolling the system's wheel lines (vertically) or characters (horizontally) at 100/3
//! pixels each, the result cut to whole pixels. So there each amount goes out converted
//! ([`notch_units`]) with the settings of `HKCU\Control Panel\Desktop`, read at each turn, so
//! that CEF scrolls by the pixels the UI asked for, as it does elsewhere. Messages posted before the proxy page has
//! loaded wait for it. Every request a view's browser makes passes [`scheme::permitted`] first,
//! with the depth of the frame that makes it.
//!
//! Each view keeps the theme it was opened in, [`WidgetPage::theme`], or the one it was given
//! since. Its browser shows that theme to every frame as the `prefers-color-scheme` media
//! feature, through the `DevTools` method `Emulation.setEmulatedMedia`: once the browser has been
//! created, and again on each change. A change that comes before the browser exists is only
//! kept, and applied when it does.
//!
//! The proxy page and the widget's document stay in place: a navigation of either frame away
//! from the view's own page ([`scheme::may_navigate`]) is cancelled, and so is every new window
//! (a link with a target such as `_blank`, `window.open`, a middle click). When the user asked
//! for it with a click, the URL is reported as a [`FromEngine::OpenLink`] for the system's
//! browser; a page that navigates on its own is only stopped. Frames the widget nests navigate
//! as its policy lets them, but never to the proxy page.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use cef::rc::Rc as _;
use cef::sys::cef_event_flags_t;
use cef::{
    Browser, BrowserHost, BrowserSettings, Callback, CefString, Client, CursorInfo, CursorType,
    DictionaryValue, DisplayHandler, Errorcode, Frame, ImplBrowser, ImplBrowserHost, ImplClient,
    ImplDictionaryValue, ImplDisplayHandler, ImplFrame, ImplLifeSpanHandler, ImplListValue,
    ImplLoadHandler, ImplRenderHandler, ImplRequest, ImplRequestHandler,
    ImplResourceRequestHandler, LifeSpanHandler, LoadHandler, MouseButtonType, MouseEvent,
    PaintElementType, PopupFeatures, Rect, RenderHandler, Request, RequestHandler,
    ResourceRequestHandler, ReturnValue, RuntimeStyle, ScreenInfo, TerminationStatus, WindowInfo,
    WindowOpenDisposition, WrapClient, WrapDisplayHandler, WrapLifeSpanHandler, WrapLoadHandler,
    WrapRenderHandler, WrapRequestHandler, WrapResourceRequestHandler, browser_host_create_browser,
    dictionary_value_create, list_value_create, wrap_client, wrap_display_handler,
    wrap_life_span_handler, wrap_load_handler, wrap_render_handler, wrap_request_handler,
    wrap_resource_request_handler,
};
use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    LatestFrame, Region, ViewId, WidgetCursor, WidgetInput, WidgetPage,
};
use partitur_widgets::protocol::FromEngine;

use crate::engine::Commands;
use crate::scheme::{self, Owners, Pages};
use crate::serve::Out;

/// The size a view has before its first resize, in logical pixels.
const DEFAULT_SIZE: (u32, u32) = (300, 150);

/// How often a view repaints at most, per second: CEF's greatest rate, so that a page scrolls
/// and animates as smoothly as in a browser.
const FRAME_RATE: c_int = 60;

/// What CEF's UI thread is asked to do, by Partitur or by CEF's handlers.
pub(crate) enum Command {
    /// Opens a view on a page.
    Open(ViewId, WidgetPage),
    /// Delivers a JSON message to a view's page.
    Post(ViewId, String),
    /// Resizes a view to a logical width and height at a scale factor.
    Resize(ViewId, u32, u32, f32),
    /// Shows or hides a view.
    SetVisible(ViewId, bool),
    /// Forwards pointer input to a view.
    Input(ViewId, WidgetInput),
    /// Lets a view's page follow a color scheme.
    SetTheme(ViewId, WidgetTheme),
    /// Closes a view.
    Close(ViewId),
    /// CEF created the browser of one opening of a view.
    Created(ViewId, u64, Browser),
    /// The proxy page of one opening of a view has loaded.
    Loaded(ViewId, u64),
    /// CEF is about to destroy the browser with this identifier.
    Closed(c_int),
    /// The sandbox probe's page posted its message (`Ok`) or failed with the reason given.
    Probed(Result<(), String>),
    /// Closes everything and shuts CEF down.
    Shutdown,
}

/// What every view's handlers share.
pub(crate) struct Shared {
    /// Where events and the wake-ups of painted views go: the writer of standard output.
    pub(crate) out: Sender<Out>,
    /// The way back to CEF's UI thread.
    pub(crate) commands: Commands,
    /// The pages the scheme handlers serve.
    pub(crate) pages: Pages,
    /// The view of each live browser, which the scheme handlers check requests against.
    pub(crate) owners: Owners,
    /// Whether the sandbox probe runs, whose view's events do not go out.
    pub(crate) probing: AtomicBool,
}

impl Shared {
    /// Sends `event` out, or while the probe runs hands what it says about the probe back to the
    /// loop.
    pub(crate) fn send(&self, event: FromEngine) {
        if self.probing.load(Ordering::SeqCst) {
            match event {
                FromEngine::Message { .. } => self.commands.send(Command::Probed(Ok(()))),
                FromEngine::Failed { error, .. } => {
                    self.commands.send(Command::Probed(Err(error)));
                }
                _ => {}
            }
            return;
        }
        let _ = self.out.send(Out::Event(event));
    }

    /// Wakes the writer for the picture `view` painted into `latest`, unless the probe runs.
    fn painted(&self, view: ViewId, latest: LatestFrame) {
        if !self.probing.load(Ordering::SeqCst) {
            let _ = self.out.send(Out::Frame(view, latest));
        }
    }

    fn fail(&self, view: ViewId, error: String) {
        self.send(FromEngine::Failed { view, error });
    }

    /// Reports `url`, which a page of `view` was kept from opening, as a link to open, when the
    /// user asked for it (`user_gesture` is 1) and there is a URL at all.
    fn hand_on(&self, view: ViewId, url: Option<String>, user_gesture: c_int) {
        if let Some(url) = url.filter(|url| user_gesture == 1 && !url.is_empty()) {
            self.send(FromEngine::OpenLink { view, url });
        }
    }
}

/// A view's size in logical pixels and its device scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geometry {
    width: u32,
    height: u32,
    scale: f32,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
            scale: 1.0,
        }
    }
}

type SharedGeometry = Arc<Mutex<Geometry>>;

fn geometry(shared: &SharedGeometry) -> Geometry {
    *shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The UI thread's record of one open view.
struct View {
    generation: u64,
    geometry: SharedGeometry,
    browser: Option<Browser>,
    loaded: bool,
    posts: Vec<String>,
    hidden: bool,
    theme: WidgetTheme,
    buttons: u32,
    pointer: (c_int, c_int),
}

/// Every open view, and the browsers CEF has yet to create or destroy.
pub(crate) struct Views {
    shared: Arc<Shared>,
    open: HashMap<ViewId, View>,
    generation: u64,
    creating: usize,
    live: HashSet<c_int>,
}

impl Views {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            open: HashMap::new(),
            generation: 0,
            creating: 0,
            live: HashSet::new(),
        }
    }

    /// Whether a browser is still being created or has not been destroyed yet.
    pub(crate) fn has_browsers(&self) -> bool {
        self.creating > 0 || !self.live.is_empty()
    }

    /// Closes every view.
    pub(crate) fn close_all(&mut self) {
        let views: Vec<ViewId> = self.open.keys().copied().collect();
        for view in views {
            self.close(view);
        }
    }

    /// Carries out one command. [`Command::Probed`] and [`Command::Shutdown`] belong to the loop
    /// that calls this, and are ignored here.
    pub(crate) fn handle(&mut self, command: Command) {
        match command {
            Command::Open(view, page) => self.open(view, page),
            Command::Post(view, json) => self.post(view, json),
            Command::Resize(view, width, height, scale) => self.resize(view, width, height, scale),
            Command::SetVisible(view, visible) => self.set_visible(view, visible),
            Command::Input(view, input) => self.input(view, input),
            Command::SetTheme(view, theme) => self.set_theme(view, theme),
            Command::Close(view) => self.close(view),
            Command::Created(view, generation, browser) => self.created(view, generation, browser),
            Command::Loaded(view, generation) => self.loaded(view, generation),
            Command::Closed(id) => {
                self.live.remove(&id);
            }
            Command::Probed(_) | Command::Shutdown => {}
        }
    }

    fn open(&mut self, view: ViewId, page: WidgetPage) {
        self.close(view);
        self.generation += 1;
        let generation = self.generation;
        let theme = page.theme;
        self.shared
            .pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(view, page);
        let geometry = SharedGeometry::default();
        let mut client = ViewClient::new(
            ViewRender::new(
                view,
                self.shared.clone(),
                geometry.clone(),
                LatestFrame::default(),
            ),
            ViewLoad::new(view, generation, self.shared.clone()),
            ViewLifeSpan::new(view, generation, self.shared.clone()),
            ViewRequests::new(view, self.shared.clone(), ViewResourceRequests::new(view)),
            ViewDisplay::new(view, self.shared.clone()),
        );
        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
            runtime_style: RuntimeStyle::ALLOY,
            ..WindowInfo::default()
        };
        let settings = BrowserSettings {
            windowless_frame_rate: FRAME_RATE,
            // Transparent, so that what the page leaves unpainted shows the transcript under
            // it in either theme, and a change of theme needs nothing here.
            background_color: 0,
            ..BrowserSettings::default()
        };
        let url = CefString::from(scheme::proxy_url(view).as_str());
        let created = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&settings),
            None,
            None,
        );
        if created != 1 {
            self.forget_page(view);
            self.shared
                .fail(view, "CEF could not create the widget's browser".into());
            return;
        }
        self.creating += 1;
        self.open.insert(
            view,
            View {
                generation,
                geometry,
                browser: None,
                loaded: false,
                posts: Vec::new(),
                hidden: false,
                theme,
                buttons: 0,
                pointer: (0, 0),
            },
        );
    }

    fn forget_page(&self, view: ViewId) {
        self.shared
            .pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&view);
    }

    fn created(&mut self, view: ViewId, generation: u64, browser: Browser) {
        self.creating = self.creating.saturating_sub(1);
        self.live.insert(browser.identifier());
        match self.open.get_mut(&view) {
            Some(open) if open.generation == generation && open.browser.is_none() => {
                // A resize and a theme that came before the browser existed are applied now.
                if let Some(host) = browser.host() {
                    emulate_theme(&host, open.theme);
                    host.notify_screen_info_changed();
                    host.was_resized();
                    if open.hidden {
                        host.was_hidden(1);
                    }
                }
                open.browser = Some(browser);
            }
            _ => close_browser(&browser),
        }
    }

    fn loaded(&mut self, view: ViewId, generation: u64) {
        let Some(open) = self.open.get_mut(&view) else {
            return;
        };
        if open.generation != generation || open.loaded {
            return;
        }
        open.loaded = true;
        let posts = std::mem::take(&mut open.posts);
        if let Some(browser) = &open.browser {
            for json in posts {
                deliver(browser, &json);
            }
        }
    }

    fn post(&mut self, view: ViewId, json: String) {
        let Some(open) = self.open.get_mut(&view) else {
            return;
        };
        match &open.browser {
            Some(browser) if open.loaded => deliver(browser, &json),
            _ => open.posts.push(json),
        }
    }

    fn resize(&mut self, view: ViewId, width: u32, height: u32, scale: f32) {
        let Some(open) = self.open.get(&view) else {
            return;
        };
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        *open.geometry.lock().unwrap_or_else(PoisonError::into_inner) = Geometry {
            width,
            height,
            scale,
        };
        if let Some(host) = open.browser.as_ref().and_then(ImplBrowser::host) {
            host.notify_screen_info_changed();
            host.was_resized();
        }
    }

    fn set_visible(&mut self, view: ViewId, visible: bool) {
        let Some(open) = self.open.get_mut(&view) else {
            return;
        };
        open.hidden = !visible;
        if let Some(host) = open.browser.as_ref().and_then(ImplBrowser::host) {
            host.was_hidden(c_int::from(!visible));
        }
    }

    fn set_theme(&mut self, view: ViewId, theme: WidgetTheme) {
        let Some(open) = self.open.get_mut(&view) else {
            return;
        };
        open.theme = theme;
        if let Some(host) = open.browser.as_ref().and_then(ImplBrowser::host) {
            emulate_theme(&host, theme);
        }
    }

    fn input(&mut self, view: ViewId, input: WidgetInput) {
        let Some(open) = self.open.get_mut(&view) else {
            return;
        };
        let Some(host) = open.browser.as_ref().and_then(ImplBrowser::host) else {
            return;
        };
        send_input(&host, &mut open.buttons, &mut open.pointer, input);
    }

    pub(crate) fn close(&mut self, view: ViewId) {
        let Some(open) = self.open.remove(&view) else {
            return;
        };
        self.forget_page(view);
        if let Some(browser) = &open.browser {
            close_browser(browser);
        }
    }
}

fn close_browser(browser: &Browser) {
    if let Some(host) = browser.host() {
        host.close_browser(1);
    }
}

/// The value of the `prefers-color-scheme` media feature a page in `theme` sees.
fn color_scheme(theme: WidgetTheme) -> &'static str {
    match theme {
        WidgetTheme::Light => "light",
        WidgetTheme::Dark => "dark",
    }
}

/// Lets every frame of `host`'s browser see `theme` as its `prefers-color-scheme`, through
/// the `DevTools` method `Emulation.setEmulatedMedia` with the parameters
/// `{"features":[{"name":"prefers-color-scheme","value":"light"|"dark"}]}`.
fn emulate_theme(host: &BrowserHost, theme: WidgetTheme) {
    let (Some(mut params), Some(mut features), Some(mut feature)) = (
        dictionary_value_create(),
        list_value_create(),
        dictionary_value_create(),
    ) else {
        tracing::warn!("CEF could not make the parameters of a theme change");
        return;
    };
    feature.set_string(
        Some(&CefString::from("name")),
        Some(&CefString::from("prefers-color-scheme")),
    );
    feature.set_string(
        Some(&CefString::from("value")),
        Some(&CefString::from(color_scheme(theme))),
    );
    features.set_dictionary(0, Some(&mut feature));
    params.set_list(Some(&CefString::from("features")), Some(&mut features));
    // Message number 0 lets CEF number it. The answer, the number or 0 for a failure, is of no
    // use: a theme that did not take shows as a wrong picture, and nothing can be done about it.
    let _ = host.execute_dev_tools_method(
        0,
        Some(&CefString::from("Emulation.setEmulatedMedia")),
        Some(&mut params),
    );
}

/// Runs the proxy page's delivery function with `json` in the main frame of `browser`.
fn deliver(browser: &Browser, json: &str) {
    if let Some(frame) = browser.main_frame() {
        let code = format!(
            "window.__partiturDeliver && window.__partiturDeliver({});",
            js_string(json)
        );
        frame.execute_java_script(Some(&CefString::from(code.as_str())), None, 0);
    }
}

/// `text` as a JavaScript string literal.
fn js_string(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for c in text.chars() {
        match c {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{2028}' || c == '\u{2029}' || c == '<' => {
                let _ = write!(literal, "\\u{:04x}", u32::from(c));
            }
            c => literal.push(c),
        }
    }
    literal.push('"');
    literal
}

/// A pointer position in logical view pixels, rounded to CEF's whole pixels.
#[expect(
    clippy::cast_possible_truncation,
    reason = "an `as` cast from f32 saturates, which is what an off-view position needs"
)]
fn pixel(value: f32) -> c_int {
    value.round() as c_int
}

/// The amount to hand CEF on Windows for a scroll of `pixels`, when one notch of the wheel
/// scrolls `per_notch` lines (or characters) there. CEF scrolls `amount / 120 * per_notch *
/// 100 / 3` pixels, cut towards zero; the amount chosen puts that half a pixel past `pixels`,
/// away from zero, so that the cut lands on `pixels` whatever the float arithmetic's error. It is
/// exact while a notch scrolls at most three lines, 100 pixels; beyond that each amount scrolls
/// more than a pixel, and the scroll is the nearest CEF can make. With no lines per notch CEF
/// scrolls by nothing, whatever it is handed.
#[cfg(any(windows, test))]
fn notch_units(pixels: c_int, per_notch: u32) -> c_int {
    if pixels == 0 || per_notch == 0 {
        return pixels;
    }
    // `amount = (|pixels| + 1/2) * 360 / (100 * per_notch)`, rounded to the nearest whole.
    let lines = i64::from(per_notch);
    let numerator = (2 * i64::from(pixels.unsigned_abs()) + 1) * 18 + 5 * lines;
    let amount = numerator / (10 * lines);
    c_int::try_from(amount)
        .unwrap_or(c_int::MAX)
        .saturating_mul(pixels.signum())
}

/// The wheel lines and characters one notch scrolls, as the user set them in Windows, each the
/// system's default of 3 where it cannot be read.
#[cfg(windows)]
fn wheel_settings() -> (u32, u32) {
    let desktop = windows_registry::CURRENT_USER.open(r"Control Panel\Desktop");
    let setting = |name: &str| {
        desktop
            .as_ref()
            .ok()
            .and_then(|key| key.get_string(name).ok())
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(3)
    };
    (setting("WheelScrollLines"), setting("WheelScrollChars"))
}

/// The amounts to hand CEF for a scroll of `dx` and `dy` logical pixels (see the module
/// documentation).
#[cfg(windows)]
fn wheel_amounts(dx: f32, dy: f32) -> (c_int, c_int) {
    let (lines, chars) = wheel_settings();
    (notch_units(pixel(dx), chars), notch_units(pixel(dy), lines))
}

/// The amounts to hand CEF for a scroll of `dx` and `dy` logical pixels: the pixels themselves.
#[cfg(not(windows))]
fn wheel_amounts(dx: f32, dy: f32) -> (c_int, c_int) {
    (pixel(dx), pixel(dy))
}

/// The bits of CEF's event flag `flag`, as a [`MouseEvent`]'s modifiers hold them. The
/// Windows bindings give the flags as `i32`, the others as `u32`.
#[cfg(windows)]
fn flag(flag: cef_event_flags_t) -> u32 {
    flag.0.cast_unsigned()
}

/// The bits of CEF's event flag `flag`, as a [`MouseEvent`]'s modifiers hold them.
#[cfg(not(windows))]
fn flag(flag: cef_event_flags_t) -> u32 {
    flag.0
}

/// CEF's button and its modifier flag for the button number of [`WidgetInput`].
fn button(number: u8) -> Option<(MouseButtonType, u32)> {
    match number {
        0 => Some((
            MouseButtonType::LEFT,
            flag(cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON),
        )),
        1 => Some((
            MouseButtonType::MIDDLE,
            flag(cef_event_flags_t::EVENTFLAG_MIDDLE_MOUSE_BUTTON),
        )),
        2 => Some((
            MouseButtonType::RIGHT,
            flag(cef_event_flags_t::EVENTFLAG_RIGHT_MOUSE_BUTTON),
        )),
        _ => None,
    }
}

/// The modifiers of a wheel event with `buttons` held: precise amounts, as a touchpad gives
/// them, are marked so, and Chromium scrolls by them exactly instead of animating a notch.
fn wheel_modifiers(buttons: u32, precise: bool) -> u32 {
    if precise {
        buttons | flag(cef_event_flags_t::EVENTFLAG_PRECISION_SCROLLING_DELTA)
    } else {
        buttons
    }
}

/// Forwards one pointer event, keeping the buttons held and the last position up to date.
fn send_input(
    host: &BrowserHost,
    buttons: &mut u32,
    pointer: &mut (c_int, c_int),
    input: WidgetInput,
) {
    let at = |x: f32, y: f32, modifiers: u32| MouseEvent {
        x: pixel(x),
        y: pixel(y),
        modifiers,
    };
    match input {
        WidgetInput::Move { x, y } => {
            let event = at(x, y, *buttons);
            *pointer = (event.x, event.y);
            host.send_mouse_move_event(Some(&event), 0);
        }
        WidgetInput::Press {
            x,
            y,
            button: number,
        } => {
            let Some((kind, flag)) = button(number) else {
                return;
            };
            *buttons |= flag;
            let event = at(x, y, *buttons);
            *pointer = (event.x, event.y);
            host.send_mouse_click_event(Some(&event), kind, 0, 1);
        }
        WidgetInput::Release {
            x,
            y,
            button: number,
        } => {
            let Some((kind, flag)) = button(number) else {
                return;
            };
            let event = at(x, y, *buttons);
            *buttons &= !flag;
            *pointer = (event.x, event.y);
            host.send_mouse_click_event(Some(&event), kind, 1, 1);
        }
        WidgetInput::Wheel {
            x,
            y,
            dx,
            dy,
            precise,
        } => {
            let event = at(x, y, wheel_modifiers(*buttons, precise));
            *pointer = (event.x, event.y);
            let (dx, dy) = wheel_amounts(dx, dy);
            host.send_mouse_wheel_event(Some(&event), dx, dy);
        }
        WidgetInput::Leave => {
            let event = MouseEvent {
                x: pointer.0,
                y: pointer.1,
                modifiers: *buttons,
            };
            host.send_mouse_move_event(Some(&event), 1);
        }
    }
}

/// The look of the pointer the UI shows for CEF's cursor `cursor`.
fn cursor_of(cursor: CursorType) -> WidgetCursor {
    match cursor {
        CursorType::HAND => WidgetCursor::Pointer,
        CursorType::IBEAM | CursorType::VERTICALTEXT => WidgetCursor::Text,
        CursorType::GRAB => WidgetCursor::Grab,
        CursorType::GRABBING => WidgetCursor::Grabbing,
        CursorType::NOTALLOWED | CursorType::NODROP => WidgetCursor::NotAllowed,
        CursorType::EASTRESIZE
        | CursorType::WESTRESIZE
        | CursorType::EASTWESTRESIZE
        | CursorType::COLUMNRESIZE => WidgetCursor::ResizeHorizontal,
        CursorType::NORTHRESIZE
        | CursorType::SOUTHRESIZE
        | CursorType::NORTHSOUTHRESIZE
        | CursorType::ROWRESIZE => WidgetCursor::ResizeVertical,
        CursorType::CROSS => WidgetCursor::Crosshair,
        _ => WidgetCursor::Default,
    }
}

/// How many frames `frame` is nested below the main frame: 0 for the proxy page, 1 for the
/// widget's document.
fn frame_depth(frame: &Frame) -> usize {
    let mut depth = 0;
    let mut parent = frame.parent();
    while let Some(frame) = parent {
        depth += 1;
        parent = frame.parent();
    }
    depth
}

/// Whether a load error at `depth` breaks the view. Only the proxy page and the widget's own
/// document do; a frame or image the widget nests in turn may fail without taking it down.
fn fails_the_view(depth: usize) -> bool {
    depth <= 1
}

/// The view's rectangle in logical pixels, at least one pixel each way.
fn view_rect(geometry: Geometry) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: c_int::try_from(geometry.width.max(1)).unwrap_or(c_int::MAX),
        height: c_int::try_from(geometry.height.max(1)).unwrap_or(c_int::MAX),
    }
}

/// The part of CEF's dirty rectangle `rect` that lies right of and below the view's top-left
/// corner, as a region of its picture.
fn region(rect: &Rect) -> Region {
    let part = |from: c_int, length: c_int| {
        let end = i64::from(from) + i64::from(length);
        let from = from.max(0);
        (
            u32::try_from(from).unwrap_or(0),
            u32::try_from(end - i64::from(from)).unwrap_or(0),
        )
    };
    let (x, width) = part(rect.x, rect.width);
    let (y, height) = part(rect.y, rect.height);
    Region {
        x,
        y,
        width,
        height,
    }
}

/// Paints what CEF passed to `on_paint` into `latest`: the rows of `dirty_rects`, or of the whole
/// view when it named none. Returns whether the writer must be woken for it; a paint without a
/// usable buffer is dropped.
///
/// `buffer`, `width` and `height` must be exactly what CEF passed to `on_paint`, and this must be
/// called during that call.
fn paint(
    latest: &LatestFrame,
    buffer: *const u8,
    width: c_int,
    height: c_int,
    dirty_rects: Option<&[Rect]>,
) -> bool {
    if buffer.is_null() {
        return false;
    }
    let (Some(width), Some(height)) = (
        u32::try_from(width).ok().filter(|width| *width > 0),
        u32::try_from(height).ok().filter(|height| *height > 0),
    ) else {
        return false;
    };
    let Some(len) = usize::try_from(width)
        .ok()
        .zip(usize::try_from(height).ok())
        .and_then(|(width, height)| width.checked_mul(height)?.checked_mul(4))
    else {
        return false;
    };
    // SAFETY: CEF documents the `on_paint` buffer as `width * height * 4` bytes of BGRA that it
    // owns and keeps valid for the length of the call. The pointer was checked for null and the
    // length computed without overflow from the positive width and height CEF passed with it, and
    // the slice does not outlive this function, which runs inside `on_paint`.
    #[expect(
        unsafe_code,
        reason = "CEF owns the paint buffer for the duration of `on_paint`, and its length is \
                  width * height * 4"
    )]
    let bgra = unsafe { std::slice::from_raw_parts(buffer, len) };
    let whole = dirty_rects.is_none().then(|| Region::whole(width, height));
    let dirty = dirty_rects.into_iter().flatten().map(region).chain(whole);
    latest.paint(width, height, bgra, dirty)
}

wrap_render_handler! {
    struct ViewRender {
        view: ViewId,
        shared: Arc<Shared>,
        geometry: SharedGeometry,
        latest: LatestFrame,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let Some(rect) = rect else {
                return;
            };
            *rect = view_rect(geometry(&self.geometry));
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> c_int {
            let Some(screen_info) = screen_info else {
                return 0;
            };
            let geometry = geometry(&self.geometry);
            screen_info.device_scale_factor = geometry.scale;
            screen_info.depth = 24;
            screen_info.depth_per_component = 8;
            screen_info.rect = view_rect(geometry);
            screen_info.available_rect = view_rect(geometry);
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: c_int,
            height: c_int,
        ) {
            if type_ == PaintElementType::VIEW
                && paint(&self.latest, buffer, width, height, dirty_rects)
            {
                self.shared.painted(self.view, self.latest.clone());
            }
        }
    }
}

wrap_load_handler! {
    struct ViewLoad {
        view: ViewId,
        generation: u64,
        shared: Arc<Shared>,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: c_int,
        ) {
            if frame.is_some_and(|frame| frame.is_main() == 1) {
                self.shared
                    .commands
                    .send(Command::Loaded(self.view, self.generation));
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            let depth = frame.map_or(0, |frame| frame_depth(frame));
            if error_code == Errorcode::ABORTED || !fails_the_view(depth) {
                return;
            }
            let text = error_text.map(ToString::to_string).unwrap_or_default();
            let url = failed_url.map(ToString::to_string).unwrap_or_default();
            self.shared
                .fail(self.view, format!("the widget did not load ({url}): {text}"));
        }
    }
}

wrap_life_span_handler! {
    struct ViewLifeSpan {
        view: ViewId,
        generation: u64,
        shared: Arc<Shared>,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                self.shared
                    .owners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(browser.identifier(), self.view);
                self.shared.commands.send(Command::Created(
                    self.view,
                    self.generation,
                    browser.clone(),
                ));
            }
        }

        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            user_gesture: c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut c_int>,
        ) -> c_int {
            self.shared
                .hand_on(self.view, target_url.map(ToString::to_string), user_gesture);
            // Cancelled: a view never opens a window.
            1
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                self.shared
                    .owners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&browser.identifier());
                self.shared
                    .commands
                    .send(Command::Closed(browser.identifier()));
            }
        }
    }
}

wrap_resource_request_handler! {
    struct ViewResourceRequests {
        view: ViewId,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            let Some(request) = request else {
                return ReturnValue::CANCEL;
            };
            let url = CefString::from(&request.url()).to_string();
            // A request of no frame counts as one from deep down: it gets no Partitur scheme
            // that is the main frame's alone.
            let depth = frame.map_or(usize::MAX, |frame| frame_depth(frame));
            if scheme::permitted(&url, self.view, depth) {
                ReturnValue::CONTINUE
            } else {
                tracing::debug!(view = self.view.0, %url, "a widget request was denied");
                ReturnValue::CANCEL
            }
        }
    }
}

wrap_request_handler! {
    struct ViewRequests {
        view: ViewId,
        shared: Arc<Shared>,
        resources: ResourceRequestHandler,
    }

    impl RequestHandler {
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _is_navigation: c_int,
            _is_download: c_int,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut c_int>,
        ) -> Option<ResourceRequestHandler> {
            Some(self.resources.clone())
        }

        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            user_gesture: c_int,
            is_redirect: c_int,
        ) -> c_int {
            let url = request.map(|request| CefString::from(&request.url()).to_string());
            let depth = frame.map_or(0, |frame| frame_depth(frame));
            match url {
                Some(url) if scheme::may_navigate(&url, self.view, depth) => 0,
                url => {
                    tracing::debug!(
                        view = self.view.0,
                        ?url,
                        depth,
                        "a widget navigation was stopped"
                    );
                    if is_redirect == 0 {
                        self.shared.hand_on(self.view, url, user_gesture);
                    }
                    1
                }
            }
        }

        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            user_gesture: c_int,
        ) -> c_int {
            self.shared
                .hand_on(self.view, target_url.map(ToString::to_string), user_gesture);
            // Cancelled: a view never opens a tab.
            1
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            error_code: c_int,
            error_string: Option<&CefString>,
        ) {
            let text = error_string.map(ToString::to_string).unwrap_or_default();
            self.shared.fail(
                self.view,
                format!("the widget's renderer stopped ({error_code}): {text}"),
            );
        }
    }
}

wrap_display_handler! {
    struct ViewDisplay {
        view: ViewId,
        shared: Arc<Shared>,
    }

    impl DisplayHandler {
        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: cef::CursorHandle,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> c_int {
            self.shared.send(FromEngine::Cursor {
                view: self.view,
                cursor: cursor_of(type_),
            });
            // Handled: a windowless browser has no window of its own to set the cursor on.
            1
        }
    }
}

wrap_client! {
    struct ViewClient {
        render: RenderHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        requests: RequestHandler,
        display: DisplayHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.requests.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    #[test]
    fn only_the_proxy_and_the_widget_document_fail_the_view() {
        assert!(fails_the_view(0));
        assert!(fails_the_view(1));
        assert!(!fails_the_view(2));
        assert!(!fails_the_view(5));
    }

    fn rect(x: c_int, y: c_int, width: c_int, height: c_int) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn at(x: u32, y: u32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn dirty_rectangles_become_regions_from_the_corner_on() {
        assert_eq!(region(&rect(1, 2, 3, 4)), at(1, 2, 3, 4));
        assert_eq!(region(&rect(-2, 1, 5, 1)), at(0, 1, 3, 1));
        assert_eq!(region(&rect(1, -3, 1, 2)), at(1, 0, 1, 0));
        assert_eq!(region(&rect(-1, 0, 0, -4)), at(0, 0, 0, 0));
        assert_eq!(
            region(&rect(0, c_int::MAX, 1, c_int::MAX)),
            at(
                0,
                u32::try_from(c_int::MAX).unwrap(),
                1,
                u32::try_from(c_int::MAX).unwrap()
            )
        );
    }

    #[test]
    fn a_paint_without_a_usable_buffer_is_dropped() {
        let latest = LatestFrame::default();
        let pixels = [7_u8; 16];
        assert!(!paint(&latest, std::ptr::null(), 2, 2, None));
        assert!(!paint(&latest, pixels.as_ptr(), 0, 2, None));
        assert!(!paint(&latest, pixels.as_ptr(), 2, -1, None));
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn a_paint_without_dirty_rectangles_is_read_in_full() {
        let latest = LatestFrame::default();
        let pixels: Vec<u8> = (0..16).collect();
        assert!(paint(&latest, pixels.as_ptr(), 2, 2, None));
        assert_eq!(latest.picture().bgra, pixels);
        assert_eq!(
            latest.take().map(|damage| damage.regions().to_vec()),
            Some(vec![at(0, 0, 2, 2)])
        );
    }

    #[test]
    fn a_paint_copies_only_its_dirty_rows_cut_off_at_the_view() {
        let latest = LatestFrame::default();
        let first = [1_u8; 36];
        assert!(paint(&latest, first.as_ptr(), 3, 3, Some(&[])));
        assert_eq!(
            latest.picture().bgra,
            [0; 36],
            "a new size starts transparent"
        );
        latest.take();

        let pixels: Vec<u8> = (0..36).collect();
        assert!(paint(
            &latest,
            pixels.as_ptr(),
            3,
            3,
            Some(&[rect(-1, 1, 2, 1), rect(2, 2, 5, 5)])
        ));
        let mut expected = [0_u8; 36];
        expected[12..16].copy_from_slice(&pixels[12..16]);
        expected[32..36].copy_from_slice(&pixels[32..36]);
        assert_eq!(latest.picture().bgra, expected);
        assert_eq!(
            latest.take().map(|damage| damage.regions().to_vec()),
            Some(vec![at(0, 1, 1, 1), at(2, 2, 1, 1)])
        );
    }

    #[test]
    fn buttons_map_to_cef_buttons() {
        assert_eq!(button(0).map(|(_, flag)| flag), Some(16));
        assert_eq!(button(1).map(|(_, flag)| flag), Some(32));
        assert_eq!(button(2).map(|(_, flag)| flag), Some(64));
        assert!(button(0).is_some_and(|(kind, _)| kind == MouseButtonType::LEFT));
        assert!(button(1).is_some_and(|(kind, _)| kind == MouseButtonType::MIDDLE));
        assert!(button(2).is_some_and(|(kind, _)| kind == MouseButtonType::RIGHT));
        assert!(button(3).is_none());
    }

    /// What CEF on Windows scrolls for `amount` at `per_notch` lines a notch, as
    /// `GetUiWheelEventOffset` in its `browser_platform_delegate_native_win.cc` computes it: in
    /// f32, cut towards zero by `gfx::Vector2d`.
    #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn cef_windows_scroll(amount: c_int, per_notch: u32) -> c_int {
        let pixels_per_line = 100.0_f32 / 3.0;
        let notches = amount as f32 / 120.0;
        (notches * (per_notch as f32 * pixels_per_line)) as c_int
    }

    // At up to three lines a notch, every amount of pixels comes out of CEF as itself; the
    // numbers Windows CI saw without the conversion were 50 scrolling 41 and 3 scrolling 2.
    #[test]
    fn windows_wheel_amounts_scroll_by_their_pixels() {
        assert_eq!(cef_windows_scroll(50, 3), 41);
        assert_eq!(cef_windows_scroll(3, 3), 2);
        for per_notch in 1..=3 {
            for pixels in -1000..=1000 {
                let amount = notch_units(pixels, per_notch);
                assert_eq!(
                    cef_windows_scroll(amount, per_notch),
                    pixels,
                    "{pixels} px at {per_notch} lines a notch went out as {amount}"
                );
            }
        }
        assert_eq!(notch_units(50, 3), 61);
        assert_eq!(notch_units(-50, 3), -61);
        assert_eq!(notch_units(0, 3), 0);
        assert_eq!(notch_units(7, 0), 7);
    }

    // Past three lines a notch CEF cannot scroll by every pixel; it scrolls by the nearest it can.
    #[test]
    fn many_lines_a_notch_scroll_by_the_nearest_amount() {
        for per_notch in [4, 6, 10, 100] {
            let step = c_int::try_from(per_notch * 100 / 360 + 1).unwrap();
            for pixels in -1000..=1000 {
                let scrolled = cef_windows_scroll(notch_units(pixels, per_notch), per_notch);
                assert!(
                    (scrolled - pixels).abs() <= step,
                    "{pixels} px at {per_notch} lines scrolled {scrolled}"
                );
            }
        }
        assert_eq!(notch_units(c_int::MAX, 1), c_int::MAX);
        assert_eq!(notch_units(c_int::MIN, 1), -c_int::MAX);
    }

    #[test]
    fn precise_wheel_amounts_are_marked_so() {
        assert_eq!(wheel_modifiers(16, false), 16);
        assert_eq!(wheel_modifiers(16, true), 16 | 16384);
        assert_eq!(wheel_modifiers(0, true), 16384);
    }

    #[test]
    fn cef_cursors_map_to_the_looks_the_ui_shows() {
        let looks = [
            (CursorType::POINTER, WidgetCursor::Default),
            (CursorType::HAND, WidgetCursor::Pointer),
            (CursorType::IBEAM, WidgetCursor::Text),
            (CursorType::VERTICALTEXT, WidgetCursor::Text),
            (CursorType::GRAB, WidgetCursor::Grab),
            (CursorType::GRABBING, WidgetCursor::Grabbing),
            (CursorType::NOTALLOWED, WidgetCursor::NotAllowed),
            (CursorType::NODROP, WidgetCursor::NotAllowed),
            (CursorType::EASTRESIZE, WidgetCursor::ResizeHorizontal),
            (CursorType::WESTRESIZE, WidgetCursor::ResizeHorizontal),
            (CursorType::EASTWESTRESIZE, WidgetCursor::ResizeHorizontal),
            (CursorType::COLUMNRESIZE, WidgetCursor::ResizeHorizontal),
            (CursorType::NORTHRESIZE, WidgetCursor::ResizeVertical),
            (CursorType::SOUTHRESIZE, WidgetCursor::ResizeVertical),
            (CursorType::NORTHSOUTHRESIZE, WidgetCursor::ResizeVertical),
            (CursorType::ROWRESIZE, WidgetCursor::ResizeVertical),
            (CursorType::CROSS, WidgetCursor::Crosshair),
            (CursorType::WAIT, WidgetCursor::Default),
            (CursorType::CUSTOM, WidgetCursor::Default),
        ];
        for (cursor, look) in looks {
            assert_eq!(cursor_of(cursor), look, "{cursor:?}");
        }
    }

    #[test]
    fn positions_round_to_whole_pixels() {
        assert_eq!(pixel(1.4), 1);
        assert_eq!(pixel(1.5), 2);
        assert_eq!(pixel(-0.6), -1);
        assert_eq!(pixel(f32::MAX), c_int::MAX);
    }

    #[test]
    fn themes_become_color_schemes() {
        assert_eq!(color_scheme(WidgetTheme::Light), "light");
        assert_eq!(color_scheme(WidgetTheme::Dark), "dark");
    }

    #[test]
    fn json_becomes_a_safe_string_literal() {
        assert_eq!(js_string(r#"{"a":"b\c"}"#), r#""{\"a\":\"b\\c\"}""#);
        assert_eq!(
            js_string("a\nb\u{2028}</script>"),
            "\"a\\nb\\u2028\\u003c/script>\""
        );
        assert_eq!(js_string("\u{1}"), "\"\\u0001\"");
    }
}
