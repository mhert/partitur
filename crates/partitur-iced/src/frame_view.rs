//! The newest picture of a widget engine's view, drawn pixel for pixel.
//!
//! The engine paints a view at the window's scale factor, so one pixel of its picture is meant
//! for one pixel of the screen. [`frame_view`] draws it that way: the picture's top-left pixel
//! sits on the screen pixel nearest the top-left corner of the widget's bounds, rounded the
//! way the renderer rounds the bounds to the screen's pixels, and every pixel after it goes to
//! the next screen pixel, unscaled and unfiltered. A view's logical size is often a fraction of
//! a pixel off what the engine painted, and scaling the picture into it would blur every glyph
//! by a fraction of a pixel. Where the picture is smaller than the bounds, what lies under the
//! widget shows; what of it reaches past the bounds is cut off.
//!
//! A [`Frame`] does not hold the pixels: it names the view's [`LatestFrame`], which keeps the
//! picture the engine paints in BGRA with premultiplied alpha, and the [`Damage`] the engine
//! painted since the frame before. On the GPU (`wgpu`) each view keeps one texture of the size
//! of its picture ([`gpu`]). While the renderer prepares the frame that first draws a new
//! picture, before that frame is drawn, the damaged regions are copied from the slot into the
//! texture, so a new picture never leaves a frame blank and its pixels are never converted; the
//! whole picture is written only into a new texture, or when the texture missed a picture
//! between, and an unchanged picture is not written again. A texture of another size is made
//! only when the picture's size changes, and a view not drawn in a frame gives its texture up
//! after it. On the software renderer (tiny-skia), which the tests and machines without a
//! usable GPU use, the picture is turned into an image in RGBA with straight alpha once, at its
//! first draw, and drawn at its own size, its pixels divided by the scale factor in logical
//! pixels, with the nearest pixel taken, and cut off at the bounds.

mod gpu;

use iced::advanced::image::{self as image_renderer, FilterMethod, Image};
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Widget, renderer};
use iced::widget::image::Handle;
use iced::widget::shader;
use iced::{Element, Length, Radians, Rectangle, Renderer, Size, Theme, mouse};
use std::sync::{Arc, OnceLock};

use partitur_core::widget_host::{Damage, LatestFrame, ViewId};

/// One picture of a view, as the transcript and the viewer draw it.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    /// The view the picture is of.
    view: ViewId,
    /// Counts the view's pictures: a later picture has a greater one.
    generation: u64,
    /// The regions painted since the picture before.
    damage: Damage,
    /// The view's slot, which holds its newest pixels.
    latest: LatestFrame,
    /// The picture as the software renderer draws it, with its size, made at its first draw.
    image: Arc<OnceLock<(Handle, (u32, u32))>>,
}

impl Frame {
    /// The frame of the picture of `view` in `latest`, the view's picture numbered
    /// `generation`, `damage` being the regions painted since the picture before.
    pub(crate) fn new(view: ViewId, generation: u64, latest: LatestFrame, damage: Damage) -> Self {
        Self {
            view,
            generation,
            damage,
            latest,
            image: Arc::default(),
        }
    }

    /// The number of the view's picture this is.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The width and height of the slot's newest picture in physical pixels.
    #[cfg(test)]
    pub(crate) fn size(&self) -> (u32, u32) {
        self.latest.size()
    }

    /// The slot's newest pixels, in BGRA with premultiplied alpha.
    #[cfg(test)]
    pub(crate) fn pixels(&self) -> Vec<u8> {
        self.latest.picture().bgra
    }

    /// The regions painted since the picture before.
    pub(crate) fn damage(&self) -> &Damage {
        &self.damage
    }

    /// The view's slot, which holds its newest pixels.
    pub(crate) fn latest(&self) -> &LatestFrame {
        &self.latest
    }

    /// The picture as an image in RGBA with straight alpha, and its size, for the software
    /// renderer: made from the slot's newest pixels the first time it is asked for.
    fn image(&self) -> &(Handle, (u32, u32)) {
        self.image.get_or_init(|| {
            let picture = self.latest.picture();
            let size = (picture.width, picture.height);
            (Handle::from_rgba(size.0, size.1, picture.to_rgba()), size)
        })
    }
}

/// Draws `frame` pixel for pixel in a window of the scale factor `scale`, in bounds `width`
/// wide and `height` tall; see the module documentation.
pub(crate) fn frame_view<'a, Message: 'a>(
    frame: &Frame,
    scale: f32,
    width: Length,
    height: Length,
) -> Element<'a, Message> {
    Element::new(FrameView {
        gpu: shader(gpu::FrameProgram::new(frame))
            .width(width)
            .height(height)
            .into(),
        frame: frame.clone(),
        scale,
        width,
        height,
    })
}

/// The widget [`frame_view`] makes.
struct FrameView<'a, Message> {
    /// The shader widget that draws the picture on the GPU.
    gpu: Element<'a, Message>,
    /// The picture.
    frame: Frame,
    /// The window's scale factor, which the software renderer divides the picture by.
    scale: f32,
    /// The width of the bounds.
    width: Length,
    /// The height of the bounds.
    height: Length,
}

impl<Message> Widget<Message, Theme, Renderer> for FrameView<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.gpu)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.gpu));
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.width, self.height)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        match renderer {
            Renderer::Primary(_) => {
                if let Some(child) = tree.children.first() {
                    self.gpu
                        .as_widget()
                        .draw(child, renderer, theme, style, layout, cursor, viewport);
                }
            }
            Renderer::Secondary(_) => {
                let (handle, size) = self.frame.image();
                // The renderer does not cut an image off at its clip bounds, but at its layer's.
                iced::advanced::Renderer::with_layer(renderer, bounds, |renderer| {
                    image_renderer::Renderer::draw_image(
                        renderer,
                        Image {
                            handle: handle.clone(),
                            filter_method: FilterMethod::Nearest,
                            rotation: Radians(0.0),
                            border_radius: iced::border::Radius::default(),
                            opacity: 1.0,
                            snap: true,
                        },
                        at_own_size(bounds, *size, self.scale),
                        bounds,
                    );
                });
            }
        }
    }
}

/// Where a picture `size` physical pixels large is drawn at its own size in a window of the
/// scale factor `scale`: at the top-left corner of `bounds`, in logical pixels. A scale factor
/// that is not positive counts as 1.
#[expect(
    clippy::cast_precision_loss,
    reason = "a picture's side is far below the 2^24 pixels an f32 holds exactly"
)]
fn at_own_size(bounds: Rectangle, (width, height): (u32, u32), scale: f32) -> Rectangle {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    Rectangle {
        x: bounds.x,
        y: bounds.y,
        width: width as f32 / scale,
        height: height as f32 / scale,
    }
}

#[cfg(test)]
mod tests;
