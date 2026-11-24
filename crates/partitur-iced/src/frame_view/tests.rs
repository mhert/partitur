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
    Frame::new(
        ViewId(1),
        1,
        Picture {
            width: side,
            height: side,
            rgba,
        },
    )
    .unwrap()
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
        &mut renderer,
    );
    ui.draw(
        &mut renderer,
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

#[test]
fn a_picture_whose_pixels_do_not_fill_its_size_makes_no_frame() {
    let made = |width, height, bytes| {
        Frame::new(
            ViewId(1),
            1,
            Picture {
                width,
                height,
                rgba: vec![0; bytes],
            },
        )
    };
    assert!(made(2, 2, 15).is_none());
    assert!(made(2, 2, 17).is_none());
    assert!(made(0, 2, 0).is_none());
    assert!(made(2, 0, 0).is_none());

    let frame = made(2, 3, 24).unwrap();
    assert_eq!(frame.size(), (2, 3));
    assert_eq!(frame.generation(), 1);
    assert_eq!(frame.pixels(), [0; 24]);
}
