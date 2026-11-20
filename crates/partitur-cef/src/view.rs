//! One windowless CEF browser per widget view, and the commands that drive them.
//!
//! [`Views`] lives on the `cef-main` thread and carries out every [`Command`]: it opens a browser
//! on the view's proxy page, resizes it, hides it, forwards pointer input and delivers messages
//! into the page, and closes it again. CEF reports back through the handlers of each view's
//! client, on its own threads: paints become [`WidgetHostEvent::Frame`]s in RGBA, a load error or
//! a renderer that stopped becomes [`WidgetHostEvent::Failed`], and the creation, load and close
//! of a browser come back to [`Views`] as commands.
//!
//! A view's size is logical pixels at a device scale factor; CEF learns the scale through the
//! screen info and paints at logical size times scale. Messages posted before the proxy page has
//! loaded wait for it. Every request a view's browser makes passes [`scheme::permitted`] first.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::os::raw::c_int;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use cef::rc::Rc as _;
use cef::sys::cef_event_flags_t;
use cef::{
    Browser, BrowserHost, BrowserSettings, Callback, CefString, Client, Errorcode, Frame,
    ImplBrowser, ImplBrowserHost, ImplClient, ImplFrame, ImplLifeSpanHandler, ImplLoadHandler,
    ImplRenderHandler, ImplRequest, ImplRequestHandler, ImplResourceRequestHandler,
    LifeSpanHandler, LoadHandler, MouseButtonType, MouseEvent, PaintElementType, Rect,
    RenderHandler, Request, RequestHandler, ResourceRequestHandler, ReturnValue, RuntimeStyle,
    ScreenInfo, TerminationStatus, WindowInfo, WrapClient, WrapLifeSpanHandler, WrapLoadHandler,
    WrapRenderHandler, WrapRequestHandler, WrapResourceRequestHandler, browser_host_create_browser,
    wrap_client, wrap_life_span_handler, wrap_load_handler, wrap_render_handler,
    wrap_request_handler, wrap_resource_request_handler,
};
use futures::channel::mpsc::UnboundedSender;
use partitur_core::widget_host::{ViewId, WidgetHostEvent, WidgetInput, WidgetPage};

use crate::scheme::{self, Pages};

/// The size a view has before its first resize, in logical pixels.
const DEFAULT_SIZE: (u32, u32) = (300, 150);

/// How often a view repaints at most, per second.
const FRAME_RATE: c_int = 30;

/// What the `cef-main` thread is asked to do, by the host or by CEF's handlers.
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
    /// Closes a view.
    Close(ViewId),
    /// CEF created the browser of one opening of a view.
    Created(ViewId, u64, Browser),
    /// The proxy page of one opening of a view has loaded.
    Loaded(ViewId, u64),
    /// CEF is about to destroy the browser with this identifier.
    Closed(c_int),
    /// CEF asks for its message loop to run in this many milliseconds (external pump only).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Pump(i64),
    /// Closes everything and shuts CEF down.
    Shutdown,
}

/// What every view's handlers share.
pub(crate) struct Shared {
    /// Where the host's events go.
    pub(crate) events: UnboundedSender<WidgetHostEvent>,
    /// The way back to the `cef-main` thread.
    pub(crate) commands: Sender<Command>,
    /// The pages the scheme handlers serve.
    pub(crate) pages: Pages,
}

impl Shared {
    fn send(&self, event: WidgetHostEvent) {
        let _ = self.events.unbounded_send(event);
    }

    fn fail(&self, view: ViewId, error: String) {
        self.send(WidgetHostEvent::Failed { view, error });
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

/// The `cef-main` thread's record of one open view.
struct View {
    generation: u64,
    geometry: SharedGeometry,
    browser: Option<Browser>,
    loaded: bool,
    posts: Vec<String>,
    hidden: bool,
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

    /// Carries out one command. [`Command::Shutdown`] and [`Command::Pump`] belong to the loop
    /// that calls this, and are ignored here.
    pub(crate) fn handle(&mut self, command: Command) {
        match command {
            Command::Open(view, page) => self.open(view, page),
            Command::Post(view, json) => self.post(view, json),
            Command::Resize(view, width, height, scale) => self.resize(view, width, height, scale),
            Command::SetVisible(view, visible) => self.set_visible(view, visible),
            Command::Input(view, input) => self.input(view, input),
            Command::Close(view) => self.close(view),
            Command::Created(view, generation, browser) => self.created(view, generation, browser),
            Command::Loaded(view, generation) => self.loaded(view, generation),
            Command::Closed(id) => {
                self.live.remove(&id);
            }
            Command::Pump(_) | Command::Shutdown => {}
        }
    }

    fn open(&mut self, view: ViewId, page: WidgetPage) {
        self.close(view);
        self.generation += 1;
        let generation = self.generation;
        self.shared
            .pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(view, page);
        let geometry = SharedGeometry::default();
        let mut client = ViewClient::new(
            ViewRender::new(view, self.shared.clone(), geometry.clone()),
            ViewLoad::new(view, generation, self.shared.clone()),
            ViewLifeSpan::new(view, generation, self.shared.clone()),
            ViewRequests::new(view, self.shared.clone(), ViewResourceRequests::new(view)),
        );
        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
            runtime_style: RuntimeStyle::ALLOY,
            ..WindowInfo::default()
        };
        let settings = BrowserSettings {
            windowless_frame_rate: FRAME_RATE,
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
                // A resize that came before the browser existed is applied now.
                if let Some(host) = browser.host() {
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

    fn input(&mut self, view: ViewId, input: WidgetInput) {
        let Some(open) = self.open.get_mut(&view) else {
            return;
        };
        let Some(host) = open.browser.as_ref().and_then(ImplBrowser::host) else {
            return;
        };
        send_input(&host, &mut open.buttons, &mut open.pointer, input);
    }

    fn close(&mut self, view: ViewId) {
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

/// CEF's button and its modifier flag for the button number of [`WidgetInput`].
fn button(number: u8) -> Option<(MouseButtonType, u32)> {
    match number {
        0 => Some((
            MouseButtonType::LEFT,
            cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON.0,
        )),
        1 => Some((
            MouseButtonType::MIDDLE,
            cef_event_flags_t::EVENTFLAG_MIDDLE_MOUSE_BUTTON.0,
        )),
        2 => Some((
            MouseButtonType::RIGHT,
            cef_event_flags_t::EVENTFLAG_RIGHT_MOUSE_BUTTON.0,
        )),
        _ => None,
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
        WidgetInput::Wheel { x, y, dx, dy } => {
            let event = at(x, y, *buttons);
            *pointer = (event.x, event.y);
            host.send_mouse_wheel_event(Some(&event), pixel(dx), pixel(dy));
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

/// The view's rectangle in logical pixels, at least one pixel each way.
fn view_rect(geometry: Geometry) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width: c_int::try_from(geometry.width.max(1)).unwrap_or(c_int::MAX),
        height: c_int::try_from(geometry.height.max(1)).unwrap_or(c_int::MAX),
    }
}

/// BGRA pixels, as CEF paints them, turned into RGBA.
fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        if let &[b, g, r, a] = pixel {
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    rgba
}

/// The width, height and RGBA pixels of a paint, or `None` when CEF passed no usable buffer.
///
/// `buffer`, `width` and `height` must be exactly what CEF passed to `on_paint`, and this must be
/// called during that call.
fn paint_to_rgba(buffer: *const u8, width: c_int, height: c_int) -> Option<(u32, u32, Vec<u8>)> {
    if buffer.is_null() {
        return None;
    }
    let width = u32::try_from(width).ok().filter(|width| *width > 0)?;
    let height = u32::try_from(height).ok().filter(|height| *height > 0)?;
    let len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
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
    Some((width, height, bgra_to_rgba(bgra)))
}

wrap_render_handler! {
    struct ViewRender {
        view: ViewId,
        shared: Arc<Shared>,
        geometry: SharedGeometry,
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
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: c_int,
            height: c_int,
        ) {
            if type_ != PaintElementType::VIEW {
                return;
            }
            if let Some((width, height, rgba)) = paint_to_rgba(buffer, width, height) {
                self.shared.send(WidgetHostEvent::Frame {
                    view: self.view,
                    width,
                    height,
                    rgba: Arc::new(rgba),
                });
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
                let _ = self
                    .shared
                    .commands
                    .send(Command::Loaded(self.view, self.generation));
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if error_code == Errorcode::ABORTED {
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
                let _ = self.shared.commands.send(Command::Created(
                    self.view,
                    self.generation,
                    browser.clone(),
                ));
            }
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                let _ = self
                    .shared
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
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            let Some(request) = request else {
                return ReturnValue::CANCEL;
            };
            let url = CefString::from(&request.url()).to_string();
            if scheme::permitted(&url, self.view) {
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

wrap_client! {
    struct ViewClient {
        render: RenderHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        requests: RequestHandler,
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
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn bgra_becomes_rgba() {
        assert_eq!(
            bgra_to_rgba(&[1, 2, 3, 4, 10, 20, 30, 40]),
            vec![3, 2, 1, 4, 30, 20, 10, 40]
        );
        assert_eq!(bgra_to_rgba(&[1, 2, 3]), Vec::<u8>::new());
    }

    #[test]
    fn a_paint_without_a_usable_buffer_is_dropped() {
        let pixels = [0_u8; 16];
        assert_eq!(paint_to_rgba(std::ptr::null(), 2, 2), None);
        assert_eq!(paint_to_rgba(pixels.as_ptr(), 0, 2), None);
        assert_eq!(paint_to_rgba(pixels.as_ptr(), 2, -1), None);
    }

    #[test]
    fn a_paint_is_read_in_full() {
        let pixels: Vec<u8> = (0..16).collect();
        let (width, height, rgba) = paint_to_rgba(pixels.as_ptr(), 2, 2).unwrap();
        assert_eq!((width, height), (2, 2));
        assert_eq!(rgba, bgra_to_rgba(&pixels));
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

    #[test]
    fn positions_round_to_whole_pixels() {
        assert_eq!(pixel(1.4), 1);
        assert_eq!(pixel(1.5), 2);
        assert_eq!(pixel(-0.6), -1);
        assert_eq!(pixel(f32::MAX), c_int::MAX);
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
