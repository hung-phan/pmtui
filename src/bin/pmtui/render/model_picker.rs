//! The reusable model picker (`e` decider / `w` worker): a two-stage single-select list over the
//! dashboard. Its own file, one-per-overlay like `sending`/`confirm`, because its geometry (a header
//! line plus one row per choice) is its own. The decider walks Engine→Model; the worker picks Model
//! only. The catalog is read from `app.model_catalog` — filled when the picker OPENED — so the render
//! path does NO discovery I/O.

use crate::*;

/// The model picker overlay — a fixed-height single-select list.
///
/// `▸` is the movable cursor; `●`/`○` is the ON-DISK "current" marker, INDEPENDENT of the cursor on
/// both stages. The `Engine` stage marks the stored engine passed in; the `Model` stage marks the
/// row whose catalog value equals `stored_model` (or the `(default)` row when it is `None`), and
/// marks nothing when the stored value is not in this engine's catalog. Both carry a `(current)`
/// suffix. Reading `stored_model` (a value carried on the mode) rather than the cursor is what keeps
/// "which is set now" visible as `▸` moves — see the `UiMode::ModelPicker` doc.
// The picker's six state fields (target/id/stage/engine/cursor/stored_model) come straight off the
// `UiMode::ModelPicker` arm, plus `f`/`area`/`app` — the same "pass the destructured overlay fields"
// shape the other overlay renderers use, just wider than the lint's default.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_model_picker(
    f: &mut Frame,
    area: Rect,
    app: &App,
    id: &str,
    target: PickTarget,
    stage: PickStage,
    engine: Engine,
    cursor: usize,
    stored_model: Option<&str>,
) {
    let title = match target {
        PickTarget::Decider => format!("Decider \u{b7} {}", truncate(id, 24)),
        PickTarget::Worker => format!("Worker \u{b7} {}", truncate(id, 24)),
    };
    // `rows` = each choice's label; `current_ix` = the row the `●` on-disk marker sits on, computed
    // WITHOUT the cursor so it never drifts as `▸` moves. `None` ⇒ no row is marked current.
    let (subheader, rows, current_ix): (String, Vec<String>, Option<usize>) = match stage {
        PickStage::Engine => {
            let rows = Engine::ALL.iter().map(|e| e.label().to_string()).collect();
            let current = Engine::ALL.iter().position(|&e| e == engine);
            // Say what Enter does here: this is step 1 of 2 — picking the engine advances to its
            // model list. Without this, the engine screen reads like the whole picker and the model
            // step goes unnoticed. Keeps the word "Engine" (a test anchors on it).
            (
                "Engine \u{b7} Enter \u{2192} choose its model".to_string(),
                rows,
                current,
            )
        }
        PickStage::Model => {
            // Cached catalog ONLY — the render path does no discovery I/O (the catalog was filled
            // when the picker opened). Missing ⇒ just the "(default)" row.
            let catalog = app
                .model_catalog
                .get(&engine)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut rows = Vec::with_capacity(1 + catalog.len());
            rows.push("(default)".to_string());
            rows.extend(catalog.iter().map(|m| m.label.clone()));
            // Same mapping as `App::model_cursor_for`, but against `stored_model` (not the cursor):
            // `None` ⇒ the `(default)` row (0); a stored value in the catalog ⇒ `1 + its index`; a
            // stored value this engine's catalog lacks ⇒ `None` (mark nothing).
            let current = match stored_model {
                None => Some(0),
                Some(v) => catalog.iter().position(|m| m.value == v).map(|i| i + 1),
            };
            (format!("Model \u{b7} {}", engine.label()), rows, current)
        }
    };

    // header line + blank + one row per choice. The footer names what Enter does at THIS stage: the
    // Engine stage advances to the model list (so say so — the model step is otherwise easy to miss),
    // the Model stage commits.
    let hint = match stage {
        PickStage::Engine => {
            "\u{2191}\u{2193} move \u{b7} enter \u{2192} choose model \u{b7} esc cancel"
        }
        PickStage::Model => "\u{2191}\u{2193} move \u{b7} enter select \u{b7} esc back",
    };
    let inner = draw_overlay_frame(
        f,
        area,
        60,
        overlay_h(2 + rows.len()),
        &title,
        agent_manager::theme::accent(),
        hint,
    );
    let text_w = usize::from(inner.width);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut body = vec![
        Line::styled(truncate(&subheader, text_w), dim),
        Line::raw(""),
    ];
    let picked = cursor.min(rows.len().saturating_sub(1));
    for (i, label) in rows.iter().enumerate() {
        let on = i == picked;
        // `●` = the ON-DISK current row (cursor-independent, both stages); `▸` = the movable cursor.
        let is_current = current_ix == Some(i);
        let focus = if on { "\u{25b8} " } else { "  " };
        let mark = if is_current { "\u{25cf}" } else { "\u{25cb}" };
        let suffix = if is_current { "  (current)" } else { "" };
        let text = format!("{focus}{mark} {label}{suffix}");
        body.push(Line::styled(
            truncate(&text, text_w),
            if on {
                Style::default()
                    .fg(agent_manager::theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ));
    }
    f.render_widget(Paragraph::new(Text::from(body)), inner);
}
