//! The wire protocol between Partitur and its widget helper process, spoken over the helper's
//! standard input (commands, [`ToEngine`]) and standard output (events, [`FromEngine`]).
//!
//! Every message is framed as a little-endian `u32` length of the rest, one kind byte and the
//! body. A length of zero (no kind byte) is malformed, and one over [`MAX_MESSAGE`] is refused
//! before anything more is read or allocated. The kinds are:
//!
//! - `1`, a control message: the body is JSON of a private mirror of [`ToEngine`] (commands) or
//!   of every [`FromEngine`] but a frame (events). The mirrors keep the wire independent of the
//!   core types.
//! - `2`, a frame, events only: the body is binary and little-endian. The view as `u64`, the
//!   picture's width and height as `u32`, the region count as `u32`, then for each region its
//!   `x`, `y`, `width` and `height` as `u32` followed by its rows, `width * height * 4` bytes of
//!   BGRA with premultiplied alpha, top to bottom. A region whose rows do not fill it, or bytes
//!   after the last region, make the frame malformed.
//!
//! A stream that ends between messages is a clean end ([`None`]); one that ends inside a message
//! is [`ProtocolError::Truncated`].
//!
//! The helper's first event is [`FromEngine::Hello`], naming the [`VERSION`] it speaks. It also
//! understands two switches of its own, [`NO_SANDBOX`] and [`NO_PROBE`], and exits with
//! [`PROBE_FAILED`] when its sandbox probe failed without aborting.

use std::io::{self, Read, Write};

use partitur_core::widget_bridge::WidgetTheme;
use partitur_core::widget_host::{
    Damage, Picture, Region, ViewId, WidgetCursor, WidgetInput, WidgetPage,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The version of this protocol, which the helper reports in its [`FromEngine::Hello`].
pub const VERSION: u32 = 1;

/// The longest message accepted or written, counting its kind byte and body.
pub const MAX_MESSAGE: u32 = 64 * 1024 * 1024;

/// The helper's exit code when its sandbox probe failed without aborting.
pub const PROBE_FAILED: i32 = 3;

/// The helper's switch to run without the sandbox.
pub const NO_SANDBOX: &str = "--no-sandbox";

/// The helper's switch to skip the sandbox probe.
pub const NO_PROBE: &str = "--partitur-no-probe";

/// The kind byte of a control message.
const CONTROL: u8 = 1;

/// The kind byte of a frame.
const FRAME: u8 = 2;

/// What goes wrong reading or writing a message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The stream failed.
    #[error("the widget engine's stream failed: {0}")]
    Io(String),
    /// A message is longer than [`MAX_MESSAGE`]; the number is its length, or `u32::MAX` when
    /// that does not even fit.
    #[error("a widget engine message of {0} bytes is over the limit")]
    TooLong(u32),
    /// A message of a kind this side does not know.
    #[error("a widget engine message of unknown kind {0}")]
    UnknownKind(u8),
    /// A message whose body is not what its kind says.
    #[error("a malformed widget engine message: {0}")]
    Malformed(String),
    /// The stream ended inside a message.
    #[error("the widget engine's stream ended inside a message")]
    Truncated,
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// A command from Partitur to the helper.
#[derive(Debug, Clone, PartialEq)]
pub enum ToEngine {
    /// Opens a view showing `page`.
    Open {
        /// The view.
        view: ViewId,
        /// The page to show.
        page: WidgetPage,
    },
    /// Posts a JSON message to the page of `view`.
    Post {
        /// The view.
        view: ViewId,
        /// The message.
        json: String,
    },
    /// Resizes `view` to `width` by `height` logical pixels at `scale` device pixels each.
    Resize {
        /// The view.
        view: ViewId,
        /// The width in logical pixels.
        width: u32,
        /// The height in logical pixels.
        height: u32,
        /// Device pixels per logical pixel.
        scale: f32,
    },
    /// Shows or hides `view`.
    SetVisible {
        /// The view.
        view: ViewId,
        /// Whether it is visible.
        visible: bool,
    },
    /// Forwards a pointer event to `view`.
    Input {
        /// The view.
        view: ViewId,
        /// The event.
        input: WidgetInput,
    },
    /// Tells `view` the color scheme to follow.
    SetTheme {
        /// The view.
        view: ViewId,
        /// The scheme.
        theme: WidgetTheme,
    },
    /// Closes `view`.
    Close {
        /// The view.
        view: ViewId,
    },
    /// Ends the helper.
    Shutdown,
}

/// The regions of a view's picture that changed, each with its own rows in BGRA with
/// premultiplied alpha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRegions {
    /// The view.
    pub view: ViewId,
    /// The picture's width in pixels.
    pub width: u32,
    /// The picture's height in pixels.
    pub height: u32,
    /// Each changed region and its rows, `region.width * 4` bytes a row.
    pub regions: Vec<(Region, Vec<u8>)>,
}

/// An event from the helper to Partitur.
#[derive(Debug, Clone, PartialEq)]
pub enum FromEngine {
    /// The helper's first message.
    Hello {
        /// The protocol version it speaks.
        version: u32,
        /// Whether it runs in CEF's sandbox.
        sandboxed: bool,
    },
    /// A view painted.
    Frame(FrameRegions),
    /// The pointer's look over a view changed.
    Cursor {
        /// The view.
        view: ViewId,
        /// The new look.
        cursor: WidgetCursor,
    },
    /// The page of a view posted a JSON message.
    Message {
        /// The view.
        view: ViewId,
        /// The message.
        json: String,
    },
    /// The user followed a link in a view's page.
    OpenLink {
        /// The view.
        view: ViewId,
        /// The link.
        url: String,
    },
    /// A view failed.
    Failed {
        /// The view.
        view: ViewId,
        /// What went wrong.
        error: String,
    },
}

/// The wire form of a [`WidgetTheme`].
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireTheme {
    Light,
    Dark,
}

impl From<WidgetTheme> for WireTheme {
    fn from(theme: WidgetTheme) -> Self {
        match theme {
            WidgetTheme::Light => Self::Light,
            WidgetTheme::Dark => Self::Dark,
        }
    }
}

impl From<WireTheme> for WidgetTheme {
    fn from(theme: WireTheme) -> Self {
        match theme {
            WireTheme::Light => Self::Light,
            WireTheme::Dark => Self::Dark,
        }
    }
}

/// The wire form of a [`WidgetPage`].
#[derive(Serialize, Deserialize)]
struct WirePage {
    html: String,
    csp: String,
    theme: WireTheme,
}

/// The wire form of a [`WidgetInput`].
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireInput {
    Move {
        x: f32,
        y: f32,
    },
    Press {
        x: f32,
        y: f32,
        button: u8,
    },
    Release {
        x: f32,
        y: f32,
        button: u8,
    },
    Wheel {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        precise: bool,
    },
    Leave,
}

impl From<WidgetInput> for WireInput {
    fn from(input: WidgetInput) -> Self {
        match input {
            WidgetInput::Move { x, y } => Self::Move { x, y },
            WidgetInput::Press { x, y, button } => Self::Press { x, y, button },
            WidgetInput::Release { x, y, button } => Self::Release { x, y, button },
            WidgetInput::Wheel {
                x,
                y,
                dx,
                dy,
                precise,
            } => Self::Wheel {
                x,
                y,
                dx,
                dy,
                precise,
            },
            WidgetInput::Leave => Self::Leave,
        }
    }
}

impl From<WireInput> for WidgetInput {
    fn from(input: WireInput) -> Self {
        match input {
            WireInput::Move { x, y } => Self::Move { x, y },
            WireInput::Press { x, y, button } => Self::Press { x, y, button },
            WireInput::Release { x, y, button } => Self::Release { x, y, button },
            WireInput::Wheel {
                x,
                y,
                dx,
                dy,
                precise,
            } => Self::Wheel {
                x,
                y,
                dx,
                dy,
                precise,
            },
            WireInput::Leave => Self::Leave,
        }
    }
}

/// The wire form of a [`WidgetCursor`].
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireCursor {
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
    NotAllowed,
    ResizeHorizontal,
    ResizeVertical,
    Crosshair,
}

impl From<WidgetCursor> for WireCursor {
    fn from(cursor: WidgetCursor) -> Self {
        match cursor {
            WidgetCursor::Default => Self::Default,
            WidgetCursor::Pointer => Self::Pointer,
            WidgetCursor::Text => Self::Text,
            WidgetCursor::Grab => Self::Grab,
            WidgetCursor::Grabbing => Self::Grabbing,
            WidgetCursor::NotAllowed => Self::NotAllowed,
            WidgetCursor::ResizeHorizontal => Self::ResizeHorizontal,
            WidgetCursor::ResizeVertical => Self::ResizeVertical,
            WidgetCursor::Crosshair => Self::Crosshair,
        }
    }
}

impl From<WireCursor> for WidgetCursor {
    fn from(cursor: WireCursor) -> Self {
        match cursor {
            WireCursor::Default => Self::Default,
            WireCursor::Pointer => Self::Pointer,
            WireCursor::Text => Self::Text,
            WireCursor::Grab => Self::Grab,
            WireCursor::Grabbing => Self::Grabbing,
            WireCursor::NotAllowed => Self::NotAllowed,
            WireCursor::ResizeHorizontal => Self::ResizeHorizontal,
            WireCursor::ResizeVertical => Self::ResizeVertical,
            WireCursor::Crosshair => Self::Crosshair,
        }
    }
}

/// The wire form of a [`ToEngine`].
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireToEngine {
    Open {
        view: u64,
        page: WirePage,
    },
    Post {
        view: u64,
        json: String,
    },
    Resize {
        view: u64,
        width: u32,
        height: u32,
        scale: f32,
    },
    SetVisible {
        view: u64,
        visible: bool,
    },
    Input {
        view: u64,
        input: WireInput,
    },
    SetTheme {
        view: u64,
        theme: WireTheme,
    },
    Close {
        view: u64,
    },
    Shutdown,
}

impl From<&ToEngine> for WireToEngine {
    fn from(message: &ToEngine) -> Self {
        match message {
            ToEngine::Open { view, page } => Self::Open {
                view: view.0,
                page: WirePage {
                    html: page.html.clone(),
                    csp: page.csp.clone(),
                    theme: page.theme.into(),
                },
            },
            ToEngine::Post { view, json } => Self::Post {
                view: view.0,
                json: json.clone(),
            },
            ToEngine::Resize {
                view,
                width,
                height,
                scale,
            } => Self::Resize {
                view: view.0,
                width: *width,
                height: *height,
                scale: *scale,
            },
            ToEngine::SetVisible { view, visible } => Self::SetVisible {
                view: view.0,
                visible: *visible,
            },
            ToEngine::Input { view, input } => Self::Input {
                view: view.0,
                input: (*input).into(),
            },
            ToEngine::SetTheme { view, theme } => Self::SetTheme {
                view: view.0,
                theme: (*theme).into(),
            },
            ToEngine::Close { view } => Self::Close { view: view.0 },
            ToEngine::Shutdown => Self::Shutdown,
        }
    }
}

impl From<WireToEngine> for ToEngine {
    fn from(message: WireToEngine) -> Self {
        match message {
            WireToEngine::Open { view, page } => Self::Open {
                view: ViewId(view),
                page: WidgetPage {
                    html: page.html,
                    csp: page.csp,
                    theme: page.theme.into(),
                },
            },
            WireToEngine::Post { view, json } => Self::Post {
                view: ViewId(view),
                json,
            },
            WireToEngine::Resize {
                view,
                width,
                height,
                scale,
            } => Self::Resize {
                view: ViewId(view),
                width,
                height,
                scale,
            },
            WireToEngine::SetVisible { view, visible } => Self::SetVisible {
                view: ViewId(view),
                visible,
            },
            WireToEngine::Input { view, input } => Self::Input {
                view: ViewId(view),
                input: input.into(),
            },
            WireToEngine::SetTheme { view, theme } => Self::SetTheme {
                view: ViewId(view),
                theme: theme.into(),
            },
            WireToEngine::Close { view } => Self::Close { view: ViewId(view) },
            WireToEngine::Shutdown => Self::Shutdown,
        }
    }
}

/// The wire form of a [`FromEngine`] but a frame, which has a kind of its own.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireFromEngine {
    Hello { version: u32, sandboxed: bool },
    Cursor { view: u64, cursor: WireCursor },
    Message { view: u64, json: String },
    OpenLink { view: u64, url: String },
    Failed { view: u64, error: String },
}

impl From<WireFromEngine> for FromEngine {
    fn from(message: WireFromEngine) -> Self {
        match message {
            WireFromEngine::Hello { version, sandboxed } => Self::Hello { version, sandboxed },
            WireFromEngine::Cursor { view, cursor } => Self::Cursor {
                view: ViewId(view),
                cursor: cursor.into(),
            },
            WireFromEngine::Message { view, json } => Self::Message {
                view: ViewId(view),
                json,
            },
            WireFromEngine::OpenLink { view, url } => Self::OpenLink {
                view: ViewId(view),
                url,
            },
            WireFromEngine::Failed { view, error } => Self::Failed {
                view: ViewId(view),
                error,
            },
        }
    }
}

/// Writes one message of `kind` with `body`, as a single write followed by a flush.
fn write_message(out: &mut impl Write, kind: u8, body: &[u8]) -> Result<(), ProtocolError> {
    let len = u32::try_from(body.len() + 1).unwrap_or(u32::MAX);
    if len > MAX_MESSAGE {
        return Err(ProtocolError::TooLong(len));
    }
    let mut message = Vec::with_capacity(body.len() + 5);
    message.extend_from_slice(&len.to_le_bytes());
    message.push(kind);
    message.extend_from_slice(body);
    out.write_all(&message)?;
    out.flush()?;
    Ok(())
}

/// Reads up to `len` bytes, fewer only when the input ends.
fn fill(input: &mut impl Read, len: u64) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = Vec::new();
    input.take(len).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Reads one message's kind and body, or `None` when the input ends before its first byte.
fn read_message(input: &mut impl Read) -> Result<Option<(u8, Vec<u8>)>, ProtocolError> {
    let header = fill(input, 4)?;
    let Ok(header) = <[u8; 4]>::try_from(header.as_slice()) else {
        return if header.is_empty() {
            Ok(None)
        } else {
            Err(ProtocolError::Truncated)
        };
    };
    let len = u32::from_le_bytes(header);
    if len > MAX_MESSAGE {
        return Err(ProtocolError::TooLong(len));
    }
    if len == 0 {
        return Err(ProtocolError::Malformed(
            "a message without a kind".to_owned(),
        ));
    }
    let body = fill(input, u64::from(len))?;
    if u64::try_from(body.len()) != Ok(u64::from(len)) {
        return Err(ProtocolError::Truncated);
    }
    let mut body = body;
    let kind = body.remove(0);
    Ok(Some((kind, body)))
}

/// Parses a control message's JSON body.
fn parse<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, ProtocolError> {
    serde_json::from_slice(body).map_err(|error| ProtocolError::Malformed(error.to_string()))
}

/// Writes a command.
///
/// # Errors
///
/// Fails when the message is over [`MAX_MESSAGE`] or the stream fails.
pub fn write_to_engine(out: &mut impl Write, message: &ToEngine) -> Result<(), ProtocolError> {
    let body = serde_json::to_vec(&WireToEngine::from(message))
        .map_err(|error| ProtocolError::Malformed(error.to_string()))?;
    write_message(out, CONTROL, &body)
}

/// Reads a command, or `None` when the input ended between messages.
///
/// # Errors
///
/// Fails on a stream that fails or ends inside a message, a message that is too long or of an
/// unknown kind, and a body that is not a command.
pub fn read_to_engine(input: &mut impl Read) -> Result<Option<ToEngine>, ProtocolError> {
    match read_message(input)? {
        None => Ok(None),
        Some((CONTROL, body)) => Ok(Some(parse::<WireToEngine>(&body)?.into())),
        Some((kind, _)) => Err(ProtocolError::UnknownKind(kind)),
    }
}

/// Writes an event.
///
/// # Errors
///
/// Fails when the message is over [`MAX_MESSAGE`] or the stream fails.
pub fn write_from_engine(out: &mut impl Write, message: &FromEngine) -> Result<(), ProtocolError> {
    let wire = match message {
        FromEngine::Frame(frame) => {
            if let Some((region, _)) = frame
                .regions
                .iter()
                .find(|(region, rows)| rows_len(*region) != Some(rows.len()))
            {
                return Err(ProtocolError::Malformed(format!(
                    "the rows of the region {region:?} do not fill it"
                )));
            }
            return write_message(out, FRAME, &frame_body(frame));
        }
        FromEngine::Hello { version, sandboxed } => WireFromEngine::Hello {
            version: *version,
            sandboxed: *sandboxed,
        },
        FromEngine::Cursor { view, cursor } => WireFromEngine::Cursor {
            view: view.0,
            cursor: (*cursor).into(),
        },
        FromEngine::Message { view, json } => WireFromEngine::Message {
            view: view.0,
            json: json.clone(),
        },
        FromEngine::OpenLink { view, url } => WireFromEngine::OpenLink {
            view: view.0,
            url: url.clone(),
        },
        FromEngine::Failed { view, error } => WireFromEngine::Failed {
            view: view.0,
            error: error.clone(),
        },
    };
    let body =
        serde_json::to_vec(&wire).map_err(|error| ProtocolError::Malformed(error.to_string()))?;
    write_message(out, CONTROL, &body)
}

/// Reads an event, or `None` when the input ended between messages.
///
/// # Errors
///
/// Fails on a stream that fails or ends inside a message, a message that is too long or of an
/// unknown kind, and a body that is not an event.
pub fn read_from_engine(input: &mut impl Read) -> Result<Option<FromEngine>, ProtocolError> {
    match read_message(input)? {
        None => Ok(None),
        Some((CONTROL, body)) => Ok(Some(parse::<WireFromEngine>(&body)?.into())),
        Some((FRAME, body)) => Ok(Some(FromEngine::Frame(parse_frame(&body)?))),
        Some((kind, _)) => Err(ProtocolError::UnknownKind(kind)),
    }
}

/// The binary body of a frame.
fn frame_body(frame: &FrameRegions) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&frame.view.0.to_le_bytes());
    body.extend_from_slice(&frame.width.to_le_bytes());
    body.extend_from_slice(&frame.height.to_le_bytes());
    body.extend_from_slice(
        &u32::try_from(frame.regions.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for (region, rows) in &frame.regions {
        for value in [region.x, region.y, region.width, region.height] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body.extend_from_slice(rows);
    }
    body
}

/// A reader over a frame's body.
struct Body<'a>(&'a [u8]);

impl<'a> Body<'a> {
    /// The next `len` bytes.
    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        let (head, tail) = self
            .0
            .split_at_checked(len)
            .ok_or_else(|| ProtocolError::Malformed("a frame cut short".to_owned()))?;
        self.0 = tail;
        Ok(head)
    }

    /// The next `u32`.
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap_or_default()))
    }

    /// The next `u64`.
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap_or_default()))
    }
}

/// The number of bytes of the rows of `region`, if it fits in memory.
fn rows_len(region: Region) -> Option<usize> {
    usize::try_from(region.width)
        .ok()?
        .checked_mul(usize::try_from(region.height).ok()?)?
        .checked_mul(4)
}

/// Reads a frame from its binary body.
fn parse_frame(body: &[u8]) -> Result<FrameRegions, ProtocolError> {
    let mut body = Body(body);
    let view = ViewId(body.u64()?);
    let width = body.u32()?;
    let height = body.u32()?;
    let count = body.u32()?;
    let mut regions = Vec::new();
    for _ in 0..count {
        let region = Region {
            x: body.u32()?,
            y: body.u32()?,
            width: body.u32()?,
            height: body.u32()?,
        };
        let len = rows_len(region)
            .ok_or_else(|| ProtocolError::Malformed("a region too large".to_owned()))?;
        regions.push((region, body.take(len)?.to_vec()));
    }
    if !body.0.is_empty() {
        return Err(ProtocolError::Malformed(
            "bytes after the last region".to_owned(),
        ));
    }
    Ok(FrameRegions {
        view,
        width,
        height,
        regions,
    })
}

/// The bytes of a frame's body besides its regions: view, size and region count.
const FRAME_HEADER: usize = 20;

/// The bytes of a region's position and size in a frame's body.
const REGION_HEADER: usize = 16;

/// Cuts the rows of each damaged region out of `picture`, in as many frames as it takes for
/// each to stay within [`MAX_MESSAGE`] once written: a region goes into the frame in the making
/// until the next would not fit, and one too large for a frame alone is cut into bands of whole
/// rows. A region is cut down to what lies inside the picture, and one outside it, or whose
/// rows the picture's pixels do not hold, is left out. There is always at least one frame, empty
/// when nothing is left. The UI paints all of them into the one picture.
#[must_use]
pub fn frame_regions(view: ViewId, picture: &Picture, damage: &Damage) -> Vec<FrameRegions> {
    let room = MAX_MESSAGE as usize - 1 - FRAME_HEADER;
    let empty = || FrameRegions {
        view,
        width: picture.width,
        height: picture.height,
        regions: Vec::new(),
    };
    let mut frames = Vec::new();
    let mut frame = empty();
    let mut used = 0;
    let regions = damage
        .regions()
        .iter()
        .filter_map(|region| region.within(picture.width, picture.height));
    for region in regions {
        let row = usize::try_from(region.width).map_or(usize::MAX, |width| width.saturating_mul(4));
        let band = (room.saturating_sub(REGION_HEADER) / row.max(1)).max(1);
        let band = u32::try_from(band).unwrap_or(u32::MAX);
        let end = region.y + region.height;
        for top in (region.y..end).step_by(usize::try_from(band).unwrap_or(usize::MAX)) {
            let piece = Region {
                height: band.min(end - top),
                y: top,
                ..region
            };
            let Some(rows) = cut(picture, piece) else {
                continue;
            };
            let cost = REGION_HEADER + rows.len();
            if used + cost > room && !frame.regions.is_empty() {
                frames.push(std::mem::replace(&mut frame, empty()));
                used = 0;
            }
            used += cost;
            frame.regions.push((piece, rows));
        }
    }
    if !frame.regions.is_empty() || frames.is_empty() {
        frames.push(frame);
    }
    frames
}

/// The rows of `region`, which lies inside `picture`, or `None` when its pixels are missing.
fn cut(picture: &Picture, region: Region) -> Option<Vec<u8>> {
    let stride = usize::try_from(picture.width).ok()?.checked_mul(4)?;
    let column = usize::try_from(region.x).ok()?.checked_mul(4)?;
    let len = usize::try_from(region.width).ok()?.checked_mul(4)?;
    let mut rows = Vec::with_capacity(len.checked_mul(usize::try_from(region.height).ok()?)?);
    for y in region.y..region.y + region.height {
        let at = usize::try_from(y)
            .ok()?
            .checked_mul(stride)?
            .checked_add(column)?;
        rows.extend_from_slice(picture.bgra.get(at..at.checked_add(len)?)?);
    }
    Some(rows)
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
    use partitur_core::widget_host::LatestFrame;

    const VIEW: ViewId = ViewId(7);

    fn to_bytes(message: &ToEngine) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_to_engine(&mut bytes, message).unwrap();
        bytes
    }

    fn from_bytes(message: &FromEngine) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_from_engine(&mut bytes, message).unwrap();
        bytes
    }

    fn framed(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(body.len() + 1)
            .unwrap()
            .to_le_bytes()
            .to_vec();
        bytes.push(kind);
        bytes.extend_from_slice(body);
        bytes
    }

    fn frame_body(regions: &[(u32, u32, u32, u32, usize)]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&7_u64.to_le_bytes());
        body.extend_from_slice(&4_u32.to_le_bytes());
        body.extend_from_slice(&4_u32.to_le_bytes());
        body.extend_from_slice(&u32::try_from(regions.len()).unwrap().to_le_bytes());
        for &(x, y, width, height, bytes) in regions {
            for value in [x, y, width, height] {
                body.extend_from_slice(&value.to_le_bytes());
            }
            body.extend(std::iter::repeat_n(9_u8, bytes));
        }
        body
    }

    #[test]
    fn every_command_survives_the_wire() {
        let inputs = [
            WidgetInput::Move { x: 1.5, y: -2.25 },
            WidgetInput::Press {
                x: 1.0,
                y: 2.0,
                button: 2,
            },
            WidgetInput::Release {
                x: 3.0,
                y: 4.0,
                button: 1,
            },
            WidgetInput::Wheel {
                x: 5.0,
                y: 6.0,
                dx: -7.5,
                dy: 8.5,
                precise: true,
            },
            WidgetInput::Leave,
        ];
        let mut commands = vec![
            ToEngine::Open {
                view: VIEW,
                page: WidgetPage {
                    html: "<p>é</p>".to_owned(),
                    csp: "default-src 'none'".to_owned(),
                    theme: WidgetTheme::Dark,
                },
            },
            ToEngine::Open {
                view: VIEW,
                page: WidgetPage {
                    html: String::new(),
                    csp: String::new(),
                    theme: WidgetTheme::Light,
                },
            },
            ToEngine::Post {
                view: VIEW,
                json: r#"{"a":1}"#.to_owned(),
            },
            ToEngine::Resize {
                view: VIEW,
                width: 640,
                height: 480,
                scale: 1.25,
            },
            ToEngine::SetVisible {
                view: VIEW,
                visible: true,
            },
            ToEngine::SetVisible {
                view: VIEW,
                visible: false,
            },
            ToEngine::SetTheme {
                view: VIEW,
                theme: WidgetTheme::Dark,
            },
            ToEngine::SetTheme {
                view: VIEW,
                theme: WidgetTheme::Light,
            },
            ToEngine::Close { view: VIEW },
            ToEngine::Shutdown,
        ];
        commands.extend(inputs.map(|input| ToEngine::Input { view: VIEW, input }));
        let mut wire = Vec::new();
        for command in &commands {
            write_to_engine(&mut wire, command).unwrap();
        }
        let mut input = wire.as_slice();
        for command in commands {
            assert_eq!(read_to_engine(&mut input).unwrap(), Some(command));
        }
        assert_eq!(read_to_engine(&mut input).unwrap(), None);
    }

    #[test]
    fn every_event_survives_the_wire() {
        let cursors = [
            WidgetCursor::Default,
            WidgetCursor::Pointer,
            WidgetCursor::Text,
            WidgetCursor::Grab,
            WidgetCursor::Grabbing,
            WidgetCursor::NotAllowed,
            WidgetCursor::ResizeHorizontal,
            WidgetCursor::ResizeVertical,
            WidgetCursor::Crosshair,
        ];
        let mut events = vec![
            FromEngine::Hello {
                version: VERSION,
                sandboxed: true,
            },
            FromEngine::Hello {
                version: 2,
                sandboxed: false,
            },
            FromEngine::Frame(FrameRegions {
                view: VIEW,
                width: 4,
                height: 2,
                regions: vec![
                    (
                        Region {
                            x: 0,
                            y: 0,
                            width: 1,
                            height: 2,
                        },
                        vec![1; 8],
                    ),
                    (
                        Region {
                            x: 2,
                            y: 1,
                            width: 2,
                            height: 1,
                        },
                        vec![2; 8],
                    ),
                ],
            }),
            FromEngine::Frame(FrameRegions {
                view: VIEW,
                width: 0,
                height: 0,
                regions: vec![],
            }),
            FromEngine::Message {
                view: VIEW,
                json: "{}".to_owned(),
            },
            FromEngine::OpenLink {
                view: VIEW,
                url: "https://example.com".to_owned(),
            },
            FromEngine::Failed {
                view: VIEW,
                error: "boom".to_owned(),
            },
        ];
        events.extend(cursors.map(|cursor| FromEngine::Cursor { view: VIEW, cursor }));
        let mut wire = Vec::new();
        for event in &events {
            write_from_engine(&mut wire, event).unwrap();
        }
        let mut input = wire.as_slice();
        for event in events {
            assert_eq!(read_from_engine(&mut input).unwrap(), Some(event));
        }
        assert_eq!(read_from_engine(&mut input).unwrap(), None);
    }

    #[test]
    fn the_end_of_input_between_messages_is_no_message() {
        assert_eq!(read_to_engine(&mut [].as_slice()).unwrap(), None);
        assert_eq!(read_from_engine(&mut [].as_slice()).unwrap(), None);
    }

    #[test]
    fn the_end_of_input_inside_a_message_is_truncated() {
        let frame = from_bytes(&FromEngine::Frame(FrameRegions {
            view: VIEW,
            width: 1,
            height: 1,
            regions: vec![(Region::whole(1, 1), vec![0; 4])],
        }));
        assert_eq!(
            read_from_engine(&mut &frame[..10]),
            Err(ProtocolError::Truncated)
        );
        assert_eq!(
            read_from_engine(&mut &frame[..2]),
            Err(ProtocolError::Truncated)
        );
        assert_eq!(
            read_from_engine(&mut &frame[..4]),
            Err(ProtocolError::Truncated)
        );
        let command = to_bytes(&ToEngine::Shutdown);
        assert_eq!(
            read_to_engine(&mut &command[..command.len() - 1]),
            Err(ProtocolError::Truncated)
        );
    }

    #[test]
    fn a_length_over_the_limit_is_refused_before_reading() {
        let bytes = (MAX_MESSAGE + 1).to_le_bytes();
        assert_eq!(
            read_to_engine(&mut bytes.as_slice()),
            Err(ProtocolError::TooLong(MAX_MESSAGE + 1))
        );
        assert_eq!(
            read_from_engine(&mut bytes.as_slice()),
            Err(ProtocolError::TooLong(MAX_MESSAGE + 1))
        );
        // The limit itself is allowed: it only runs out of input.
        let bytes = MAX_MESSAGE.to_le_bytes();
        assert_eq!(
            read_to_engine(&mut bytes.as_slice()),
            Err(ProtocolError::Truncated)
        );
    }

    #[test]
    fn the_limit_is_sixty_four_mebibytes() {
        assert_eq!(MAX_MESSAGE, 67_108_864);
    }

    #[test]
    fn an_interrupted_read_is_tried_again() {
        struct Once<'a>(bool, &'a [u8]);
        impl Read for Once<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.1.read(buf)
            }
        }
        let bytes = to_bytes(&ToEngine::Shutdown);
        assert_eq!(
            read_to_engine(&mut Once(false, &bytes)),
            Ok(Some(ToEngine::Shutdown))
        );
    }

    #[test]
    fn a_message_of_exactly_the_limit_is_written() {
        let body = vec![0; MAX_MESSAGE as usize - 1];
        write_message(&mut std::io::sink(), CONTROL, &body).unwrap();
        let body = vec![0; MAX_MESSAGE as usize];
        assert_eq!(
            write_message(&mut std::io::sink(), CONTROL, &body),
            Err(ProtocolError::TooLong(MAX_MESSAGE + 1))
        );
    }

    #[test]
    fn an_empty_message_is_malformed() {
        let bytes = 0_u32.to_le_bytes();
        assert!(matches!(
            read_to_engine(&mut bytes.as_slice()),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        assert_eq!(
            read_to_engine(&mut framed(9, b"{}").as_slice()),
            Err(ProtocolError::UnknownKind(9))
        );
        assert_eq!(
            read_from_engine(&mut framed(0, b"").as_slice()),
            Err(ProtocolError::UnknownKind(0))
        );
        // A frame only goes from the engine.
        assert_eq!(
            read_to_engine(&mut framed(2, b"").as_slice()),
            Err(ProtocolError::UnknownKind(2))
        );
    }

    #[test]
    fn bad_json_is_malformed() {
        assert!(matches!(
            read_to_engine(&mut framed(1, b"{nope").as_slice()),
            Err(ProtocolError::Malformed(_))
        ));
        assert!(matches!(
            read_from_engine(&mut framed(1, br#"{"type":"nothing"}"#).as_slice()),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn a_frame_region_whose_rows_do_not_fill_it_is_malformed() {
        // 2x2 pixels need 16 bytes.
        for body in [
            frame_body(&[(0, 0, 2, 2, 15)]),
            frame_body(&[(0, 0, 2, 2, 17)]),
            frame_body(&[(0, 0, 2, 2, 16), (0, 0, 1, 1, 0)]),
            // The header itself is cut short.
            frame_body(&[])[..10].to_vec(),
            // A region whose size overflows.
            frame_body(&[(0, 0, u32::MAX, u32::MAX, 0)]),
        ] {
            assert!(matches!(
                read_from_engine(&mut framed(2, &body).as_slice()),
                Err(ProtocolError::Malformed(_))
            ));
        }
        let ok = frame_body(&[(0, 0, 2, 2, 16)]);
        assert!(read_from_engine(&mut framed(2, &ok).as_slice()).is_ok());
    }

    #[test]
    fn io_errors_are_reported() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("gone"))
            }
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("gone"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            read_to_engine(&mut Broken),
            Err(ProtocolError::Io("gone".to_owned()))
        );
        assert_eq!(
            write_to_engine(&mut Broken, &ToEngine::Shutdown),
            Err(ProtocolError::Io("gone".to_owned()))
        );
    }

    #[test]
    fn a_message_is_written_and_flushed_at_once() {
        struct Counting {
            writes: usize,
            flushes: usize,
        }
        impl Write for Counting {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut out = Counting {
            writes: 0,
            flushes: 0,
        };
        write_to_engine(&mut out, &ToEngine::Shutdown).unwrap();
        assert_eq!((out.writes, out.flushes), (1, 1));
    }

    #[test]
    fn the_layout_of_a_frame_is_pinned() {
        let bytes = from_bytes(&FromEngine::Frame(FrameRegions {
            view: ViewId(1),
            width: 2,
            height: 3,
            regions: vec![(
                Region {
                    x: 4,
                    y: 5,
                    width: 1,
                    height: 1,
                },
                vec![9, 8, 7, 6],
            )],
        }));
        let mut expected = Vec::new();
        for value in [1_u32, 0, 2, 3, 1, 4, 5, 1, 1] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&[9, 8, 7, 6]);
        assert_eq!(bytes, framed(2, &expected));
    }

    fn picture_4x2() -> Picture {
        let bgra = (0..8_u8)
            .flat_map(|pixel| [pixel, pixel, pixel, 255])
            .collect();
        Picture {
            width: 4,
            height: 2,
            bgra,
        }
    }

    #[test]
    fn frame_regions_cut_each_damaged_region_out_of_the_picture() {
        let mut damage = Damage::default();
        damage.add(Region {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        });
        let frame = frame_regions(VIEW, &picture_4x2(), &damage).remove(0);
        let pixels = |ids: [u8; 4]| {
            ids.iter()
                .flat_map(|id| [*id, *id, *id, 255])
                .collect::<Vec<u8>>()
        };
        assert_eq!(frame.view, VIEW);
        assert_eq!((frame.width, frame.height), (4, 2));
        assert_eq!(
            frame.regions,
            vec![(
                Region {
                    x: 1,
                    y: 0,
                    width: 2,
                    height: 2
                },
                pixels([1, 2, 5, 6])
            )]
        );
    }

    #[test]
    fn frame_regions_skip_what_the_picture_does_not_hold() {
        let mut damage = Damage::default();
        damage.add(Region {
            x: 10,
            y: 10,
            width: 2,
            height: 2,
        });
        damage.add(Region {
            x: 3,
            y: 1,
            width: 5,
            height: 5,
        });
        let frame = frame_regions(VIEW, &picture_4x2(), &damage).remove(0);
        assert_eq!(
            frame.regions,
            vec![(
                Region {
                    x: 3,
                    y: 1,
                    width: 1,
                    height: 1
                },
                vec![7, 7, 7, 255]
            )]
        );
        let short = Picture {
            width: 4,
            height: 2,
            bgra: vec![0; 8],
        };
        let mut damage = Damage::default();
        damage.add(Region::whole(4, 2));
        assert!(frame_regions(VIEW, &short, &damage)[0].regions.is_empty());
    }

    #[test]
    fn frame_regions_on_the_ui_side_rebuild_the_picture() {
        let picture = picture_4x2();
        let mut damage = Damage::default();
        damage.add(Region::whole(4, 2));
        let frames = frame_regions(VIEW, &picture, &damage);
        assert_eq!(frames.len(), 1);
        let slot = LatestFrame::default();
        assert!(paint_all(&slot, &frames));
        assert_eq!(slot.picture(), picture);
    }

    fn paint_all(slot: &LatestFrame, frames: &[FrameRegions]) -> bool {
        let mut announced = false;
        for frame in frames {
            announced |= slot.paint_regions(
                frame.width,
                frame.height,
                frame
                    .regions
                    .iter()
                    .map(|(region, rows)| (*region, rows.as_slice())),
            );
        }
        announced
    }

    #[test]
    fn a_picture_too_large_for_one_message_is_split_and_rebuilt() {
        let (width, height) = (5000_u32, 4000_u32);
        let bgra = (0..width * height * 4).map(|i| (i % 251) as u8).collect();
        let picture = Picture {
            width,
            height,
            bgra,
        };
        let mut damage = Damage::default();
        damage.add(Region::whole(width, height));
        let frames = frame_regions(VIEW, &picture, &damage);
        assert!(frames.len() > 1);
        for frame in &frames {
            let mut wire = Vec::new();
            write_from_engine(&mut wire, &FromEngine::Frame(frame.clone())).unwrap();
        }
        let slot = LatestFrame::default();
        assert!(paint_all(&slot, &frames));
        assert!(slot.picture() == picture);
    }

    #[test]
    fn regions_fill_a_message_until_the_next_would_not_fit() {
        let (width, height) = (3000_u32, 7000_u32);
        let picture = Picture {
            width,
            height,
            bgra: vec![1; (width * height * 4) as usize],
        };
        // Four 12 MB regions fit one message of up to 64 MiB.
        let mut damage = Damage::default();
        for y in 0..4 {
            damage.add(Region {
                x: 0,
                y: y * 2000,
                width,
                height: 1000,
            });
        }
        let frames = frame_regions(VIEW, &picture, &damage);
        let counts: Vec<usize> = frames.iter().map(|frame| frame.regions.len()).collect();
        assert_eq!(counts, vec![4]);
        // Two 36 MB regions do not fit one message together: each gets its own.
        let mut damage = Damage::default();
        damage.add(Region {
            x: 0,
            y: 0,
            width,
            height: 3000,
        });
        damage.add(Region {
            x: 0,
            y: 3500,
            width,
            height: 3000,
        });
        let frames = frame_regions(VIEW, &picture, &damage);
        let counts: Vec<usize> = frames.iter().map(|frame| frame.regions.len()).collect();
        assert_eq!(counts, vec![1, 1]);
        assert_eq!(
            frames[0].regions[0].0,
            Region {
                x: 0,
                y: 0,
                width,
                height: 3000
            }
        );
    }

    #[test]
    fn a_frame_region_whose_rows_do_not_fill_it_is_not_written() {
        let frame = FrameRegions {
            view: VIEW,
            width: 2,
            height: 2,
            regions: vec![(Region::whole(2, 2), vec![0; 15])],
        };
        let mut out = Vec::new();
        assert!(matches!(
            write_from_engine(&mut out, &FromEngine::Frame(frame)),
            Err(ProtocolError::Malformed(_))
        ));
        assert!(out.is_empty());
    }

    #[test]
    fn messages_are_packed_up_to_the_limit() {
        // One column of 4-byte pixels: bands of whole rows leave less than a row unused.
        let height = 18_000_000_u32;
        let picture = Picture {
            width: 1,
            height,
            bgra: vec![3; height as usize * 4],
        };
        let mut damage = Damage::default();
        damage.add(Region::whole(1, height));
        let frames = frame_regions(VIEW, &picture, &damage);
        let sizes: Vec<usize> = frames
            .iter()
            .map(|frame| from_bytes(&FromEngine::Frame(frame.clone())).len() - 4)
            .collect();
        assert!(sizes.iter().all(|size| *size <= MAX_MESSAGE as usize));
        assert!(sizes[0] + 4 > MAX_MESSAGE as usize);
        assert_eq!(sizes.len(), 2);
    }
}
