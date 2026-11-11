//! A contiguous run of the transcript's rows, stacked from top to bottom without space
//! between them; the gaps belong to the rows.
//!
//! iced hands a column's widget state to its children by position. When the window of rows
//! laid out slides by a few rows and keeps its length, every row would take over the state
//! of the row that stood at its position before: its text would be shaped anew, its sensor
//! would report again, and the scroll offset of a code block or the hover of a button would
//! jump to another row. [`Rows`] knows the index of its first row, so it drops the state of
//! the rows that left at the front, gives the rows that came in at the front fresh state,
//! and only then hands the state out by position. A row that stays keeps its own.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, overlay, renderer};
use iced::{
    Alignment, Element, Event, Length, Padding, Rectangle, Renderer, Size, Theme, Vector, mouse,
};

/// The rows from the one at `first` on, stacked from top to bottom.
pub(super) fn rows<'a, Message: 'a>(
    first: usize,
    rows: impl IntoIterator<Item = Element<'a, Message>>,
) -> Rows<'a, Message> {
    Rows {
        first,
        children: rows.into_iter().collect(),
    }
}

/// A contiguous run of rows that keeps each row's state as the run slides. See the
/// [module](self).
pub(super) struct Rows<'a, Message> {
    /// The index of the first row among all rows.
    first: usize,
    /// The rows, top to bottom.
    children: Vec<Element<'a, Message>>,
}

/// The tree state of [`Rows`]: the index of the first row the children's state belongs to.
struct First(usize);

impl<Message> Widget<Message, Theme, Renderer> for Rows<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<First>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(First(self.first))
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let First(before) = *tree.state.downcast_ref::<First>();
        if self.first > before {
            // The rows before the new first one left.
            let left = (self.first - before).min(tree.children.len());
            drop(tree.children.drain(..left));
        } else if self.first < before {
            // Rows came in before the old first one; they start with fresh state.
            let came = self
                .children
                .iter()
                .take(before - self.first)
                .map(Tree::new);
            drop(tree.children.splice(..0, came));
        }
        *tree.state.downcast_mut::<First>() = First(self.first);
        // The rows that stayed now stand at their positions; the run ends where it ends.
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            Padding::ZERO,
            0.0,
            Alignment::Start,
            &mut self.children,
            &mut tree.children,
        )
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
        for ((child, tree), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            child
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, tree), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Rows<'a, Message>> for Element<'a, Message> {
    fn from(rows: Rows<'a, Message>) -> Self {
        Element::new(rows)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeSet;
    use std::ops::Range;

    use iced::advanced::renderer::Headless;
    use iced::widget::{sensor, text};
    use iced_test::runtime::user_interface::{self, UserInterface};

    use super::*;

    /// The rows of `shown`, each of which reports its index when it is shown first.
    fn view<'a>(shown: Range<usize>) -> Element<'a, usize> {
        let first = shown.start;
        rows(
            first,
            shown.map(|index| {
                sensor(text(index.to_string()))
                    .on_show(move |_| index)
                    .into()
            }),
        )
        .into()
    }

    /// The rows that report themselves in a frame of `shown` drawn with the state of
    /// `cache`, and the cache to draw the next frame with.
    fn frame(
        shown: Range<usize>,
        renderer: &mut Renderer,
        cache: user_interface::Cache,
    ) -> (BTreeSet<usize>, user_interface::Cache) {
        let mut ui = UserInterface::build(view(shown), Size::new(400.0, 4000.0), cache, renderer);
        let mut reported = Vec::new();
        let _ = ui.update(
            &[Event::Window(iced::window::Event::RedrawRequested(
                iced::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut reported,
        );
        (reported.into_iter().collect(), ui.into_cache())
    }

    #[test]
    fn only_the_rows_that_come_in_get_fresh_state() {
        let mut renderer = futures::executor::block_on(<Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            None,
        ))
        .unwrap();
        let (shown, cache) = frame(10..20, &mut renderer, user_interface::Cache::default());
        assert_eq!(shown, (10..20).collect());

        // Down by three: three rows come in at the end, as many leave at the front.
        let (shown, cache) = frame(13..23, &mut renderer, cache);
        assert_eq!(shown, (20..23).collect());

        // Up by five, one row shorter: five come in at the front.
        let (shown, cache) = frame(8..17, &mut renderer, cache);
        assert_eq!(shown, (8..13).collect());

        // Far away: every row is new.
        let (shown, _) = frame(40..45, &mut renderer, cache);
        assert_eq!(shown, (40..45).collect());
    }
}
