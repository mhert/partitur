//! A drawing an agent shows the user: one SVG document or one HTML page, handed over by the
//! `show_svg` and `show_html` tools. The markup travels as shared text, so the transcript, the
//! viewer and the page engine hold one copy. A drawing is at most [`MAX_DRAWING_BYTES`] bytes
//! and not blank; the texts the tools answer with are defined here, so the tool server, the
//! core and the viewer say the same.

use std::sync::Arc;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_names_its_tool_and_argument() {
        assert_eq!(DrawingKind::Svg.tool(), "show_svg");
        assert_eq!(DrawingKind::Svg.argument(), "svg");
        assert_eq!(DrawingKind::Html.tool(), "show_html");
        assert_eq!(DrawingKind::Html.argument(), "html");
    }

    #[test]
    fn the_limit_is_two_megabytes() {
        assert_eq!(MAX_DRAWING_BYTES, 2_097_152);
    }
}
