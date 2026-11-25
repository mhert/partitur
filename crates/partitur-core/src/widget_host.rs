//! The port to the engine that renders widget pages.
//!
//! Like [`Backend`](crate::Backend), every method only queues a command and returns at once.
//! What the engine produces later (frames, the pointer's look, messages from the page, links
//! the user followed in it, failures) arrives as [`WidgetHostEvent`]s, so the core never waits
//! on the engine.
//!
//! A page never navigates away from its own document. When the user follows a link in it, or
//! it opens a window on a click, the engine keeps the page where it is and reports the URL as
//! [`WidgetHostEvent::OpenLink`], for the UI to open in the system's browser.
//!
//! A page follows the color scheme it is given: it opens in its [`WidgetPage::theme`], and
//! [`WidgetHost::set_theme`] changes it later. The engine shows the theme to the page as its
//! `prefers-color-scheme` media feature, so a plain page that knows nothing of the MCP Apps
//! bridge follows the desktop all the same.
//!
//! An engine paints a view as often as its page changes, which may be faster than the UI takes
//! the pictures. So a view's pictures pass through one [`LatestFrame`]. It holds the view's
//! whole [`Picture`] as the engine paints it, in BGRA with premultiplied alpha, and the
//! [`Damage`]: the regions painted since the UI last took the picture. A paint copies only the
//! regions it changed into the picture and adds them to the damage; a paint of another size
//! replaces the picture, all of it damaged. The engine sends a [`WidgetHostEvent::Frame`] only
//! when its paint found the damage empty, so however fast it paints, at most one frame event
//! per view waits. Taking the picture it announces ([`LatestFrame::take`]) yields the damage and
//! empties it. The UI then reads the newest pixels in place, under the slot's lock
//! ([`LatestFrame::read`]), and has to redraw only the damaged regions of what it showed. No
//! side holds the lock for longer than it takes to copy the pixels it changes or reads, and no
//! paint makes a picture anew unless its size changed.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use thiserror::Error;

use crate::widget_bridge::WidgetTheme;

/// Names one widget view across the host's commands and events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewId(pub u64);

/// The page a widget view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetPage {
    /// The HTML document.
    pub html: String,
    /// The content security policy the page runs under.
    pub csp: String,
    /// The color scheme the page follows.
    pub theme: WidgetTheme,
}

/// A pointer event to forward to a widget view. Widgets are read-only, so there is no keyboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WidgetInput {
    /// The pointer moved.
    Move {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
    },
    /// A button went down.
    Press {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// The button number: 0 for the left button, 1 for the middle one, 2 for the right one.
        button: u8,
    },
    /// A button went up.
    Release {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// The button number: 0 for the left button, 1 for the middle one, 2 for the right one.
        button: u8,
    },
    /// The wheel turned.
    Wheel {
        /// Horizontal position in view pixels.
        x: f32,
        /// Vertical position in view pixels.
        y: f32,
        /// Horizontal scroll amount in view pixels, positive to the left.
        dx: f32,
        /// Vertical scroll amount in view pixels, positive upwards.
        dy: f32,
        /// Whether the amounts are exact, as a touchpad gives them, rather than whole wheel
        /// notches, which the engine may animate.
        precise: bool,
    },
    /// The pointer left the view.
    Leave,
}

/// The pointer's look a page asks for over a view, as far as the UI can show it. Engines map
/// every look not named here to [`WidgetCursor::Default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WidgetCursor {
    /// The ordinary arrow.
    #[default]
    Default,
    /// The hand over a link or a button.
    Pointer,
    /// The I-beam over text.
    Text,
    /// An open hand over something that can be dragged.
    Grab,
    /// A closed hand while dragging.
    Grabbing,
    /// What may not be done here.
    NotAllowed,
    /// A sideways resize.
    ResizeHorizontal,
    /// An up-and-down resize.
    ResizeVertical,
    /// A crosshair.
    Crosshair,
}

/// A rectangle of a view's picture, in physical pixels from its top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Region {
    /// The left edge.
    pub x: u32,
    /// The top edge.
    pub y: u32,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
}

impl Region {
    /// The whole of a picture `width` by `height` pixels large.
    pub fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The part of the region inside a picture `width` by `height` pixels large, or `None` when
    /// no pixel of it is.
    pub fn within(self, width: u32, height: u32) -> Option<Self> {
        let right = self.right().min(width);
        let bottom = self.bottom().min(height);
        (self.x < right && self.y < bottom).then(|| Self {
            x: self.x,
            y: self.y,
            width: right - self.x,
            height: bottom - self.y,
        })
    }

    /// Whether `other` lies wholly inside this region.
    fn contains(self, other: Self) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// The smallest region that holds both.
    fn union(self, other: Self) -> Self {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Self {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }

    /// The column just right of the region.
    fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// The row just below the region.
    fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }
}

/// How many separate regions a [`Damage`] keeps before it holds their union instead.
pub const MAX_REGIONS: usize = 4;

/// The regions of a view's picture painted since the UI last took it: at most [`MAX_REGIONS`],
/// none of them empty or inside another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Damage(Vec<Region>);

impl Damage {
    /// The regions.
    pub fn regions(&self) -> &[Region] {
        &self.0
    }

    /// Whether nothing was painted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds `region`. An empty one, or one inside a region already there, adds nothing; the
    /// regions inside it give way to it. Past [`MAX_REGIONS`] the damage becomes the union of
    /// all.
    pub fn add(&mut self, region: Region) {
        if region.width == 0
            || region.height == 0
            || self.0.iter().any(|kept| kept.contains(region))
        {
            return;
        }
        self.0.retain(|kept| !region.contains(*kept));
        self.0.push(region);
        if self.0.len() > MAX_REGIONS {
            let union = self.0.iter().copied().reduce(Region::union);
            self.0.clear();
            self.0.extend(union);
        }
    }
}

/// The whole picture of a view, in BGRA with premultiplied alpha as engines paint it: four bytes
/// a pixel, row by row.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Picture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixels.
    pub bgra: Vec<u8>,
}

impl Picture {
    /// The pixels in RGBA with straight alpha, as images are drawn from.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.bgra.len());
        for pixel in self.bgra.chunks_exact(4) {
            if let &[b, g, r, a] = pixel {
                rgba.extend_from_slice(&[straight(r, a), straight(g, a), straight(b, a), a]);
            }
        }
        rgba
    }
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bgra.len())
            .finish()
    }
}

/// A colour `channel` premultiplied by `alpha`, divided by it again and rounded; a channel of
/// a transparent pixel is 0, and one greater than its alpha, which engines never paint, is full.
fn straight(channel: u8, alpha: u8) -> u8 {
    if alpha == 0 {
        return 0;
    }
    let alpha = u16::from(alpha);
    u8::try_from((u16::from(channel) * 255 + alpha / 2) / alpha).unwrap_or(u8::MAX)
}

/// The number of bytes of a picture `width` by `height` pixels large, if it fits in memory.
fn byte_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

/// Copies `region` of the picture `from` into the picture `into`, both `width` pixels wide and of
/// the same size, the region inside them.
fn copy_region(into: &mut [u8], from: &[u8], width: u32, region: Region) {
    let Some(len) = byte_len(region.width, 1) else {
        return;
    };
    for y in region.y..region.bottom() {
        let Some(at) = byte_len(width, y)
            .zip(byte_len(region.x, 1))
            .map(|(row, x)| row + x)
        else {
            return;
        };
        if let (Some(into), Some(from)) = (into.get_mut(at..at + len), from.get(at..at + len)) {
            into.copy_from_slice(from);
        }
    }
}

/// A view's picture and what of it changed since the UI last took it.
#[derive(Default)]
struct Canvas {
    /// The picture.
    picture: Picture,
    /// The regions painted since the last take.
    damage: Damage,
}

/// The slot through which a view's pictures pass from the engine to the UI; see the module
/// documentation. Clones share the one slot, and two slots are equal when they are the same.
#[derive(Clone, Default)]
pub struct LatestFrame(Arc<Mutex<Canvas>>);

impl LatestFrame {
    /// Copies the regions `dirty` of a paint `width` by `height` pixels large into the view's
    /// picture and adds them to the damage, `pixels` being the whole paint in BGRA with
    /// premultiplied alpha. A paint of another size than the picture replaces it, all of it
    /// damaged and transparent where `dirty` does not reach. Regions are cut off at the paint's
    /// edges, and a paint whose pixels do not fill its size is dropped. Returns whether the
    /// damage was empty and no longer is, which is when a [`WidgetHostEvent::Frame`] must
    /// announce it.
    pub fn paint(
        &self,
        width: u32,
        height: u32,
        pixels: &[u8],
        dirty: impl IntoIterator<Item = Region>,
    ) -> bool {
        if byte_len(width, height) != Some(pixels.len()) {
            return false;
        }
        let mut canvas = self.lock();
        let was_empty = canvas.damage.is_empty();
        let Canvas { picture, damage } = &mut *canvas;
        if (picture.width, picture.height) != (width, height) {
            picture.width = width;
            picture.height = height;
            picture.bgra.clear();
            picture.bgra.resize(pixels.len(), 0);
            *damage = Damage::default();
            damage.add(Region::whole(width, height));
        }
        for region in dirty
            .into_iter()
            .filter_map(|region| region.within(width, height))
        {
            copy_region(&mut picture.bgra, pixels, width, region);
            damage.add(region);
        }
        was_empty && !damage.is_empty()
    }

    /// Paints `picture` whole; see [`paint`](Self::paint).
    pub fn put(&self, picture: &Picture) -> bool {
        self.paint(
            picture.width,
            picture.height,
            &picture.bgra,
            [Region::whole(picture.width, picture.height)],
        )
    }

    /// Takes the damage painted since the last take and empties it, or `None` when nothing
    /// was painted.
    pub fn take(&self) -> Option<Damage> {
        let mut canvas = self.lock();
        (!canvas.damage.is_empty()).then(|| std::mem::take(&mut canvas.damage))
    }

    /// The width and height of the picture.
    pub fn size(&self) -> (u32, u32) {
        let canvas = self.lock();
        (canvas.picture.width, canvas.picture.height)
    }

    /// Calls `read` with the newest picture, under the slot's lock: the engine cannot paint
    /// until it returns, so it should do no more than copy what it needs.
    pub fn read<R>(&self, read: impl FnOnce(&Picture) -> R) -> R {
        read(&self.lock().picture)
    }

    /// A copy of the newest picture.
    pub fn picture(&self) -> Picture {
        self.read(Picture::clone)
    }

    /// The slot, also after a thread panicked while holding it: a picture is whole either way,
    /// if partly older.
    fn lock(&self) -> MutexGuard<'_, Canvas> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for LatestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LatestFrame").finish_non_exhaustive()
    }
}

impl PartialEq for LatestFrame {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LatestFrame {}

/// Something the engine reports about a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetHostEvent {
    /// The view has a new picture, to be taken from `latest`.
    Frame {
        /// The view it belongs to.
        view: ViewId,
        /// The view's slot, which holds the newest picture.
        latest: LatestFrame,
    },
    /// The page asks for another look of the pointer over the view.
    Cursor {
        /// The view it belongs to.
        view: ViewId,
        /// The look.
        cursor: WidgetCursor,
    },
    /// The page sent a message.
    Message {
        /// The view it came from.
        view: ViewId,
        /// The message, as JSON text.
        json: String,
    },
    /// The user followed a link in the page, or the page opened a window on a click, and the
    /// page stayed where it was: the URL is for the system to open.
    OpenLink {
        /// The view it came from.
        view: ViewId,
        /// The URL, as the page named it.
        url: String,
    },
    /// The view could not be shown or broke.
    Failed {
        /// The view that failed.
        view: ViewId,
        /// What went wrong.
        error: String,
    },
}

/// Why a [`WidgetHost`] refused a command.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum WidgetHostError {
    /// There is no engine to run widgets.
    #[error("widget host unavailable: {0}")]
    Unavailable(String),
    /// The command could not be handed to the engine.
    #[error("sending to the widget host failed: {0}")]
    Send(String),
}

/// The command port to the widget engine. Every method queues and returns at once.
pub trait WidgetHost: Send {
    /// Opens `view` on `page`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn open(&self, view: ViewId, page: WidgetPage) -> Result<(), WidgetHostError>;

    /// Delivers a JSON message to the page of `view`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn post(&self, view: ViewId, json: String) -> Result<(), WidgetHostError>;

    /// Resizes `view` to `width` by `height` pixels at the device pixel ratio `scale`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn resize(
        &self,
        view: ViewId,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<(), WidgetHostError>;

    /// Shows or hides `view`; a hidden view stops producing frames.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn set_visible(&self, view: ViewId, visible: bool) -> Result<(), WidgetHostError>;

    /// Forwards a pointer event to `view`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn input(&self, view: ViewId, input: WidgetInput) -> Result<(), WidgetHostError>;

    /// Lets the page of `view` follow `theme` from now on, as its `prefers-color-scheme`.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn set_theme(&self, view: ViewId, theme: WidgetTheme) -> Result<(), WidgetHostError>;

    /// Closes `view` and frees what it held.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetHostError`] when the command cannot be queued.
    fn close(&self, view: ViewId) -> Result<(), WidgetHostError>;
}

/// The [`WidgetHost`] used when the engine does not start: every `open` is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWidgetHost;

impl WidgetHost for NoWidgetHost {
    fn open(&self, _view: ViewId, _page: WidgetPage) -> Result<(), WidgetHostError> {
        Err(WidgetHostError::Unavailable(
            "widgets are not available".into(),
        ))
    }

    fn post(&self, _view: ViewId, _json: String) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn resize(
        &self,
        _view: ViewId,
        _width: u32,
        _height: u32,
        _scale: f32,
    ) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn set_visible(&self, _view: ViewId, _visible: bool) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn input(&self, _view: ViewId, _input: WidgetInput) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn set_theme(&self, _view: ViewId, _theme: WidgetTheme) -> Result<(), WidgetHostError> {
        Ok(())
    }

    fn close(&self, _view: ViewId) -> Result<(), WidgetHostError> {
        Ok(())
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
    use crate::testing::{RecordingWidgetHost, WidgetCall};

    fn page() -> WidgetPage {
        WidgetPage {
            html: "<p>hi</p>".into(),
            csp: "default-src 'none'".into(),
            theme: WidgetTheme::Dark,
        }
    }

    fn region(x: u32, y: u32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    /// A `width` by `height` paint whose every byte is `byte`.
    fn filled(width: u32, height: u32, byte: u8) -> Vec<u8> {
        vec![byte; byte_len(width, height).unwrap()]
    }

    fn damage(regions: &[Region]) -> Damage {
        let mut damage = Damage::default();
        for region in regions {
            damage.add(*region);
        }
        damage
    }

    #[test]
    fn a_region_is_cut_off_at_the_edges_of_its_picture() {
        assert_eq!(region(1, 1, 2, 2).within(4, 4), Some(region(1, 1, 2, 2)));
        assert_eq!(region(1, 1, 3, 3).within(4, 4), Some(region(1, 1, 3, 3)));
        assert_eq!(region(2, 1, 5, 9).within(4, 3), Some(region(2, 1, 2, 2)));
        assert_eq!(region(3, 2, 1, 1).within(4, 3), Some(region(3, 2, 1, 1)));
        assert_eq!(
            region(1, 2, u32::MAX, u32::MAX).within(4, 3),
            Some(region(1, 2, 3, 1))
        );
        assert_eq!(region(4, 0, 1, 1).within(4, 3), None);
        assert_eq!(region(0, 3, 1, 1).within(4, 3), None);
        assert_eq!(region(1, 1, 0, 1).within(4, 3), None);
        assert_eq!(region(1, 1, 1, 0).within(4, 3), None);
        assert_eq!(Region::whole(4, 3), region(0, 0, 4, 3));
    }

    #[test]
    fn damage_keeps_separate_regions_none_inside_another() {
        let mut damage = Damage::default();
        assert!(damage.is_empty());
        damage.add(region(0, 0, 0, 5));
        damage.add(region(0, 0, 5, 0));
        assert!(damage.is_empty(), "an empty region adds nothing");

        damage.add(region(2, 2, 4, 4));
        damage.add(region(3, 3, 2, 2));
        damage.add(region(2, 2, 4, 4));
        damage.add(region(2, 2, 4, 3));
        damage.add(region(3, 2, 3, 4));
        assert_eq!(damage.regions(), [region(2, 2, 4, 4)], "inside it, nothing");

        damage.add(region(5, 5, 2, 1));
        damage.add(region(1, 2, 5, 4));
        damage.add(region(2, 1, 4, 5));
        assert_eq!(
            damage.regions(),
            [region(5, 5, 2, 1), region(1, 2, 5, 4), region(2, 1, 4, 5)],
            "each sticks out of the others on one side"
        );
        damage.add(region(1, 1, 6, 6));
        assert_eq!(
            damage.regions(),
            [region(1, 1, 6, 6)],
            "the regions inside it give way"
        );
    }

    #[test]
    fn damage_past_its_limit_becomes_the_union() {
        let separate = [
            region(0, 0, 1, 1),
            region(10, 0, 1, 1),
            region(0, 10, 1, 1),
            region(5, 5, 2, 3),
        ];
        let mut damage = damage(&separate);
        assert_eq!(damage.regions(), separate);
        assert_eq!(damage.regions().len(), MAX_REGIONS);
        damage.add(region(8, 9, 4, 1));
        assert_eq!(damage.regions(), [region(0, 0, 12, 11)]);
        damage.add(region(20, 20, 1, 1));
        assert_eq!(
            damage.regions(),
            [region(0, 0, 12, 11), region(20, 20, 1, 1)]
        );
    }

    #[test]
    fn a_union_reaches_the_far_edges_of_both() {
        assert_eq!(
            region(3, 1, 2, 2).union(region(1, 4, 1, 3)),
            region(1, 1, 4, 6)
        );
        assert_eq!(
            region(1, 4, 1, 3).union(region(3, 1, 2, 2)),
            region(1, 1, 4, 6)
        );
    }

    #[test]
    fn a_first_paint_makes_the_picture_and_is_announced() {
        let latest = LatestFrame::default();
        assert_eq!(latest.take(), None);
        assert_eq!(latest.size(), (0, 0));
        let pixels: Vec<u8> = (0..48).collect();
        assert!(latest.paint(4, 3, &pixels, [region(1, 1, 2, 1)]));
        assert_eq!(latest.size(), (4, 3));
        let mut expected = filled(4, 3, 0);
        expected[20..28].copy_from_slice(&pixels[20..28]);
        assert_eq!(latest.picture().bgra, expected, "only the dirty pixels");
        assert_eq!(
            latest.take(),
            Some(damage(&[Region::whole(4, 3)])),
            "a new picture is damaged whole"
        );
        assert_eq!(latest.take(), None, "a take empties the damage");
    }

    #[test]
    fn paints_copy_their_dirty_rows_and_gather_damage_until_taken() {
        let latest = LatestFrame::default();
        assert!(latest.put(&Picture {
            width: 4,
            height: 3,
            bgra: filled(4, 3, 1),
        }));
        assert!(latest.take().is_some());

        let pixels: Vec<u8> = (0..48).collect();
        assert!(latest.paint(4, 3, &pixels, [region(2, 0, 2, 2)]));
        assert!(
            !latest.paint(
                4,
                3,
                &filled(4, 3, 9),
                [region(0, 2, 1, 9), region(7, 0, 1, 1)]
            ),
            "a paint before the take is not announced again"
        );
        let mut expected = filled(4, 3, 1);
        expected[8..16].copy_from_slice(&pixels[8..16]);
        expected[24..32].copy_from_slice(&pixels[24..32]);
        expected[32..36].copy_from_slice(&[9; 4]);
        assert_eq!(latest.read(|picture| picture.bgra.clone()), expected);
        assert_eq!(
            latest.take(),
            Some(damage(&[region(2, 0, 2, 2), region(0, 2, 1, 1)]))
        );
        assert!(
            latest.clone().paint(4, 3, &pixels, [region(0, 0, 1, 1)]),
            "the first paint after a take is announced"
        );
    }

    #[test]
    fn a_paint_that_changes_nothing_is_not_announced() {
        let latest = LatestFrame::default();
        latest.put(&Picture {
            width: 2,
            height: 2,
            bgra: filled(2, 2, 1),
        });
        latest.take();
        assert!(!latest.paint(2, 2, &filled(2, 2, 5), [region(2, 0, 1, 1)]));
        assert!(!latest.paint(2, 2, &filled(2, 2, 5), []));
        assert_eq!(latest.take(), None);
        assert_eq!(latest.picture().bgra, filled(2, 2, 1));
    }

    #[test]
    fn a_paint_that_does_not_fill_its_size_is_dropped() {
        let latest = LatestFrame::default();
        assert!(!latest.paint(2, 2, &filled(2, 1, 5), [Region::whole(2, 2)]));
        assert!(!latest.paint(2, 2, &[5; 17], [Region::whole(2, 2)]));
        assert!(!latest.paint(u32::MAX, u32::MAX, &[], [Region::whole(1, 1)]));
        assert_eq!(latest.size(), (0, 0));
        assert_eq!(latest.take(), None);
        assert!(!latest.paint(0, 0, &[], [Region::whole(1, 1)]));
        assert_eq!(latest.take(), None, "an empty paint damages nothing");
    }

    #[test]
    fn a_paint_of_another_size_replaces_the_picture_whole() {
        let latest = LatestFrame::default();
        latest.put(&Picture {
            width: 2,
            height: 1,
            bgra: filled(2, 1, 7),
        });
        latest.take();
        assert!(latest.paint(2, 1, &filled(2, 1, 8), [region(1, 0, 1, 1)]));
        assert!(
            !latest.paint(1, 1, &filled(1, 1, 3), []),
            "announced already"
        );
        assert_eq!(latest.picture().bgra, filled(1, 1, 0), "transparent anew");
        assert_eq!(
            latest.take(),
            Some(damage(&[Region::whole(1, 1)])),
            "the old damage goes with the old picture"
        );
        assert!(latest.paint(3, 1, &filled(3, 1, 4), [region(2, 0, 1, 1)]));
        assert_eq!(latest.picture().bgra, [0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 4, 4]);
    }

    #[test]
    fn premultiplied_bgra_becomes_straight_rgba() {
        let picture = |bgra: &[u8]| Picture {
            width: 1,
            height: 1,
            bgra: bgra.to_vec(),
        };
        assert_eq!(
            picture(&[1, 2, 3, 255, 10, 20, 30, 255]).to_rgba(),
            vec![3, 2, 1, 255, 30, 20, 10, 255]
        );
        assert_eq!(picture(&[1, 2, 3]).to_rgba(), Vec::<u8>::new());
        // Half-transparent red, and a quarter-transparent grey.
        assert_eq!(picture(&[0, 0, 128, 128]).to_rgba(), vec![255, 0, 0, 128]);
        assert_eq!(picture(&[48, 48, 48, 192]).to_rgba(), vec![64, 64, 64, 192]);
        // Rounded to the nearest, not down.
        assert_eq!(straight(1, 3), 85);
        assert_eq!(straight(2, 3), 170);
        assert_eq!(straight(1, 2), 128);
        // Opaque pixels keep their colour, transparent ones have none.
        assert_eq!(straight(77, 255), 77);
        assert_eq!(straight(77, 0), 0);
        assert_eq!(straight(0, 0), 0);
        // A channel greater than its alpha is out of range, and full.
        assert_eq!(straight(200, 100), 255);
        assert_eq!(straight(100, 100), 255);
    }

    #[test]
    fn pictures_and_slots_show_no_pixels_when_printed() {
        let picture = Picture {
            width: 2,
            height: 1,
            bgra: filled(2, 1, 0),
        };
        assert_eq!(
            format!("{picture:?}"),
            "Picture { width: 2, height: 1, bytes: 8 }"
        );
        assert_eq!(
            format!("{:?}", LatestFrame::default()),
            "LatestFrame { .. }"
        );
    }

    #[test]
    fn latest_frames_are_equal_when_they_are_the_same_slot() {
        let latest = LatestFrame::default();
        assert_eq!(latest, latest.clone());
        assert_ne!(latest, LatestFrame::default());
    }

    #[test]
    fn no_widget_host_refuses_open() {
        let host = NoWidgetHost;
        assert_eq!(
            host.open(ViewId(1), page()),
            Err(WidgetHostError::Unavailable(
                "widgets are not available".into()
            ))
        );
        let view = ViewId(1);
        assert_eq!(host.post(view, "{}".into()), Ok(()));
        assert_eq!(host.resize(view, 1, 2, 1.0), Ok(()));
        assert_eq!(host.set_visible(view, true), Ok(()));
        assert_eq!(host.input(view, WidgetInput::Leave), Ok(()));
        assert_eq!(host.close(view), Ok(()));
    }

    #[test]
    fn no_widget_host_takes_a_theme() {
        assert_eq!(
            NoWidgetHost.set_theme(ViewId(1), WidgetTheme::Light),
            Ok(())
        );
        assert_eq!(NoWidgetHost.set_theme(ViewId(1), WidgetTheme::Dark), Ok(()));
    }

    #[test]
    fn recording_widget_host_records_in_order() {
        let host = RecordingWidgetHost::new();
        let view = ViewId(7);
        let input = WidgetInput::Press {
            x: 1.0,
            y: 2.0,
            button: 0,
        };
        host.open(view, page()).unwrap();
        host.post(view, "{\"a\":1}".into()).unwrap();
        host.resize(view, 320, 200, 2.0).unwrap();
        host.set_visible(view, false).unwrap();
        host.input(view, input).unwrap();
        host.set_theme(view, WidgetTheme::Light).unwrap();
        host.close(view).unwrap();
        assert_eq!(
            host.calls(),
            vec![
                WidgetCall::Open { view, page: page() },
                WidgetCall::Post {
                    view,
                    json: "{\"a\":1}".into()
                },
                WidgetCall::Resize {
                    view,
                    width: 320,
                    height: 200,
                    scale: 2.0
                },
                WidgetCall::SetVisible {
                    view,
                    visible: false
                },
                WidgetCall::Input { view, input },
                WidgetCall::SetTheme {
                    view,
                    theme: WidgetTheme::Light
                },
                WidgetCall::Close { view },
            ]
        );
    }

    #[test]
    fn recording_widget_host_fails_on_request() {
        let host = RecordingWidgetHost::new();
        let view = ViewId(1);
        host.fail_next_call();
        assert_eq!(
            host.close(view),
            Err(WidgetHostError::Unavailable("recording widget host".into()))
        );
        assert_eq!(host.close(view), Ok(()));

        host.fail_next_call_with(WidgetHostError::Send("gone".into()));
        assert_eq!(host.close(view), Err(WidgetHostError::Send("gone".into())));
        assert_eq!(host.close(view), Ok(()));

        host.fail_call_after(2);
        assert_eq!(host.close(view), Ok(()));
        assert_eq!(host.close(view), Ok(()));
        assert!(host.close(view).is_err());
        assert_eq!(host.close(view), Ok(()));
        assert_eq!(host.calls().len(), 8);
    }

    #[test]
    fn recording_widget_host_clones_share_the_recording() {
        let host = RecordingWidgetHost::new();
        host.clone().close(ViewId(3)).unwrap();
        assert_eq!(host.calls(), vec![WidgetCall::Close { view: ViewId(3) }]);
    }
}
