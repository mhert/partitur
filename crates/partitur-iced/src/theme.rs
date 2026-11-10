//! The look every view shares: the colour tokens in a dark and a light set, the two iced
//! themes built from them, the bundled fonts and the text sizes.
//!
//! A view never names a colour itself. Its style closures ask [`tokens`] for the set of the
//! active theme, so the dark and the light window differ only by the values in [`DARK`] and
//! [`LIGHT`].

use iced::font::Weight;
use iced::theme::{Mode, Palette};
use iced::{Color, Font, Theme, color};

/// The size of interface text and prose, in logical pixels.
pub const TEXT: f32 = 13.5;

/// The size of card headings and the wordmark.
pub const HEADING: f32 = 13.0;

/// The size of secondary text: paths, hints, breadcrumbs, the options row.
pub const SMALL: f32 = 12.0;

/// The size of text in pills and key caps, and of times.
pub const TINY: f32 = 11.0;

/// The height of one line of prose at [`TEXT`] size, in logical pixels.
pub const LINE_HEIGHT: f32 = 20.0;

/// Inter, the interface font, at regular weight.
pub const UI: Font = Font::with_name("Inter");

/// Inter at medium weight: selected titles and repository names.
pub const UI_MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..UI
};

/// Inter at semibold weight: project names, headings, the active tab.
pub const UI_SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..UI
};

/// The monospaced font, for paths, identifiers, routes, counts and key caps.
pub const MONO: Font = Font::with_name("JetBrains Mono");

/// The bundled font files, each one weight of [`UI`] (Inter) or [`MONO`] (`JetBrains` Mono); both families are under
/// the SIL Open Font License, whose texts sit next to the files.
pub const FONTS: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
];

/// A pill's colours: the text and the background it reads on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tint {
    /// The text, also used alone for dots and coloured words.
    pub fg: Color,
    /// The pill's background.
    pub bg: Color,
}

/// Every colour the interface uses, by what it is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    /// The window background.
    pub bg: Color,
    /// The toolbar, the status line, cards, event rows and the composer.
    pub panel: Color,
    /// The footer of the split table, a step deeper than a card.
    pub panel_deep: Color,
    /// One-pixel borders.
    pub border: Color,
    /// Hairlines inside cards, and the chip behind inline code.
    pub border_soft: Color,
    /// The border of the composer and of outlined buttons.
    pub border_input: Color,
    /// The border of the proposed split's card.
    pub border_split: Color,
    /// The background of the selected tree row.
    pub selection: Color,
    /// The background of text selected with the mouse.
    pub text_selection: Color,
    /// The background of a prompt bubble.
    pub prompt_bubble: Color,
    /// The text in a prompt bubble.
    pub prompt_text: Color,
    /// Primary text.
    pub text: Color,
    /// The title of the selected tree row.
    pub text_strong: Color,
    /// Secondary values.
    pub text_soft: Color,
    /// Labels and hints on a tinted surface such as the selected row.
    pub muted_strong: Color,
    /// Labels and hints.
    pub muted: Color,
    /// Separators and other marks that carry no information; too faint for text.
    pub dim: Color,
    /// The surface of the archive button and of an archived session's banner.
    pub archive: Color,
    /// The outline of the key cap on the archive button.
    pub archive_edge: Color,
    /// The surface of the delete button while hovered, and the outline of its key cap; the
    /// button itself sits on the failed tint.
    pub danger_edge: Color,
    /// A running session.
    pub running: Tint,
    /// A session that waits for the user.
    pub asks_you: Tint,
    /// A finished session.
    pub done: Tint,
    /// A failed session.
    pub failed: Tint,
    /// An orchestrator, and what it did by itself.
    pub orchestrator: Tint,
    /// The tools segment of the context bar, and links in prose.
    pub tools_mcp: Color,
    /// The unused rest of a bar.
    pub track: Color,
    /// Added lines.
    pub diff_add: Color,
    /// Removed lines, and controls that stop work.
    pub diff_del: Color,
}

/// The tokens of the dark theme.
pub const DARK: Tokens = Tokens {
    bg: color!(0x14_15_18),
    panel: color!(0x17_18_1c),
    panel_deep: color!(0x15_16_1a),
    border: color!(0x2a_2d_33),
    border_soft: color!(0x1f_21_26),
    border_input: color!(0x3a_3f_49),
    border_split: color!(0x3a_34_50),
    selection: color!(0x20_27_38),
    text_selection: color!(0x2c_44_70),
    prompt_bubble: color!(0x24_31_4a),
    prompt_text: color!(0xe8_ec_f4),
    text: color!(0xe2_e4_e8),
    text_strong: color!(0xff_ff_ff),
    text_soft: color!(0xc4_c8_d0),
    muted_strong: color!(0x9a_a0_ab),
    muted: color!(0x7c_83_90),
    dim: color!(0x51_58_66),
    archive: color!(0x2a_35_50),
    archive_edge: color!(0x3a_46_61),
    danger_edge: color!(0x63_2c_22),
    running: Tint {
        fg: color!(0x7f_e3_b1),
        bg: color!(0x16_41_2e),
    },
    asks_you: Tint {
        fg: color!(0xf3_c9_69),
        bg: color!(0x4a_3a_10),
    },
    done: Tint {
        fg: color!(0xa3_a9_b4),
        bg: color!(0x2a_2d_33),
    },
    failed: Tint {
        fg: color!(0xf2_9a_8a),
        bg: color!(0x4a_1f_18),
    },
    orchestrator: Tint {
        fg: color!(0xc9_bd_ff),
        bg: color!(0x2c_25_50),
    },
    tools_mcp: color!(0x8f_c1_ff),
    track: color!(0x24_26_2c),
    diff_add: color!(0x7f_e3_b1),
    diff_del: color!(0xf2_9a_8a),
};

/// The tokens of the light theme. Every text colour keeps a contrast of at least 4.5:1 on
/// the surface it is used on.
pub const LIGHT: Tokens = Tokens {
    bg: color!(0xf6_f7_f8),
    panel: color!(0xff_ff_ff),
    panel_deep: color!(0xf0_f1_f3),
    border: color!(0xd9_dc_e1),
    border_soft: color!(0xe8_ea_ed),
    border_input: color!(0xb9_be_c7),
    border_split: color!(0xcf_c6_f5),
    selection: color!(0xdd_e5_f6),
    text_selection: color!(0xb6_cc_f5),
    prompt_bubble: color!(0xd9_e4_f8),
    prompt_text: color!(0x16_20_3a),
    text: color!(0x1b_1d_22),
    text_strong: color!(0x0c_0d_10),
    text_soft: color!(0x3a_3f_49),
    muted_strong: color!(0x5b_61_6c),
    muted: color!(0x68_6e_7a),
    dim: color!(0xa0_a6_b1),
    archive: color!(0xd3_dc_ef),
    archive_edge: color!(0xa9_b6_d3),
    danger_edge: color!(0xf7_cb_c1),
    running: Tint {
        fg: color!(0x0e_6a_42),
        bg: color!(0xd2_f1_e1),
    },
    asks_you: Tint {
        fg: color!(0x73_51_00),
        bg: color!(0xfa_e8_b4),
    },
    done: Tint {
        fg: color!(0x55_5b_66),
        bg: color!(0xe3_e5_e9),
    },
    failed: Tint {
        fg: color!(0xa8_30_1a),
        bg: color!(0xfb_dc_d5),
    },
    orchestrator: Tint {
        fg: color!(0x4a_39_a6),
        bg: color!(0xe5_df_ff),
    },
    tools_mcp: color!(0x23_69_b8),
    track: color!(0xe3_e5_e9),
    diff_add: color!(0x0e_74_46),
    diff_del: color!(0xb5_37_1f),
};

/// The tokens for the desktop's preference `mode`: the light set when the desktop asks for
/// light, the dark one when it asks for dark or states no preference.
pub fn tokens_for(mode: Mode) -> &'static Tokens {
    match mode {
        Mode::Light => &LIGHT,
        Mode::Dark | Mode::None => &DARK,
    }
}

/// The iced theme for the desktop's preference `mode`, built from the tokens
/// [`tokens_for`] picks.
pub fn theme(mode: Mode) -> Theme {
    let tokens = tokens_for(mode);
    let name = if mode == Mode::Light {
        "Partitur Light"
    } else {
        "Partitur Dark"
    };
    // iced derives the colours of widgets no view styles itself from this palette. The
    // design has no accent colour, so the primary role takes the soft text colour.
    Theme::custom(
        name,
        Palette {
            background: tokens.bg,
            text: tokens.text,
            primary: tokens.text_soft,
            success: tokens.running.fg,
            warning: tokens.asks_you.fg,
            danger: tokens.diff_del,
        },
    )
}

/// The tokens that belong to `theme`: [`DARK`] for a theme with a dark background,
/// [`LIGHT`] otherwise.
pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
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

    /// The relative luminance of `color`, as WCAG 2 defines it.
    fn luminance(color: Color) -> f32 {
        let linear = |channel: f32| {
            if channel <= 0.039_28 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    /// The WCAG 2 contrast ratio between `text` and `surface`.
    fn contrast(text: Color, surface: Color) -> f32 {
        let (text, surface) = (luminance(text), luminance(surface));
        (text.max(surface) + 0.05) / (text.min(surface) + 0.05)
    }

    /// Every pair of a text colour and a surface it is used on.
    fn text_pairs(tokens: &Tokens) -> Vec<(&'static str, Color, Color)> {
        vec![
            ("text on bg", tokens.text, tokens.bg),
            ("text on panel", tokens.text, tokens.panel),
            ("strong on selection", tokens.text_strong, tokens.selection),
            ("soft on bg", tokens.text_soft, tokens.bg),
            ("soft on panel deep", tokens.text_soft, tokens.panel_deep),
            ("muted on bg", tokens.muted, tokens.bg),
            ("muted on panel", tokens.muted, tokens.panel),
            ("muted on panel deep", tokens.muted, tokens.panel_deep),
            ("strong muted on panel", tokens.muted_strong, tokens.panel),
            ("inline code", tokens.text, tokens.border_soft),
            ("link on bg", tokens.tools_mcp, tokens.bg),
            ("failed on bg", tokens.failed.fg, tokens.bg),
            ("failed on panel", tokens.failed.fg, tokens.panel),
            (
                "strong muted on selection",
                tokens.muted_strong,
                tokens.selection,
            ),
            ("prompt", tokens.prompt_text, tokens.prompt_bubble),
            ("running", tokens.running.fg, tokens.running.bg),
            ("asks you", tokens.asks_you.fg, tokens.asks_you.bg),
            ("done", tokens.done.fg, tokens.done.bg),
            ("failed", tokens.failed.fg, tokens.failed.bg),
            (
                "orchestrator",
                tokens.orchestrator.fg,
                tokens.orchestrator.bg,
            ),
            ("waits on panel", tokens.asks_you.fg, tokens.panel),
            ("added on panel", tokens.diff_add, tokens.panel),
            ("removed on panel", tokens.diff_del, tokens.panel),
            ("removed on bg", tokens.diff_del, tokens.bg),
            ("primary button", tokens.bg, tokens.text),
            ("text on archive", tokens.text, tokens.archive),
            ("soft on archive", tokens.text_soft, tokens.archive),
            ("danger button", tokens.bg, tokens.failed.fg),
            ("danger button hovered", tokens.bg, tokens.diff_del),
            (
                "failed on danger edge",
                tokens.failed.fg,
                tokens.danger_edge,
            ),
        ]
    }

    #[test]
    fn every_text_colour_reads_on_its_surface_in_both_themes() {
        for (theme, tokens) in [("dark", &DARK), ("light", &LIGHT)] {
            for (pair, text, surface) in text_pairs(tokens) {
                let ratio = contrast(text, surface);
                assert!(ratio >= 4.5, "{theme}: {pair} has a contrast of {ratio}");
            }
        }
    }

    #[test]
    fn the_theme_follows_the_desktop_and_is_dark_without_a_preference() {
        assert_eq!(tokens(&theme(Mode::Light)), &LIGHT);
        assert_eq!(tokens(&theme(Mode::Dark)), &DARK);
        assert_eq!(tokens(&theme(Mode::None)), &DARK);
    }

    #[test]
    fn the_tokens_for_a_preference_are_those_of_its_theme() {
        for mode in [Mode::Light, Mode::Dark, Mode::None] {
            assert_eq!(tokens_for(mode), tokens(&theme(mode)));
        }
        assert_eq!(tokens_for(Mode::Light), &LIGHT);
    }

    #[test]
    fn the_theme_takes_its_base_colours_from_the_tokens() {
        for (mode, tokens) in [(Mode::Dark, &DARK), (Mode::Light, &LIGHT)] {
            let palette = theme(mode).palette();

            assert_eq!(palette.background, tokens.bg);
            assert_eq!(palette.text, tokens.text);
        }
    }
}
