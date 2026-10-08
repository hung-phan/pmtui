//! Scrolling a pane with the MOUSE WHEEL — the one place that knows a wheel notch means "move the
//! selection" over the list and "scroll a few lines" over the two text panes (the detail transcript
//! and the status log).
//!
//! There is no keyboard focus anymore: `j`/`k`/`PgUp`/`PgDn`/`Home`/`End` move the SESSION SELECTION
//! (see `keys.rs`), and the panes scroll only when the wheel is over them — the pane under the
//! pointer is resolved by [`PaneRects::hit`] and handed to [`App::wheel_pane`] here.

use crate::*;

/// Lines a page-worth of selection movement covers. Ten, matching the wake view and the help
/// overlay, so "a page" means the same thing everywhere. `keys.rs` feeds this to `move_sel` for
/// `PgUp`/`PgDn`.
pub(crate) const PAGE_LINES: usize = 10;

/// Lines ONE mouse-wheel notch moves a text pane.
///
/// Three, the convention every terminal and editor uses, because one was measurably wrong: user, on
/// the first cut, *"The mouse scroll on the main panel is laggy"*. A notch that moves a single line
/// makes a long transcript feel stuck — the human spins the wheel and the text barely budges, which
/// reads as the app failing to keep up rather than as a small step.
pub(crate) const WHEEL_LINES: usize = 3;

impl App {
    /// One WHEEL NOTCH in the pane under the pointer.
    ///
    /// A notch is [`WHEEL_LINES`] in the text panes and ONE ROW in the list,
    /// which is not an inconsistency: scrolling text past the window is cheap and reversible, while
    /// the selection drives which session the right pane reads and which row every key acts on.
    /// Three rows per notch there would fling the human past the row they were aiming at.
    pub(crate) fn wheel_pane(&mut self, pane: Pane, down: bool) {
        match pane {
            Pane::Sessions => self.move_sel(if down { 1 } else { -1 }),
            Pane::Detail => {
                self.detail_scroll =
                    scroll_by(self.detail_scroll, down, WHEEL_LINES, self.detail_max.get());
            }
        }
    }
}

/// Both text scrolls count BACKWARD from the tail (0 = newest), so moving `down` shrinks the
/// offset. Clamped against the max the last render measured, so a scroll can never run off content
/// that is not there.
fn scroll_by(cur: usize, down: bool, by: usize, max: usize) -> usize {
    if down {
        cur.saturating_sub(by)
    } else {
        cur.saturating_add(by).min(max)
    }
}
