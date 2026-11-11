//! The windowing of the transcript's rows: which rows the Session tab lays out, what it
//! remembers of their heights and what it guesses for rows it has never shown.
//!
//! The transcript is a column of rows that is pinned to its end, so every coordinate here
//! counts from the content's end: `bottom` is how far the viewport's lower edge sits above
//! it, and a row's distance is the sum of the extents of the rows after it. A row is
//! in the window when it intersects the viewport extended by one viewport height on each
//! side (the overscan), so a normal scroll meets rows that are already laid out. The rows in
//! the window are contiguous; the rows before and after them are replaced by two spacers
//! whose heights are the sums of their extents.
//!
//! The window is kept while it serves: it is picked anew only once the viewport, extended by
//! half a viewport on each side, no longer lies inside it, or when the number of rows
//! changed. So a scroll of a few rows leaves the rows laid out as they are. While the user
//! selects text the window's first row stays where it is, since the selection counts the
//! text it spans from there; the window then only grows towards the end.
//!
//! An extent is a row's height including the gap above it. [`Heights`] keeps the extents of
//! rows that were shown, per session, and forgets them all when the width changes because
//! text wraps differently then. [`estimate`] guesses the extent of a row that was never
//! shown. Those guesses are placeholders only: they keep the scrollbar and the spacers in
//! proportion until the row has been measured, and are replaced by the measured extent.

use std::collections::HashMap;
use std::ops::Range;

use iced::widget::scrollable;
use partitur_core::{Entry, SessionId};

use crate::theme::LINE_HEIGHT;

/// The viewport height assumed before the scrollable has reported itself.
const ASSUMED_VIEWPORT: f32 = 1200.0;

/// The estimated extent of a thought, tool run, notice, separator or pending row.
const EVENT_ROW: f32 = 38.0;

/// The estimated extent of a permission request that waits for an answer.
const OPEN_PERMISSION: f32 = 110.0;

/// The estimated extent of a permission request that was answered.
const SETTLED_PERMISSION: f32 = 70.0;

/// The estimated extent of a proposal of children.
const PROPOSAL: f32 = 320.0;

/// The padding and gap of a text row, around its lines.
const TEXT_ROW_CHROME: f32 = 24.0;

/// The average width of one character of body text.
const CHAR_WIDTH: f32 = 7.5;

/// A row of the transcript, stable while the transcript grows.
///
/// The `usize` is the index of the row's first entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// A row of the conversation.
    Conversation(usize),
    /// A row of an orchestrator's timeline.
    Timeline(usize),
    /// The row that shows the agent is about to answer.
    Pending,
}

/// The transcript's scrollable as it last reported itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visible {
    /// How far the viewport's lower edge sits above the content's end.
    pub bottom: f32,
    /// The viewport's height.
    pub height: f32,
    /// The viewport's width.
    pub width: f32,
}

impl Visible {
    /// Reads the report of a transcript scrollable that is anchored to its end, where iced
    /// stores the offset as the distance from the end.
    pub(crate) fn of(viewport: scrollable::Viewport) -> Self {
        let bounds = viewport.bounds();
        Self {
            bottom: viewport.absolute_offset().y,
            height: bounds.height,
            width: bounds.width,
        }
    }
}

/// The measured extents of the rows that were shown, per session.
#[derive(Debug, Default)]
pub(crate) struct Heights {
    width: Option<f32>,
    rows: HashMap<(SessionId, RowKey), f32>,
}

impl Heights {
    /// The measured extent of a row, if it was shown at the current width.
    pub(crate) fn get(&self, session: &SessionId, key: RowKey) -> Option<f32> {
        self.rows.get(&(session.clone(), key)).copied()
    }

    /// Remembers the measured extent of a row.
    pub(crate) fn record(&mut self, session: &SessionId, key: RowKey, extent: f32) {
        self.rows.insert((session.clone(), key), extent);
    }

    /// Notes the width the rows are laid out at; a different width clears every row.
    pub(crate) fn set_width(&mut self, width: f32) {
        if self.width != Some(width) {
            self.rows.clear();
            self.width = Some(width);
        }
    }

    /// Drops everything measured for a session.
    pub(crate) fn forget(&mut self, session: &SessionId) {
        self.rows.retain(|(owner, _), _| owner != session);
    }
}

/// The rows to lay out and the heights of the spacers that stand in for the others.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Window {
    /// The indices of the rows to lay out.
    pub(crate) rows: Range<usize>,
    /// The summed extent of the rows before the window.
    pub(crate) above: f32,
    /// The summed extent of the rows after the window.
    pub(crate) below: f32,
}

/// The window a session's transcript keeps, with what it was picked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Kept {
    /// The session whose rows it holds.
    pub(crate) session: SessionId,
    /// The width the rows were laid out at.
    pub(crate) width: f32,
    /// How many rows the transcript had.
    pub(crate) count: usize,
    /// The window itself.
    pub(crate) window: Window,
}

impl Kept {
    /// Whether it was picked for `count` rows of `session` at `width`.
    pub(crate) fn fits(&self, session: &SessionId, width: f32, count: usize) -> bool {
        &self.session == session && self.width.to_bits() == width.to_bits() && self.count == count
    }
}

/// The viewport to window for, and where its lower edge sits once it is kept inside the
/// content of `total` height.
fn viewport(visible: Option<Visible>, total: f32) -> (Visible, f32) {
    let visible = visible.unwrap_or(Visible {
        bottom: 0.0,
        height: ASSUMED_VIEWPORT,
        width: 0.0,
    });
    let bottom = visible.bottom.min((total - visible.height).max(0.0));
    (visible, bottom)
}

/// Picks the rows that intersect the viewport and one viewport of overscan on each side.
pub(crate) fn window(extents: &[f32], visible: Option<Visible>) -> Window {
    let total: f32 = extents.iter().sum();
    let (visible, bottom) = viewport(visible, total);
    let near = (bottom - visible.height).max(0.0);
    let far = bottom + 2.0 * visible.height;

    let mut distance = 0.0;
    let mut included: Option<(usize, usize)> = None;
    for (index, extent) in extents.iter().enumerate().rev() {
        if distance < far && distance + extent > near {
            included = Some(included.map_or((index, index), |(_, last)| (index, last)));
        }
        distance += extent;
    }

    let Some((first, last)) = included else {
        return spanning(extents, 0..0);
    };
    spanning(extents, first..last + 1)
}

/// Keeps the window of the rows `kept`, picked when the transcript had `count` rows, as long
/// as it serves the viewport `visible`; else picks a new one. While the user `selects`, the
/// window's first row stays and the window only grows towards the end.
pub(crate) fn keep(
    extents: &[f32],
    visible: Option<Visible>,
    kept: Option<(Range<usize>, usize)>,
    selects: bool,
) -> Window {
    let fresh = window(extents, visible);
    let Some((kept, count)) = kept else {
        return fresh;
    };
    let rows = extents.len();
    if selects {
        let start = kept.start.min(rows);
        let end = kept.end.max(fresh.rows.end).clamp(start, rows);
        return spanning(extents, start..end);
    }
    if count == rows && serves(extents, visible, &kept) {
        return spanning(extents, kept);
    }
    fresh
}

/// Whether the rows `kept` cover the viewport `visible` and half a viewport on each side, as
/// far as the content reaches.
fn serves(extents: &[f32], visible: Option<Visible>, kept: &Range<usize>) -> bool {
    let total: f32 = extents.iter().sum();
    let (visible, bottom) = viewport(visible, total);
    let near = (bottom - visible.height / 2.0).max(0.0);
    let far = (bottom + 1.5 * visible.height).min(total);
    let below: f32 = extents.iter().skip(kept.end).sum();
    let shown: f32 = extents.iter().skip(kept.start).take(kept.len()).sum();
    below <= near && below + shown >= far
}

/// The window of the rows `rows`, with the spacers for the rows around them.
fn spanning(extents: &[f32], rows: Range<usize>) -> Window {
    let above = extents.iter().take(rows.start).sum();
    let below = extents.iter().skip(rows.end).sum();
    Window { rows, above, below }
}

/// Guesses the extent of a row that was never shown.
///
/// A run is drawn as one row. It holds several entries only when they are consecutive tool
/// calls, which collapse into one event row, so the first entry stands for the whole run;
/// an empty run is the pending row. `open` tells whether a permission in the row still
/// waits for an answer.
pub(crate) fn estimate(run: &[Entry], open: bool, width: f32) -> f32 {
    run.first()
        .map_or(EVENT_ROW, |entry| entry_estimate(entry, open, width))
}

fn entry_estimate(entry: &Entry, open: bool, width: f32) -> f32 {
    match entry {
        Entry::User(text)
        | Entry::Agent(text)
        | Entry::OrchestratorPrompt(text)
        | Entry::PartiturPrompt(text)
        | Entry::ChildResult { text, .. }
        | Entry::ChildPrompted { prompt: text, .. } => text_estimate(text, width),
        Entry::Permission { .. } if open => OPEN_PERMISSION,
        Entry::Permission { .. } => SETTLED_PERMISSION,
        Entry::Proposal { .. } => PROPOSAL,
        _ => EVENT_ROW,
    }
}

fn text_estimate(text: &str, width: f32) -> f32 {
    // Truncation is intended: whole characters per line, whole lines.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let per_line = ((width / CHAR_WIDTH).floor().max(0.0) as usize).max(1);
    let lines: usize = text
        .lines()
        .map(|line| line.chars().count().div_ceil(per_line).max(1))
        .sum::<usize>()
        .max(1);
    #[expect(clippy::cast_precision_loss)]
    let lines = lines as f32;
    TEXT_ROW_CHROME + LINE_HEIGHT * lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use partitur_core::{ToolCallId, ToolKind, ToolStatus};

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-3, "{actual} != {expected}");
    }

    fn visible(bottom: f32, height: f32) -> Visible {
        Visible {
            bottom,
            height,
            width: 800.0,
        }
    }

    #[test]
    fn without_a_viewport_the_window_covers_the_end() {
        let w = window(&[100.0; 100], None);
        assert_eq!(w.rows, 76..100);
        assert_close(w.above, 7600.0);
        assert_close(w.below, 0.0);
    }

    #[test]
    fn a_viewport_at_the_top_covers_the_start() {
        let w = window(&[100.0; 100], Some(visible(9232.0, 768.0)));
        assert_eq!(w.rows, 0..16);
        assert_close(w.above, 0.0);
        assert_close(w.below, 8400.0);
    }

    #[test]
    fn a_viewport_beyond_the_content_falls_back_to_its_start() {
        let w = window(&[100.0; 5], Some(visible(5000.0, 768.0)));
        assert_eq!(w.rows, 0..5);
    }

    #[test]
    fn rows_and_spacers_add_up_to_the_whole() {
        let extents = [30.0, 500.0, 38.0, 2000.0, 38.0, 90.0];
        let w = window(&extents, Some(visible(400.0, 300.0)));
        let shown: f32 = extents.iter().skip(w.rows.start).take(w.rows.len()).sum();
        assert_close(w.above + shown + w.below, extents.iter().sum::<f32>());
    }

    #[test]
    fn a_row_taller_than_the_viewport_stays_while_any_part_is_visible() {
        let w = window(&[100.0, 5000.0, 100.0], Some(visible(2500.0, 300.0)));
        assert!(w.rows.contains(&1));
    }

    #[test]
    fn an_empty_transcript_has_an_empty_window() {
        let w = window(&[], None);
        assert_eq!(w.rows, 0..0);
        assert_close(w.above, 0.0);
        assert_close(w.below, 0.0);
    }

    #[test]
    fn a_new_width_forgets_every_measured_height() {
        let session = SessionId::new();
        let mut heights = Heights::default();
        heights.set_width(800.0);
        heights.record(&session, RowKey::Pending, 40.0);
        heights.set_width(800.0);
        assert_eq!(
            heights.get(&session, RowKey::Pending).map(f32::to_bits),
            Some(40.0_f32.to_bits())
        );
        heights.set_width(640.0);
        assert_eq!(heights.get(&session, RowKey::Pending), None);
    }

    #[test]
    fn heights_are_kept_per_session() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let key = RowKey::Conversation(3);
        let mut heights = Heights::default();
        heights.record(&a, key, 10.0);
        heights.record(&b, key, 20.0);
        heights.forget(&a);
        assert_eq!(heights.get(&a, key), None);
        assert_eq!(
            heights.get(&b, key).map(f32::to_bits),
            Some(20.0_f32.to_bits())
        );
    }

    /// The rows the window keeps after the viewport moved to `visible` from where it picked
    /// the window of `kept`.
    fn kept_after(kept: &Window, visible: Visible, selects: bool) -> Range<usize> {
        let extents = [100.0; 100];
        keep(
            &extents,
            Some(visible),
            Some((kept.rows.clone(), extents.len())),
            selects,
        )
        .rows
    }

    #[test]
    fn a_scroll_within_half_a_viewport_keeps_the_window() {
        let picked = window(&[100.0; 100], Some(visible(3000.0, 768.0)));
        assert_eq!(
            kept_after(&picked, visible(3300.0, 768.0), false),
            picked.rows
        );
        assert_eq!(
            kept_after(&picked, visible(2700.0, 768.0), false),
            picked.rows
        );
    }

    #[test]
    fn a_scroll_beyond_half_a_viewport_picks_a_new_window() {
        let picked = window(&[100.0; 100], Some(visible(3000.0, 768.0)));
        let moved = visible(3500.0, 768.0);
        assert_eq!(
            kept_after(&picked, moved, false),
            window(&[100.0; 100], Some(moved)).rows
        );
    }

    #[test]
    fn while_the_user_selects_the_first_row_stays_and_the_window_grows() {
        let picked = window(&[100.0; 100], Some(visible(3000.0, 768.0)));
        let up = kept_after(&picked, visible(5000.0, 768.0), true);
        assert_eq!(up.start, picked.rows.start);
        assert_eq!(up.end, picked.rows.end);
        let down = kept_after(&picked, visible(500.0, 768.0), true);
        assert_eq!(down, picked.rows.start..100);
    }

    #[test]
    fn new_rows_pick_a_new_window_unless_the_user_selects() {
        let picked = window(&[100.0; 100], Some(visible(0.0, 768.0)));
        let grown = [100.0; 103];
        let at_end = Some(visible(0.0, 768.0));
        let kept = Some((picked.rows.clone(), 100));
        assert_eq!(
            keep(&grown, at_end, kept.clone(), false),
            window(&grown, at_end)
        );
        let selecting = keep(&grown, at_end, kept, true);
        assert_eq!(selecting.rows, picked.rows.start..103);
        assert_close(
            selecting.above,
            grown.iter().take(picked.rows.start).sum::<f32>(),
        );
        assert_close(selecting.below, 0.0);
    }

    #[test]
    fn without_a_kept_window_a_new_one_is_picked() {
        let extents = [100.0; 100];
        let at = Some(visible(3000.0, 768.0));
        assert_eq!(keep(&extents, at, None, true), window(&extents, at));
    }

    #[test]
    fn a_longer_answer_is_estimated_taller() {
        let short = [Entry::Agent("one line".to_owned())];
        let long = [Entry::Agent(vec!["a line"; 20].join("\n"))];
        assert!(estimate(&long, false, 760.0) > estimate(&short, false, 760.0));
    }

    #[test]
    fn a_run_of_tool_calls_is_estimated_as_one_row() {
        let call = |n: usize| Entry::ToolCall {
            id: ToolCallId::new(format!("t-{n}")),
            title: "run".to_owned(),
            kind: ToolKind::Other,
            status: ToolStatus::Completed,
            content: Vec::new(),
            locations: Vec::new(),
        };
        let run: Vec<Entry> = (0..10).map(call).collect();
        let one = estimate(std::slice::from_ref(&call(0)), false, 760.0);
        assert_close(estimate(&run, false, 760.0), one);
        assert_close(one, EVENT_ROW);
    }
}
