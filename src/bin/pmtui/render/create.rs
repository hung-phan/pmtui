//! The `n` create-a-session overlay: a SINGLE-COLUMN card sized like every other modal — the shared
//! overlay width policy (`s` send / `g` edit): width 72, growing toward ~45% of the screen, capped
//! at [`OVERLAY_MAX_W`]. (It used to bypass that cap at ~80%; a compact card reads better.) Each field is one
//! labelled row (three advanced rows the Autonomy dial hides); the two MODEL rows (Worker Model, Decider
//! Model) are `\u{2190}\u{2192}` stepper rows whose summary is `< label >` of the stored value.
//! When a model row is FOCUSED it expands INLINE below its summary — indented to [`VALUE_COL`] so it
//! sits under the value it opens — into that field's whole catalog (`(default)` then the engine's
//! models) with the current value (the one `\u{2190}\u{2192}` steps) marked `\u{25cf}` (bold) and
//! windowed to [`MODEL_LIST_VIEWPORT`] (4 rows) with a real scrollbar down the list's right edge on
//! overflow, its thumb tracking the current `\u{25cf}` over the whole catalog. A dim full-width rule
//! splits "the session" (Engine/Model/Directory) from "how it's driven" (Autonomy + the autopilot
//! rows). The height is FIXED, independent of which field is focused: one row of top air, the shown
//! fields (each ONE row), the group divider, a reserved [`MODEL_LIST_VIEWPORT`] the focused model
//! field's inline list FILLS (scrolling within it) rather than growing the card, and the two borders
//! — clamped into the terminal. Focusing a model field never resizes the window. Every row is
//! TRUNCATED into its width budget (no `.wrap()`, like the `s` send overlay), so a long value can
//! never wrap and shove the fixed-height card's bottom rows off screen.

use crate::*;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
// The Directory ghost is split at a CLUSTER boundary, the same way `truncate` measures it, so the
// caret cell can never land inside a multi-codepoint glyph.
use unicode_segmentation::UnicodeSegmentation;

/// Rows the focused model field's inline list is windowed to — a FIXED, compact 4-row reserve the
/// popup always leaves room for, so focusing a model field FILLS this area (scrolling within it)
/// instead of growing the card. Kept small so the popup stays well within the terminal (a taller
/// reserve pushed the card's bottom off shorter screens, clipping it with no way to scroll). Any
/// catalog longer than 4 shows a scrollbar down the list's right edge (its thumb tracking the
/// current `\u{25cf}`) rather than a `… N more` line.
const MODEL_LIST_VIEWPORT: usize = 4;
/// Width of the label gutter each field row left-pads its label to.
const LABEL_W: usize = 14;
/// Column where a field's `< value >` begins: the 2-col focus marker + the [`LABEL_W`] label gutter
/// the `field_row`/`caret_row` closures use. The focused model field's inline list indents to here
/// so it sits under the value it expands.
pub(crate) const VALUE_COL: usize = 2 + LABEL_W;

/// Create-project overlay (a single-screen intake interview).
#[cfg(test)]
pub(crate) fn render_create(f: &mut Frame, area: Rect, form: &CreateForm) {
    render_create_inner(f, area, form, None);
}

pub(crate) fn render_create_for_app(f: &mut Frame, area: Rect, app: &App, form: &CreateForm) {
    render_create_inner(f, area, form, Some(&app.create_hits));
}

fn render_create_inner(
    f: &mut Frame,
    area: Rect,
    form: &CreateForm,
    hit_sink: Option<&std::cell::RefCell<Vec<(Rect, usize)>>>,
) {
    // FIXED height, independent of which field is focused: the shown fields + the group divider +
    // a reserved MODEL_LIST_VIEWPORT for the focused model field's inline list. Focusing a model
    // field FILLS the reserve (windowed/scrolling) instead of growing the popup, so the window
    // never resizes as you navigate. Each field counts as ONE row here (the list lives in the
    // reserve, not added on top).
    let base_fields = 4 // Engine, Model, Directory, Autonomy (always shown)
        + 1 // the shared Message/Goal intent row (always shown; its label follows the tier)
        + usize::from(form.shows_cadence())
        + if form.shows_decider() { 2 } else { 0 }; // Decider engine + Decider Model
    let rows_shown = base_fields
        + 1 /* group divider */
        + usize::from(form.task_mode) /* intent/runtime divider */
        + MODEL_LIST_VIEWPORT;
    // Sized like every other modal (`s` send, `g`/`i` edits) — the SHARED width policy via
    // `draw_overlay_frame`: width 72, growing toward ~45% of the screen, capped at OVERLAY_MAX_W —
    // NOT the old ~80% bypass. Body rows = 1 top-pad + the shown rows (each truncated to ONE line —
    // no wrap, so this is exact); `overlay_h` adds the border chrome and clamps the frame into the
    // terminal.
    let inner = draw_overlay_frame(
        f,
        area,
        72,
        overlay_h(rows_shown + 1),
        if form.task_mode {
            "New task"
        } else {
            "New session"
        },
        agent_manager::theme::accent(),
        // The keybar advertises the FOCUSED row's own affordances, because the two it would
        // otherwise have to carry together do not fit: the hint is dropped whole once it needs more
        // than the card's width (`draw_overlay_frame_in`), so a sixth chip would silently erase the
        // other five. `^E $EDITOR` only ever applied to the intent row, and on the Directory row
        // `\u{2190}\u{2192}` move the caret rather than step a value — so each row says what is
        // true of it.
        if form.dir_pick().is_some() {
            // SELECT MODE says so: while a candidate is picked, `enter` takes it instead of
            // creating the session, so a keybar still offering `enter create` would be a lie. `tab`
            // is listed because the way out is the thing a human needs most to be told.
            "\u{2191}\u{2193} pick \u{b7} enter take \u{b7} esc back \u{b7} tab field"
        } else if form.field == CreateForm::DIRECTORY {
            "\u{2191}\u{2193} pick \u{b7} \u{2192} complete \u{b7} tab field \u{b7} enter create \u{b7} esc cancel"
        } else {
            "\u{2191}\u{2193} field \u{b7} \u{2190}\u{2192} value \u{b7} ^E $EDITOR \u{b7} enter create \u{b7} esc cancel"
        },
    );

    // Columns the value gets after the [`VALUE_COL`] gutter (the two-column marker + the LABEL_W
    // label) — so a long Directory path scrolls under the caret instead of wrapping the row. The
    // single column now spans the whole inner width.
    let value_cols = usize::from(inner.width).saturating_sub(VALUE_COL + 1);
    let val_style = |focused: bool| {
        if focused {
            Style::default()
                .fg(agent_manager::theme::soft())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    let field_row = |idx: usize, label: &str, value: String| -> Line {
        let focused = form.field == idx;
        let marker = if focused { "> " } else { "  " };
        Line::from(vec![
            Span::raw(marker),
            Span::styled(
                format!("{label:<width$}", width = LABEL_W),
                Style::default().add_modifier(Modifier::DIM),
            ),
            // Truncate into the value budget so the row is always ONE line (no wrap).
            Span::styled(truncate(&value, value_cols), val_style(focused)),
        ])
    };
    // A single-line TEXT field row (Directory, Goal while it is one line): its value carries a
    // MOVABLE caret when focused — a reversed cell over a character, or a trailing `_` at the end —
    // windowed by `Field::caret_view` so the caret stays visible however long the text runs.
    // Unfocused, it is plain text.
    let caret_row = |idx: usize, label: &str, field: &Field| -> Line {
        let focused = form.field == idx;
        let style = val_style(focused);
        let mut spans = vec![
            Span::raw(if focused { "> " } else { "  " }),
            Span::styled(
                format!("{label:<width$}", width = LABEL_W),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ];
        if focused {
            let v = field.caret_view(value_cols);
            // The GHOST completion — only the Directory row has one, so the row kind asks for it
            // rather than every caller passing it, and it can never be drawn against a field with
            // no completion. `dir_ghost` already requires the caret at the end of the line, the
            // only place a suggestion can honestly be drawn.
            let ghost = if idx == CreateForm::DIRECTORY {
                form.dir_ghost()
            } else {
                None
            };
            spans.push(Span::styled(v.left.clone(), style));
            match ghost {
                // The suggestion's FIRST cluster becomes the caret cell, so the caret sits ON the
                // completion the way a shell's block cursor sits on its autosuggestion, and the
                // rest is dim — visibly not typed yet. `v.right` is empty here by construction.
                Some(g) => {
                    let g = truncate(g, value_cols.saturating_sub(text_cols(&v.left)));
                    let mut clusters = g.graphemes(true);
                    spans.push(Span::styled(
                        clusters.next().unwrap_or("_").to_string(),
                        style.add_modifier(Modifier::REVERSED),
                    ));
                    let rest: String = clusters.collect();
                    if !rest.is_empty() {
                        spans.push(Span::styled(
                            rest,
                            Style::default().add_modifier(Modifier::DIM),
                        ));
                    }
                }
                None if v.at.is_empty() => spans.push(Span::styled("_", style)),
                None => {
                    spans.push(Span::styled(v.at, style.add_modifier(Modifier::REVERSED)));
                    spans.push(Span::styled(v.right, style));
                }
            }
        } else {
            // Truncate into the value budget so a long path is one line (no wrap).
            spans.push(Span::styled(truncate(field.as_str(), value_cols), style));
        }
        Line::from(spans)
    };
    // A model field (Worker Model / Decider Model): a ←→ stepper row that, when FOCUSED, expands into
    // an inline list of ALL its selections below the summary. The summary is `< (default) >` or
    // `< label >` (stored value looked up in its catalog, raw-value fallback). Each list row is
    // `● `(the stored value's row — the `(default)` row when None) / `○ ` then the label, the `●` row
    // drawn bold; windowed to MODEL_LIST_VIEWPORT keeping the `●` visible. When focused it returns
    // `Some((win, current, list_len))` so the caller can draw a scrollbar over the list on overflow
    // (the overflow cue — there is no `… N more` line). There is no separate cursor — ←→ move the
    // stored value, so `●` IS the current.
    let model_rows = |idx: usize,
                      label: &str,
                      choices: &[ModelInfo],
                      stored: &Option<String>|
     -> (Vec<Line>, Option<(usize, usize, usize)>) {
        let summary = match stored {
            None => "< (default) >".to_string(),
            Some(v) => {
                let lbl = choices
                    .iter()
                    .find(|m| &m.value == v)
                    .map(|m| m.label.as_str())
                    .unwrap_or(v.as_str());
                format!("< {lbl} >")
            }
        };
        let summary_line = field_row(idx, label, summary);
        if form.field != idx {
            return (vec![summary_line], None);
        }
        let mut out = vec![summary_line];
        let current = match stored {
            None => 0,
            Some(v) => choices
                .iter()
                .position(|m| &m.value == v)
                .map(|i| i + 1)
                .unwrap_or(0),
        };
        let list_len = 1 + choices.len();
        let win = list_len.min(MODEL_LIST_VIEWPORT);
        let max_start = list_len.saturating_sub(win);
        let start = current.saturating_sub(win.saturating_sub(1)).min(max_start);
        for disp in start..start + win {
            let is_cur = disp == current;
            let sel = if is_cur { "\u{25cf} " } else { "\u{25cb} " };
            let lbl = if disp == 0 {
                "(default)".to_string()
            } else {
                choices[disp - 1].label.clone()
            };
            let style = if is_cur {
                Style::default()
                    .fg(agent_manager::theme::soft())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            out.push(Line::from(vec![
                Span::raw(" ".repeat(VALUE_COL)),
                // Truncate to the value budget so the row is one line AND its right column stays
                // clear for the scrollbar gutter.
                Span::styled(truncate(&format!("{sel}{lbl}"), value_cols), style),
            ]));
        }
        (out, Some((win, current, list_len)))
    };
    // Every session seeds a Mode::AgentLoop session; the Autonomy row is the one dial
    // (adjustable), shown with the current level's plain-language descriptor.
    let autonomy_val = format!(
        "< {} > \u{2014} {}",
        tier_name(form.tier),
        autonomy_descriptor(form.tier)
    );
    let cadence_val = format!("< {}s ({} min) >", form.cadence_s, form.cadence_s / 60);
    // The focused model field's inline list span, captured at build time so the scrollbar can be
    // placed over exactly those rows after the Paragraph draws. Only ONE model field is ever
    // focused, so at most one of the two call sites below records a span.
    struct ListSpan {
        row_off: usize, // inner-relative Y of the first list row (rows before it)
        win: usize,     // visible list rows
        current: usize, // 0-based index of the ● selection in the full catalog
        list_len: usize,
    }
    let mut list_span: Option<ListSpan> = None;
    let mut field_rows = Vec::new();
    let mut rows = Vec::new();
    let intent_row = || {
        let label = if form.tier == Tier::Standard {
            "Message"
        } else {
            "Goal"
        };
        if form.goal.as_str().is_empty() || form.goal.contains('\n') {
            field_row(
                CreateForm::GOAL,
                label,
                goal_field_display(form.goal.as_str()),
            )
        } else {
            caret_row(CreateForm::GOAL, label, &form.goal)
        }
    };
    if form.task_mode {
        field_rows.push((CreateForm::GOAL, rows.len()));
        rows.push(intent_row());
        rows.push(Line::from(Span::styled(
            "─".repeat(usize::from(inner.width)),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    field_rows.push((CreateForm::ENGINE, rows.len()));
    rows.push(field_row(
        CreateForm::ENGINE,
        "Engine",
        format!("< {} >", form.engine.label()),
    ));
    // The Worker Model row sits between Engine and Directory; a worker always launches, so it
    // shows at every tier. Focused, it expands its catalog inline below.
    let (lines, meta) = model_rows(
        CreateForm::WORKER_MODEL,
        "Model",
        &form.model_choices,
        &form.worker_model,
    );
    field_rows.push((CreateForm::WORKER_MODEL, rows.len()));
    if let Some((win, current, list_len)) = meta {
        // summary sits at rows.len(); the list's first row is the next line.
        list_span = Some(ListSpan {
            row_off: rows.len() + 1,
            win,
            current,
            list_len,
        });
    }
    rows.extend(lines);
    // The Directory row expands the same way a focused Model row does, into the SAME reserve — only
    // one field is ever focused, so the two can never both want it, and the card's height does not
    // move. The list has no cursor on purpose: it answers "what is there", and `Tab` acts on the
    // ghost in the row above, so there is nothing to select and no key to spend selecting it.
    field_rows.push((CreateForm::DIRECTORY, rows.len()));
    rows.push(caret_row(CreateForm::DIRECTORY, "Directory", &form.dir));
    let dir_paths = form.dir_option_paths();
    if !dir_paths.is_empty() {
        // Windowed exactly like a focused model row's catalog, and for the same reason: the card's
        // height is FIXED, so the list scrolls inside the reserve instead of growing it. With no
        // pick yet the window sits at the top — there is no cursor to follow.
        let pick = form.dir_pick();
        let win = dir_paths.len().min(MODEL_LIST_VIEWPORT);
        let max_start = dir_paths.len().saturating_sub(win);
        let start = pick
            .unwrap_or(0)
            .saturating_sub(win.saturating_sub(1))
            .min(max_start);
        list_span = Some(ListSpan {
            row_off: rows.len(),
            win,
            current: pick.unwrap_or(0),
            list_len: dir_paths.len(),
        });
        for (i, path) in dir_paths.iter().enumerate().skip(start).take(win) {
            let picked = pick == Some(i);
            // The WHOLE path, not the bare name: a candidate's value is what the field would
            // become, and that is what a human compares against the path they are editing. Cut on
            // the LEFT, because the part that distinguishes two paths is their tail.
            let text = truncate_left(path, value_cols);
            rows.push(Line::from(vec![
                Span::raw(" ".repeat(VALUE_COL)),
                Span::styled(
                    text,
                    if picked {
                        // Colored BOLD marks the pick — the dashboard's emphasis everywhere.
                        Style::default()
                            .fg(agent_manager::theme::accent())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().add_modifier(Modifier::DIM)
                    },
                ),
            ]));
        }
    }
    field_rows.push((CreateForm::NAME, rows.len()));
    rows.push(caret_row(CreateForm::NAME, "Name", &form.name));
    // A dim, unlabeled rule splits "the session" (Engine/Model/Directory) from "how it's driven"
    // (Autonomy + on autopilot Cadence/Decider/Decider Model). Structure, so dim — never bold/colored.
    rows.push(Line::from(Span::styled(
        "\u{2500}".repeat(usize::from(inner.width)),
        Style::default().add_modifier(Modifier::DIM),
    )));
    field_rows.push((CreateForm::AUTONOMY, rows.len()));
    rows.push(field_row(CreateForm::AUTONOMY, "Autonomy", autonomy_val));
    // One intent row, named Message on Standard and Goal on Autopilot.
    if !form.task_mode {
        field_rows.push((CreateForm::GOAL, rows.len()));
        rows.push(intent_row());
    }
    // AUTOPILOT-ONLY as well: the cadence is the heartbeat pmd nudges on, so on Standard it
    // configures something that never happens.
    if form.shows_cadence() {
        field_rows.push((CreateForm::CADENCE, rows.len()));
        rows.push(field_row(CreateForm::CADENCE, "Cadence", cadence_val));
    }
    // AUTOPILOT-ONLY (see `CreateForm::shows_decider`): the decider only runs on a row pmd drives,
    // so on Standard this dial configures a consult that never happens.
    if form.shows_decider() {
        field_rows.push((CreateForm::DECIDER, rows.len()));
        rows.push(field_row(
            CreateForm::DECIDER,
            "Decider",
            format!("< {} >", form.decider_engine.label()),
        ));
        // The Decider Model row rides right below the Decider engine — the same ←→ stepper (and the
        // same inline catalog when focused) as the Worker Model row, over the decider's own
        // per-engine catalog. The full name (not an abbreviation) distinguishes it from the worker's
        // Model row.
        let (lines, meta) = model_rows(
            CreateForm::DECIDER_MODEL,
            "Decider Model",
            &form.decider_model_choices,
            &form.decider_model,
        );
        field_rows.push((CreateForm::DECIDER_MODEL, rows.len()));
        if let Some((win, current, list_len)) = meta {
            // summary sits at rows.len(); the list's first row is the next line.
            list_span = Some(ListSpan {
                row_off: rows.len() + 1,
                win,
                current,
                list_len,
            });
        }
        rows.extend(lines);
    }
    // The keys live on the bottom border now (see `draw_overlay_frame`), which gives
    // the fields the two rows the hint used to take. NO `.wrap()` (like the `s` send overlay): every
    // row was truncated into its width budget above, so each Line is exactly one rendered row — the
    // fixed height stays exact and the scrollbar's row offset below is accurate.
    f.render_widget(Paragraph::new(Text::from(rows)), inner);
    if let Some(hit_sink) = hit_sink {
        *hit_sink.borrow_mut() = field_rows
            .into_iter()
            .filter_map(|(field, row)| {
                let row = u16::try_from(row).ok()?;
                (row < inner.height).then_some((
                    Rect::new(inner.x, inner.y.saturating_add(row), inner.width, 1),
                    field,
                ))
            })
            .collect();
    }

    // Overflow cue for the focused model field's inline list: a real ratatui scrollbar down the
    // list's right edge, its thumb tracking the current ● over the whole catalog. Only when the
    // catalog is longer than the reserved viewport. Clamped into `inner` so it can never draw past
    // the card border or panic on a tiny terminal.
    if let Some(span) = list_span
        && span.list_len > span.win
    {
        let y = inner
            .y
            .saturating_add(u16::try_from(span.row_off).unwrap_or(u16::MAX));
        let bottom = inner.y.saturating_add(inner.height);
        if y < bottom {
            let h = u16::try_from(span.win).unwrap_or(u16::MAX).min(bottom - y);
            let bar = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: h,
            };
            let mut state = ScrollbarState::new(span.list_len).position(span.current);
            let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("\u{2191}"))
                .end_symbol(Some("\u{2193}"))
                .thumb_symbol("\u{2588}");
            f.render_stateful_widget(sb, bar, &mut state);
        }
    }
}
