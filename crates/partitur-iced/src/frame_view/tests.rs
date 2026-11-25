#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use iced::advanced::renderer::{Headless, Style};
use iced::widget::container;
use iced::{Color, Element, Length, Size, Theme, mouse};
use iced_test::runtime::user_interface::{self, UserInterface};
use partitur_core::testing::premultiplied;
use partitur_core::widget_host::{Picture, Region};

use super::*;

/// The window's size in logical pixels.
const WINDOW: f32 = 12.0;

/// The colour of the window behind the picture.
const BACKGROUND: [u8; 4] = [0, 0, 0, 255];

/// The headless renderer of `backend`: `tiny-skia` or `wgpu`.
fn renderer(backend: &str) -> Renderer {
    futures::executor::block_on(<Renderer as Headless>::new(
        iced::Font::DEFAULT,
        iced::Pixels(16.0),
        Some(backend),
    ))
    .unwrap()
}

/// A `side` by `side` picture of view 1 whose every pixel has a colour of its own.
fn picture(side: u32) -> Frame {
    let mut rgba = Vec::new();
    for y in 0..side {
        for x in 0..side {
            rgba.extend_from_slice(&colour(x, y));
        }
    }
    framed(&premultiplied(side, side, &rgba))
}

/// The first frame of view 1 painted whole with `picture`.
fn framed(picture: &Picture) -> Frame {
    let latest = LatestFrame::default();
    assert!(latest.put(picture));
    let damage = latest.take().unwrap();
    Frame::new(ViewId(1), 1, latest, damage)
}

/// The colour of the picture's pixel `x`, `y`.
fn colour(x: u32, y: u32) -> [u8; 4] {
    [
        u8::try_from(40 + 60 * x).unwrap(),
        u8::try_from(40 + 60 * y).unwrap(),
        200,
        255,
    ]
}

/// The pixels of `frame` drawn in bounds `side` logical pixels large, `inset` logical pixels
/// from the window's top-left corner, in a window of the scale factor `scale`: the window's
/// physical width and its pixels in RGBA. The software renderer draws it.
fn drawn(frame: &Frame, scale: f32, inset: f32, side: f32) -> (usize, Vec<u8>) {
    drawn_by("tiny-skia", frame, scale, inset, side)
}

/// [`drawn`] by the renderer of `backend`.
fn drawn_by(backend: &str, frame: &Frame, scale: f32, inset: f32, side: f32) -> (usize, Vec<u8>) {
    let mut renderer = renderer(backend);
    assert_eq!(
        matches!(renderer, Renderer::Primary(_)),
        backend == "wgpu",
        "{backend} draws"
    );
    drawn_with(&mut renderer, frame, scale, inset, side)
}

/// [`drawn`] by `renderer`, which keeps what it made for frames drawn before.
fn drawn_with(
    renderer: &mut Renderer,
    frame: &Frame,
    scale: f32,
    inset: f32,
    side: f32,
) -> (usize, Vec<u8>) {
    let element: Element<'_, ()> = container(frame_view(
        frame,
        scale,
        Length::Fixed(side),
        Length::Fixed(side),
    ))
    .padding(inset)
    .into();
    let mut ui = UserInterface::build(
        element,
        Size::new(WINDOW, WINDOW),
        user_interface::Cache::default(),
        renderer,
    );
    ui.draw(
        renderer,
        &Theme::Dark,
        &Style {
            text_color: Color::WHITE,
        },
        mouse::Cursor::Unavailable,
    );
    let physical = (WINDOW * scale).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the window is a few pixels large"
    )]
    let screen = physical as u32;
    let pixels = renderer.screenshot(Size::new(screen, screen), scale, Color::BLACK);
    (usize::try_from(screen).unwrap(), pixels)
}

/// The pixel `x`, `y` of a screenshot `width` pixels wide.
fn pixel(pixels: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    let at = (y * width + x) * 4;
    pixels[at..at + 4].try_into().unwrap()
}

// At scale 1.5 a picture of 3 by 3 physical pixels covers 2 by 2 logical ones: each of its
// pixels is one pixel of the screen, from the top-left corner of the bounds on, and the rest
// of the bounds shows what lies below.
#[test]
fn a_picture_is_drawn_pixel_for_pixel_at_its_own_size() {
    let frame = picture(3);
    let (width, pixels) = drawn(&frame, 1.5, 2.0, 6.0);

    // The bounds start 2 logical pixels in, at the physical pixel 3.
    for y in 0..3 {
        for x in 0..3 {
            assert_eq!(
                pixel(&pixels, width, 3 + x, 3 + y),
                colour(u32::try_from(x).unwrap(), u32::try_from(y).unwrap()),
                "{x}, {y}"
            );
        }
    }
    for (x, y) in [(6, 3), (3, 6), (6, 6), (10, 10), (2, 3), (3, 2)] {
        assert_eq!(pixel(&pixels, width, x, y), BACKGROUND, "{x}, {y}");
    }
}

// The same on the GPU, which needs one: pixel for pixel from the corner, nothing beyond the
// picture or beyond the bounds.
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_draws_a_picture_pixel_for_pixel() {
    let frame = picture(4);
    let (width, pixels) = drawn_by("wgpu", &frame, 1.5, 2.0, 2.0);
    for y in 0..3 {
        for x in 0..3 {
            assert_eq!(
                pixel(&pixels, width, 3 + x, 3 + y),
                colour(u32::try_from(x).unwrap(), u32::try_from(y).unwrap()),
                "{x}, {y}"
            );
        }
    }
    for (x, y) in [(6, 3), (3, 6), (2, 3), (3, 2)] {
        assert_eq!(pixel(&pixels, width, x, y), BACKGROUND, "{x}, {y}");
    }

    let frame = picture(3);
    let (width, pixels) = drawn_by("wgpu", &frame, 1.5, 2.0, 6.0);
    for y in 0..3 {
        for x in 0..3 {
            assert_eq!(
                pixel(&pixels, width, 3 + x, 3 + y),
                colour(u32::try_from(x).unwrap(), u32::try_from(y).unwrap()),
                "{x}, {y}"
            );
        }
    }
    assert_eq!(pixel(&pixels, width, 6, 3), BACKGROUND);
}

// What of the picture reaches past its bounds is cut off.
#[test]
fn a_picture_larger_than_its_bounds_is_cut_off() {
    let frame = picture(4);
    // Bounds of 2 logical pixels are 3 physical ones at scale 1.5.
    let (width, pixels) = drawn(&frame, 1.5, 2.0, 2.0);

    for y in 0..3 {
        for x in 0..3 {
            assert_eq!(
                pixel(&pixels, width, 3 + x, 3 + y),
                colour(u32::try_from(x).unwrap(), u32::try_from(y).unwrap()),
                "{x}, {y}"
            );
        }
    }
    for (x, y) in [(6, 3), (3, 6), (6, 6)] {
        assert_eq!(pixel(&pixels, width, x, y), BACKGROUND, "{x}, {y}");
    }
}

#[test]
fn a_picture_is_placed_at_its_own_size_from_the_corner_of_its_bounds() {
    let bounds = Rectangle {
        x: 3.0,
        y: 4.0,
        width: 500.0,
        height: 100.0,
    };
    assert_eq!(
        at_own_size(bounds, (300, 150), 1.5),
        Rectangle {
            x: 3.0,
            y: 4.0,
            width: 200.0,
            height: 100.0,
        }
    );
    assert_eq!(
        at_own_size(bounds, (300, 150), 0.0),
        Rectangle {
            x: 3.0,
            y: 4.0,
            width: 300.0,
            height: 150.0,
        },
        "a scale factor that is not positive counts as 1"
    );
}

// The engine's pixels are BGRA with premultiplied alpha; the software renderer draws them as
// the colours they stand for: an opaque one as it is, a half-transparent red half over the
// black below.
#[test]
fn premultiplied_bgra_is_drawn_in_its_colours() {
    let frame = framed(&Picture {
        width: 2,
        height: 1,
        bgra: vec![200, 100, 40, 255, 0, 0, 128, 128],
    });
    let (width, pixels) = drawn(&frame, 1.0, 2.0, 6.0);
    assert_eq!(pixel(&pixels, width, 2, 2), [40, 100, 200, 255]);
    let [red, green, blue, alpha] = pixel(&pixels, width, 3, 2);
    assert!(
        (127..=129).contains(&red) && green == 0 && blue == 0 && alpha == 255,
        "{:?}",
        [red, green, blue, alpha]
    );
}

// The software renderer makes its image of a picture once, from the slot's pixels at the first
// draw, and keeps it for every draw of that frame after.
#[test]
fn the_software_image_is_made_once_per_frame() {
    let frame = framed(&premultiplied(1, 1, &[10, 20, 30, 255]));
    let (width, pixels) = drawn(&frame, 1.0, 0.0, 4.0);
    assert_eq!(pixel(&pixels, width, 0, 0), [10, 20, 30, 255]);

    frame.latest().put(&premultiplied(1, 1, &[90, 80, 70, 255]));
    let (width, pixels) = drawn(&frame, 1.0, 0.0, 4.0);
    assert_eq!(
        pixel(&pixels, width, 0, 0),
        [10, 20, 30, 255],
        "the same frame"
    );

    let damage = frame.latest().take().unwrap();
    let next = Frame::new(ViewId(1), 2, frame.latest().clone(), damage);
    let (width, pixels) = drawn(&next, 1.0, 0.0, 4.0);
    assert_eq!(
        pixel(&pixels, width, 0, 0),
        [90, 80, 70, 255],
        "the next frame"
    );
}

// On the GPU a later picture writes only its damage into the texture of the one before: a
// pixel painted since, whose damage comes with the frame after, still shows as it was, and
// half-transparent pixels blend as the renderer's own images do.
#[test]
#[ignore = "needs a GPU"]
fn the_gpu_writes_only_a_pictures_damage() {
    let mut renderer = renderer("wgpu");
    let first = picture(4);
    let (width, pixels) = drawn_with(&mut renderer, &first, 1.0, 2.0, 6.0);
    for (x, y) in [(0, 0), (1, 1), (2, 2), (3, 3)] {
        assert_eq!(
            pixel(&pixels, width, 2 + x, 2 + y),
            colour(u32::try_from(x).unwrap(), u32::try_from(y).unwrap())
        );
    }

    let latest = first.latest().clone();
    let changed = premultiplied(4, 4, &[[250, 10, 20, 255]; 16].concat());
    assert!(latest.paint(
        4,
        4,
        &changed.bgra,
        [Region {
            x: 1,
            y: 1,
            width: 2,
            height: 1
        }]
    ));
    let damage = latest.take().unwrap();
    let second = Frame::new(ViewId(1), 2, latest.clone(), damage);
    // Painted after the frame was taken: in the slot, but not in the frame's damage.
    latest.paint(
        4,
        4,
        &changed.bgra,
        [Region {
            x: 0,
            y: 3,
            width: 1,
            height: 1,
        }],
    );

    let (width, pixels) = drawn_with(&mut renderer, &second, 1.0, 2.0, 6.0);
    assert_eq!(pixel(&pixels, width, 3, 3), [250, 10, 20, 255]);
    assert_eq!(pixel(&pixels, width, 4, 3), [250, 10, 20, 255]);
    assert_eq!(pixel(&pixels, width, 2, 3), colour(0, 1));
    assert_eq!(pixel(&pixels, width, 5, 3), colour(3, 1));
    assert_eq!(
        pixel(&pixels, width, 2, 5),
        colour(0, 3),
        "not in the damage"
    );

    let translucent = framed(&Picture {
        width: 1,
        height: 1,
        bgra: vec![0, 0, 128, 128],
    });
    let (width, pixels) = drawn_by("wgpu", &translucent, 1.0, 2.0, 6.0);
    let [red, green, blue, _] = pixel(&pixels, width, 2, 2);
    // Half of full red, in linear light on an sRGB screen and in the page's colours otherwise.
    let half = if iced::advanced::graphics::color::GAMMA_CORRECTION {
        186..=189
    } else {
        127..=129
    };
    assert!(
        half.contains(&red) && green == 0 && blue == 0,
        "{:?}",
        [red, green, blue]
    );
}

#[test]
fn a_frame_names_its_slot_and_its_damage() {
    let latest = LatestFrame::default();
    latest.put(&premultiplied(2, 3, &[7; 24]));
    let damage = latest.take().unwrap();
    let frame = Frame::new(ViewId(1), 4, latest.clone(), damage.clone());
    assert_eq!(frame.generation(), 4);
    assert_eq!(frame.size(), (2, 3));
    assert_eq!(frame.damage(), &damage);
    assert_eq!(frame.latest(), &latest);
    assert_eq!(frame.pixels(), latest.picture().bgra);
}
