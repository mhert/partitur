//! The two Partitur schemes and the rule for which requests a widget view may make.
//!
//! Both schemes are registered as standard, secure, CORS-enabled and fetch-enabled (without the
//! last, CEF hands the scheme handler no request body). The host part of every URL names the view
//! it belongs to as `view-<n>`, so each view is an origin of its own; a bare number would not do,
//! because the host of a standard scheme is canonicalized and `1` becomes the address `0.0.0.1`.
//!
//! `partitur-widget://view-<n>/` serves the sandbox proxy page, and a POST to
//! `partitur-widget://view-<n>/bridge` carries one message from the widget, which becomes a
//! [`WidgetHostEvent::Message`]. `partitur-app://view-<n>/` serves the widget's HTML under the
//! `Content-Security-Policy` header taken from its [`WidgetPage`]. Every other URL of the two
//! schemes answers 404.
//!
//! Requests outside the two schemes are left to the page's policy, but only `http`, `https` and
//! `data` are let through at all; [`permitted`] is the check the request handler applies.
//!
//! Navigations are held tighter ([`may_navigate`]): the main frame may only ever show the view's
//! proxy page and the widget's frame only the view's widget document, so neither can be
//! replaced by another page. Frames the widget nests in its document navigate as its policy
//! lets them.

use std::collections::HashMap;
use std::os::raw::c_int;
use std::sync::{Arc, Mutex, PoisonError};

use cef::rc::Rc as _;
use cef::sys::cef_scheme_options_t;
use cef::wrapper::byte_read_handler::{ByteReadHandler, ByteStream};
use cef::wrapper::stream_resource_handler::StreamResourceHandler;
use cef::{
    Browser, CefString, CefStringMultimap, Frame, ImplBrowser, ImplPostData, ImplPostDataElement,
    ImplRequest, ImplSchemeHandlerFactory, ImplSchemeRegistrar, PostDataElement, Request,
    ResourceHandler, SchemeHandlerFactory, SchemeRegistrar, WrapSchemeHandlerFactory,
    stream_reader_create_for_handler, wrap_scheme_handler_factory,
};
use futures::channel::mpsc::UnboundedSender;
use partitur_core::widget_host::{ViewId, WidgetHostEvent, WidgetPage};

/// The scheme of the sandbox proxy page and of the bridge.
pub(crate) const WIDGET_SCHEME: &str = "partitur-widget";
/// The scheme of the widget's own document.
pub(crate) const APP_SCHEME: &str = "partitur-app";

/// The policy of the proxy page: its own inline script and style, the widget frame, and the
/// bridge on its own origin.
const PROXY_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; \
    style-src 'unsafe-inline'; frame-src partitur-app:; connect-src 'self'";

/// The sandbox proxy page.
const PROXY_HTML: &str = include_str!("../assets/proxy.html");

/// The schemes a widget view may request besides the two Partitur schemes.
const OPEN_SCHEMES: [&str; 3] = ["http", "https", "data"];

/// The pages of the open views, which the scheme handlers serve.
pub(crate) type Pages = Arc<Mutex<HashMap<ViewId, WidgetPage>>>;

/// The view each live browser belongs to, by the browser's identifier.
pub(crate) type Owners = Arc<Mutex<HashMap<c_int, ViewId>>>;

/// The URL of the proxy page of `view`, which a view's browser opens.
pub(crate) fn proxy_url(view: ViewId) -> String {
    format!("{WIDGET_SCHEME}://view-{}/", view.0)
}

/// Registers both schemes as standard, secure, CORS-enabled and fetch-enabled. Every CEF process
/// does this.
pub(crate) fn register(registrar: SchemeRegistrar) {
    let options = cef_scheme_options_t::CEF_SCHEME_OPTION_STANDARD as i32
        | cef_scheme_options_t::CEF_SCHEME_OPTION_SECURE as i32
        | cef_scheme_options_t::CEF_SCHEME_OPTION_CORS_ENABLED as i32
        | cef_scheme_options_t::CEF_SCHEME_OPTION_FETCH_ENABLED as i32;
    for scheme in [WIDGET_SCHEME, APP_SCHEME] {
        registrar.add_custom_scheme(Some(&CefString::from(scheme)), options);
    }
}

/// Installs the handlers of both schemes, and returns whether CEF took both.
pub(crate) fn install(
    pages: &Pages,
    owners: &Owners,
    events: &UnboundedSender<WidgetHostEvent>,
) -> bool {
    let mut factory = WidgetSchemes::new(pages.clone(), owners.clone(), events.clone());
    [WIDGET_SCHEME, APP_SCHEME].into_iter().all(|scheme| {
        cef::register_scheme_handler_factory(
            Some(&CefString::from(scheme)),
            None,
            Some(&mut factory),
        ) == 1
    })
}

/// What a request to one of the Partitur schemes asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// The proxy page of the view.
    Proxy(ViewId),
    /// One message from the view's widget.
    Bridge(ViewId),
    /// The widget's document.
    App(ViewId),
    /// Anything else.
    NotFound,
}

/// The scheme, the view and the path (without its leading `/`, query or fragment) of a URL of
/// one of the Partitur schemes.
fn split(url: &str) -> Option<(&str, ViewId, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let view = ViewId(host.strip_prefix("view-")?.parse().ok()?);
    let path = path.split(['?', '#']).next().unwrap_or_default();
    Some((scheme, view, path))
}

/// Where a request for `url` with `method` goes.
pub(crate) fn route(url: &str, method: &str) -> Route {
    match split(url) {
        Some((WIDGET_SCHEME, view, "")) if method == "GET" => Route::Proxy(view),
        Some((WIDGET_SCHEME, view, "bridge")) if method == "POST" => Route::Bridge(view),
        Some((APP_SCHEME, view, "")) if method == "GET" => Route::App(view),
        _ => Route::NotFound,
    }
}

/// `route`, kept only when the request comes from the browser of the view it names (`owner`).
/// The request handler already cancels requests for other views' origins; this keeps a view from
/// posting as another, or reading another's page, should one get past it.
pub(crate) fn owned(route: Route, owner: Option<ViewId>) -> Route {
    match route {
        Route::Proxy(view) | Route::Bridge(view) | Route::App(view) if owner == Some(view) => route,
        _ => Route::NotFound,
    }
}

/// Whether the browser of `view` may request `url`: `http`, `https` and `data` always (the page's
/// policy has its say on those), the two Partitur schemes only on the view's own origin, and
/// nothing else.
pub(crate) fn permitted(url: &str, view: ViewId) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    if OPEN_SCHEMES.contains(&scheme.as_str()) {
        return true;
    }
    match split(url) {
        Some((found, owner, _)) if found == WIDGET_SCHEME || found == APP_SCHEME => owner == view,
        _ => false,
    }
}

/// Whether a frame `depth` frames below the main frame of the browser of `view` may navigate to
/// `url`: the main frame (0) only to the view's proxy page, the widget's frame (1) only to the
/// view's widget document, and the frames the widget nests (2 and deeper) anywhere, as the
/// page's policy and [`permitted`] decide.
pub(crate) fn may_navigate(url: &str, view: ViewId, depth: usize) -> bool {
    match depth {
        0 => route(url, "GET") == Route::Proxy(view),
        1 => route(url, "GET") == Route::App(view),
        _ => true,
    }
}

/// A response of `status` with `mime` type, `headers` and `body`.
fn respond(
    status: i32,
    text: &str,
    mime: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> ResourceHandler {
    let header_map = (!headers.is_empty()).then(|| {
        let mut map = CefStringMultimap::new();
        for (name, value) in headers {
            map.append(name, value);
        }
        map
    });
    let stream = body.and_then(|bytes| {
        let mut reader = ByteReadHandler::new(Arc::new(Mutex::new(ByteStream::new(bytes))));
        stream_reader_create_for_handler(Some(&mut reader))
    });
    StreamResourceHandler::new(status, text.into(), mime.into(), header_map, stream)
}

fn not_found() -> ResourceHandler {
    respond(404, "Not Found", "text/plain", &[], None)
}

/// The bytes of a request's body, all its elements joined.
fn body(request: &Request) -> Vec<u8> {
    let Some(post_data) = request.post_data() else {
        return Vec::new();
    };
    let mut elements: Vec<Option<PostDataElement>> = vec![None; post_data.element_count()];
    post_data.elements(Some(&mut elements));
    let mut body = Vec::new();
    for element in elements.into_iter().flatten() {
        let mut bytes = vec![0; element.bytes_count()];
        let read = element.bytes(bytes.len(), bytes.as_mut_ptr());
        bytes.truncate(read);
        body.extend_from_slice(&bytes);
    }
    body
}

wrap_scheme_handler_factory! {
    struct WidgetSchemes {
        pages: Pages,
        owners: Owners,
        events: UnboundedSender<WidgetHostEvent>,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let request = request?;
            let url = CefString::from(&request.url()).to_string();
            let method = CefString::from(&request.method()).to_string();
            let owner = browser.and_then(|browser| {
                self.owners
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&browser.identifier())
                    .copied()
            });
            Some(match owned(route(&url, &method), owner) {
                Route::Proxy(_) => respond(
                    200,
                    "OK",
                    "text/html",
                    &[("Content-Security-Policy", PROXY_CSP)],
                    Some(PROXY_HTML.as_bytes().to_vec()),
                ),
                Route::Bridge(view) => {
                    if let Ok(json) = String::from_utf8(body(request)) {
                        let _ = self.events.unbounded_send(WidgetHostEvent::Message { view, json });
                    }
                    respond(204, "No Content", "text/plain", &[], None)
                }
                Route::App(view) => {
                    let page = self
                        .pages
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get(&view)
                        .cloned();
                    match page {
                        Some(page) => respond(
                            200,
                            "OK",
                            "text/html",
                            &[("Content-Security-Policy", page.csp.as_str())],
                            Some(page.html.into_bytes()),
                        ),
                        None => not_found(),
                    }
                }
                Route::NotFound => not_found(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_url_names_the_view() {
        assert_eq!(proxy_url(ViewId(42)), "partitur-widget://view-42/");
    }

    #[test]
    fn requests_are_routed_by_scheme_path_and_method() {
        assert_eq!(
            route("partitur-widget://view-3/", "GET"),
            Route::Proxy(ViewId(3))
        );
        assert_eq!(
            route("partitur-widget://view-3/?x#y", "GET"),
            Route::Proxy(ViewId(3))
        );
        assert_eq!(
            route("partitur-widget://view-3/bridge", "POST"),
            Route::Bridge(ViewId(3))
        );
        assert_eq!(
            route("partitur-app://view-3/", "GET"),
            Route::App(ViewId(3))
        );
        assert_eq!(
            route("partitur-widget://view-3/bridge", "GET"),
            Route::NotFound
        );
        assert_eq!(route("partitur-widget://view-3/", "POST"), Route::NotFound);
        assert_eq!(route("partitur-app://view-3/other", "GET"), Route::NotFound);
        assert_eq!(route("partitur-app://view-three/", "GET"), Route::NotFound);
        assert_eq!(route("partitur-app://3/", "GET"), Route::NotFound);
        assert_eq!(route("https://3/", "GET"), Route::NotFound);
        assert_eq!(route("partitur-app:3", "GET"), Route::NotFound);
    }

    #[test]
    fn a_route_holds_only_for_the_browser_of_its_view() {
        let (mine, other) = (ViewId(1), ViewId(2));
        for route in [Route::Proxy(mine), Route::Bridge(mine), Route::App(mine)] {
            assert_eq!(owned(route, Some(mine)), route);
            assert_eq!(owned(route, Some(other)), Route::NotFound);
            assert_eq!(owned(route, None), Route::NotFound);
        }
        assert_eq!(owned(Route::NotFound, Some(mine)), Route::NotFound);
    }

    #[test]
    fn the_proxy_and_the_widget_document_stay_where_they_are() {
        let view = ViewId(5);
        assert!(may_navigate("partitur-widget://view-5/", view, 0));
        assert!(may_navigate("partitur-app://view-5/", view, 1));
        assert!(may_navigate("partitur-app://view-5/?again#top", view, 1));
        for url in [
            "https://example.com/",
            "about:blank",
            "data:text/html,hi",
            "partitur-widget://view-6/",
            "partitur-app://view-5/",
        ] {
            assert!(!may_navigate(url, view, 0), "{url}");
        }
        for url in [
            "https://example.com/",
            "about:blank",
            "data:text/html,hi",
            "partitur-app://view-6/",
            "partitur-app://view-5/other",
            "partitur-widget://view-5/",
        ] {
            assert!(!may_navigate(url, view, 1), "{url}");
        }
        assert!(may_navigate("https://example.com/", view, 2));
        assert!(may_navigate("about:blank", view, 3));
    }

    #[test]
    fn only_open_schemes_and_the_views_own_origins_are_permitted() {
        let view = ViewId(5);
        assert!(permitted("https://example.com/a.js", view));
        assert!(permitted("HTTP://example.com/", view));
        assert!(permitted("data:text/plain,hi", view));
        assert!(permitted("partitur-widget://view-5/bridge", view));
        assert!(permitted("partitur-app://view-5/", view));
        assert!(!permitted("partitur-app://view-6/", view));
        assert!(!permitted("partitur-widget://view-6/bridge", view));
        assert!(!permitted("file:///etc/hostname", view));
        assert!(!permitted("blob:partitur-app://view-5/x", view));
        assert!(!permitted("chrome://settings", view));
        assert!(!permitted("no scheme", view));
    }
}
