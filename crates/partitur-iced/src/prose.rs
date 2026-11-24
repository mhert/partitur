//! Markdown as the tabs draw prose: like iced draws it, except that its text is selectable
//! and a paragraph's lines are as high as those of all other prose. Where a click on a link
//! goes is up to the place that shows the Markdown: an agent's answer on the Session tab or
//! a document on the Spec tab names its own [`LinkTarget`].

use std::marker::PhantomData;

use iced::widget::{Column, container, markdown, scrollable};
use iced::{Element, Length, Padding, Pixels, Theme};

use crate::app::Message;
use crate::{selectable, theme};

/// The height of a line of prose, in logical pixels.
const LINE: Pixels = Pixels(theme::LINE_HEIGHT);

/// Where a click on a link in Markdown drawn by [`Prose`] goes.
pub(crate) trait LinkTarget: 'static {
    /// The message a click on the link to `uri` sends.
    fn message(uri: markdown::Uri) -> Message;
}

/// Draws Markdown the way iced does, except that its text is selectable and a paragraph's
/// lines are as high as those of all other prose. A click on a link sends the message of
/// `Links`.
pub(crate) struct Prose<Links>(PhantomData<Links>);

impl<Links> Prose<Links> {
    /// The viewer.
    pub(crate) fn new() -> Self {
        Self(PhantomData)
    }
}

impl<'a, Links: LinkTarget> markdown::Viewer<'a, Message> for Prose<Links> {
    fn on_link_click(uri: markdown::Uri) -> Message {
        Links::message(uri)
    }

    fn paragraph(
        &self,
        settings: markdown::Settings,
        text: &markdown::Text,
    ) -> Element<'a, Message> {
        selectable::rich(text.spans(settings.style))
            .size(settings.text_size)
            .line_height(LINE)
            .on_link_click(Self::on_link_click)
            .into()
    }

    fn heading(
        &self,
        settings: markdown::Settings,
        level: &'a markdown::HeadingLevel,
        text: &'a markdown::Text,
        index: usize,
    ) -> Element<'a, Message> {
        let size = match level {
            markdown::HeadingLevel::H1 => settings.h1_size,
            markdown::HeadingLevel::H2 => settings.h2_size,
            markdown::HeadingLevel::H3 => settings.h3_size,
            markdown::HeadingLevel::H4 => settings.h4_size,
            markdown::HeadingLevel::H5 => settings.h5_size,
            markdown::HeadingLevel::H6 => settings.h6_size,
        };
        // Like iced, a heading after other blocks keeps half a line's distance to them.
        let top = if index > 0 {
            settings.text_size.0 / 2.0
        } else {
            0.0
        };
        container(
            selectable::rich(text.spans(settings.style))
                .size(size)
                .on_link_click(Self::on_link_click),
        )
        .padding(Padding::ZERO.top(top))
        .into()
    }

    fn code_block(
        &self,
        settings: markdown::Settings,
        _language: Option<&'a str>,
        _code: &'a str,
        lines: &'a [markdown::Text],
    ) -> Element<'a, Message> {
        let lines = lines.iter().map(|line| {
            selectable::rich(line.spans(settings.style))
                .font(settings.style.code_block_font)
                .size(settings.code_size)
                .on_link_click(Self::on_link_click)
                .into()
        });
        let bar = settings.code_size.0 / 2.0;
        // Like iced, long lines scroll sideways instead of wrapping.
        let lines =
            scrollable(container(Column::with_children(lines)).padding(settings.code_size.0))
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default()
                        .width(bar)
                        .scroller_width(bar),
                ));
        container(lines)
            .width(Length::Fill)
            .padding(settings.code_size.0 / 4.0)
            .class(<Theme as markdown::Catalog>::code_block())
            .into()
    }
}
