//! A drawing an agent shows the user: one SVG document or one HTML page, handed over by the
//! `show_svg` and `show_html` tools. The markup travels as shared text, so the transcript, the
//! viewer and the page engine hold one copy. A drawing is at most [`MAX_DRAWING_BYTES`] bytes
//! and not blank. A transcript's tool call carries its drawing when it is a call of one of the
//! two tools with readable, non-blank input ([`Drawing::from_call`]). The texts the tools answer
//! with are defined here, so the tool server, the core and the viewer say the same, and so is
//! the content security policy a page is shown under ([`PAGE_POLICY`]).

use std::sync::Arc;

use crate::orchestrator::TOOL_SERVER;

/// The most markup, in bytes, one drawing may hold: 2 MB.
pub const MAX_DRAWING_BYTES: usize = 2 * 1024 * 1024;

/// The answer to a call whose drawing the user now sees.
pub const SHOWN: &str = "shown to the user";

/// The answer to a call from a session the user is not looking at: the drawing stays in the
/// session's transcript.
pub const KEPT: &str = "kept in the session; the user sees it when they open the session";

/// The error answer to a call whose markup is blank.
pub const EMPTY: &str = "the drawing is empty";

/// The error answer to a call whose markup is over [`MAX_DRAWING_BYTES`].
pub const TOO_LARGE: &str = "the drawing is larger than 2 MB";

/// The `Content-Security-Policy` an agent's HTML page is shown under: inline scripts and
/// styles, `data:` pictures, fonts and media, and nothing from the network. It is a widget's
/// default policy with `data:` fonts added, so that a page that must work again when reopened
/// later can carry its own fonts.
pub const PAGE_POLICY: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
    media-src 'self' data:; connect-src 'none';";

/// What kind of document a [`Drawing`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingKind {
    /// One complete SVG document.
    Svg,
    /// One complete, self-contained HTML document.
    Html,
}

impl DrawingKind {
    /// The name of the tool that shows a drawing of this kind.
    #[must_use]
    pub fn tool(self) -> &'static str {
        match self {
            Self::Svg => "show_svg",
            Self::Html => "show_html",
        }
    }

    /// The name of the tool's argument that holds the markup.
    #[must_use]
    pub fn argument(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Html => "html",
        }
    }
}

/// A document an agent asks to show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawing {
    /// Whether the markup is SVG or HTML.
    pub kind: DrawingKind,
    /// The document as the agent wrote it.
    pub markup: Arc<str>,
}

impl Drawing {
    /// The drawing a tool call carries: `title` must name the `show_svg` or `show_html` tool of
    /// Partitur's server, and `raw_input` must be a JSON object whose markup argument is a
    /// non-blank string. Anything else is no drawing.
    #[must_use]
    pub fn from_call(title: &str, raw_input: Option<&str>) -> Option<Self> {
        let kind = [DrawingKind::Svg, DrawingKind::Html]
            .into_iter()
            .find(|kind| title == format!("mcp__{TOOL_SERVER}__{}", kind.tool()))?;
        let input: serde_json::Value = serde_json::from_str(raw_input?).ok()?;
        let markup = input.get(kind.argument())?.as_str()?;
        (!markup.trim().is_empty()).then(|| Self {
            kind,
            markup: markup.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_show_svg_call_carries_its_drawing() {
        assert_eq!(
            Drawing::from_call("mcp__partitur__show_svg", Some(r#"{"svg":"<svg/>"}"#)),
            Some(Drawing {
                kind: DrawingKind::Svg,
                markup: "<svg/>".into()
            })
        );
        assert_eq!(
            Drawing::from_call("mcp__partitur__show_html", Some(r#"{"html":"<p>hi</p>"}"#)),
            Some(Drawing {
                kind: DrawingKind::Html,
                markup: "<p>hi</p>".into()
            })
        );
    }

    #[test]
    fn a_drawing_call_without_readable_input_has_no_drawing() {
        for input in [
            None,
            Some("not json"),
            Some(r#"{"html":"<p/>"}"#),
            Some(r#"{"svg":""}"#),
            Some(r#"{"svg":"  \n"}"#),
            Some(r#"{"svg":3}"#),
        ] {
            assert_eq!(Drawing::from_call("mcp__partitur__show_svg", input), None);
        }
        assert_eq!(
            Drawing::from_call("mcp__other__show_svg", Some(r#"{"svg":"<svg/>"}"#)),
            None
        );
        assert_eq!(
            Drawing::from_call("show_svg", Some(r#"{"svg":"<svg/>"}"#)),
            None
        );
    }

    #[test]
    fn each_kind_names_its_tool_and_argument() {
        assert_eq!(DrawingKind::Svg.tool(), "show_svg");
        assert_eq!(DrawingKind::Svg.argument(), "svg");
        assert_eq!(DrawingKind::Html.tool(), "show_html");
        assert_eq!(DrawingKind::Html.argument(), "html");
    }

    #[test]
    fn a_page_takes_data_fonts_and_nothing_from_the_network() {
        assert_eq!(
            PAGE_POLICY,
            "default-src 'none'; script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
             media-src 'self' data:; connect-src 'none';"
        );
    }

    #[test]
    fn the_limit_is_two_megabytes() {
        assert_eq!(MAX_DRAWING_BYTES, 2_097_152);
    }
}
