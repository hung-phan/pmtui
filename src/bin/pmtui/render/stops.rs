//! An open stop as a human reads it: the question and its numbered options, laid out
//! into whatever row budget the caller has. Shared by the preview's compact block and
//! the answer overlay, together with the wrapping both do — one layout, so the screen
//! that shows a decision and the screen that takes it cannot disagree about it.

use crate::*;
// Named explicitly, not left to the glob above, because the comment below points at this
// import to say where the shared text lives.
use agent_manager::state::stop_product_text;

// There was a `PREVIEW_MAX_STOP_LINES = 3` here: a flat per-stop row cap. It is gone because
// the cap that matters is the BLOCK's (half the pane, reserved out of the log's height), and
// a flat 3 spent that budget badly — on a 26-row pane it left the block using 3 of the 11 rows
// it was entitled to while the question sat truncated inside them. `stop_preview_lines` now
// takes the budget it has been given and lays out into it; the caller does the dividing.

// `stop_product_text` / `synthesized_stop_text` now live in `agent_manager::state`,
// beside `Stop` — see the import at the top of this file. They were defined HERE until a
// third surface turned up: `escalation::Escalation::for_stops` builds the desktop
// notification and had the identical bare-`kind` fallback, so a synthesized park told a
// human `[stop-…] confirm_done` and nothing else. A copy in the binary could not be shared
// with the library, and two copies is precisely how the kind-only fallback survived in two
// places at once in the first place.

/// The rows one stop contributes to the pinned `Stops` block, laid out into the `budget`
/// rows it has been given.
///
/// This block is the thing a human is supposed to ACT on, and it used to be the least
/// readable region on the screen. Three faults, all of them visible in one glance at a real
/// `confirm_done`:
///
/// * the row opened with a BOLD `[stop-test-0-1786869300]` — a 26-character opaque id in the
///   most prominent position on the line, which is worth nothing to a human deciding and
///   pushed everything that IS worth something to the right;
/// * the question was `truncate`d to ONE row and never wrapped, so two thirds of a three-line
///   question was simply gone;
/// * every option was concatenated onto one more truncated row (`1) … 2) … 3) …`), so the
///   last choice was routinely unreadable.
///
/// It now spends the rows it actually has, in a fixed priority ladder:
///
/// 1. the header — risk glyph, risk word, kind — with the id demoted to a dim tail;
/// 2. every option on its own row, when they fit with a row left over for the question;
/// 3. the question, WRAPPED into whatever remains.
///
/// Options outrank extra question rows because they are the actionable half and each is
/// short; the question outranks them at the very tightest (one spare row), because "what is
/// being asked" beats "what the choices are" when you can only have one — and `a` shows all
/// of it either way.
///
/// Never exceeds `budget`, because this block's height is RESERVED out of the transcript's:
/// a stop that grew freely would push the log off screen.
pub(crate) fn stop_preview_lines(s: &Stop, text_w: usize, budget: usize) -> Vec<Line<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    let dim = Style::default().fg(agent_manager::theme::rule());
    let risk = policy::effective_risk(s);
    // A GLYPH PER RISK, from the same vocabulary the buckets use, so severity is a
    // shape here too and not only a hue. Low risk was `Style::default()` — literally no
    // styling at all, i.e. indistinguishable from the agent's own question text.
    let (risk_glyph, risk_style) = match risk {
        RiskClass::Hard => ("\u{2715}", attention::fill_hard()),
        RiskClass::Medium => (
            "\u{25d0}",
            Style::default().fg(agent_manager::theme::soft()),
        ),
        RiskClass::Low => (attention::LOW_RISK, attention::text_dim()),
    };
    let question = stop_product_text(s);
    // Severity and subject FIRST, which is the order a human reads them in.
    //
    // The severity is a CHIP: its padding lives INSIDE the styled span, and a two-space plain
    // gap follows it. That matters because `Hard` is drawn with a REVERSED fill
    // (`attention::fill_hard`), so the span's own trailing space is part of the coloured block —
    // put the gap inside the style and the fill ends exactly where the next word begins, which
    // renders as `hard` and `confirm_done` glued together. The user's report on seeing it:
    // *"hard and confirm_done is rendering to close due to the background"*.
    //
    // Same construction as `keychip` in the keybar, for the same reason.
    let mut header = vec![
        Span::styled(format!(" {risk_glyph} {} ", risk_str(risk)), risk_style),
        Span::raw("  "),
    ];
    // The kind stays dim when there's a question beneath it and undimmed when it IS the
    // body — an older ledger with no text of any kind still reads as it always did.
    header.push(if question.is_empty() {
        Span::raw(s.kind.clone())
    } else {
        Span::styled(s.kind.clone(), dim)
    });
    // The id, DIM and last. It is still worth having on screen — it is how a human ties this
    // row to `answers.json`, the pmd log and an escalation — but it is reference material, not
    // the headline, and it only appears when it does not crowd out the words.
    // ` ✕ hard ` + the two-space gap + the kind.
    let used = 4 + risk_str(risk).len() + 2 + text_cols(&s.kind);
    if used + text_cols(&s.id) + 3 <= text_w {
        header.push(Span::styled(format!("   {}", s.id), dim));
    }
    let mut out = vec![Line::from(header)];

    let left = budget - 1;
    if left == 0 {
        return out;
    }
    // The body is indented two columns under the header, so the wrap width is not `text_w`.
    let body_w = text_w.saturating_sub(2);
    let q_want = if question.is_empty() {
        0
    } else {
        wrap_all(question, body_w).len()
    };
    let n = s.options.len();
    // ONE spare row goes to the question (see the ladder above). Otherwise: all the options
    // if they fit with a row to spare, else compacted onto a single row — a PARTIAL list is
    // worse than a compacted one, because three options showing two reads as two options.
    let opt_rows = if n == 0 || (left == 1 && q_want > 0) {
        0
    } else if n < left {
        // `n < left`, i.e. `n + 1 <= left`: all the options AND a row left for the question.
        n
    } else {
        1
    };
    let q_rows = q_want.min(left - opt_rows);
    // A BLANK ROW between the prose and the list of choices. Without it the question's last
    // wrapped line and the first option sit flush against each other and the eye cannot find
    // where one ends and the other begins — the user's words on seeing the new layout were
    // simply *"You need some spacing"*.
    //
    // Taken ONLY from a genuinely spare row. Buying it with a question row would trade a
    // separator for an elided sentence, and buying it with an option row would hide a choice —
    // both worse than no gap. So on a tight pane the rows stay flush and the spacing is the
    // first thing to go, which is the right end of the ladder for it.
    let gap = usize::from(q_rows > 0 && opt_rows > 0 && q_rows + opt_rows < left);
    for row in wrap_clamped(question, body_w, q_rows) {
        out.push(Line::raw(format!("  {row}")));
    }
    if gap == 1 {
        out.push(Line::raw(""));
    }
    if opt_rows == n && n > 0 {
        for (i, opt) in s.options.iter().enumerate() {
            out.push(Line::styled(
                format!("  {} {}", i + 1, truncate(opt, body_w.saturating_sub(2))),
                dim,
            ));
        }
    } else if opt_rows == 1 {
        out.push(Line::styled(
            format!("  {}", truncate(&numbered_options(&s.options), body_w)),
            dim,
        ));
    }
    out
}

/// The agent's offered choices as one compact `1) a  2) b` line, in the order it
/// listed them, so a human can answer with a number instead of re-reading the pane.
pub(crate) fn numbered_options(options: &[String]) -> String {
    options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}) {}", i + 1, o.trim()))
        .collect::<Vec<_>>()
        .join("  ")
}

/// Wrap `s` to `width` with no row budget, so every character remains available to the
/// scrolling Answer body. The computed ceiling is input-derived: even at width one, wrapping can
/// never require more rows than characters plus one. Reusing [`wrap_clamped`] keeps word breaks and
/// Unicode handling identical to fixed-budget surfaces without an arbitrary truncation threshold.
pub(crate) fn wrap_all(s: &str, width: usize) -> Vec<String> {
    wrap_clamped(s, width, s.chars().count().saturating_add(1))
}

/// Hard-wrap `s` into AT MOST `max_lines` rows of `width` columns, ellipsising the
/// last row when text remains. Breaks on whitespace where it can and mid-word only
/// for a word longer than the row. Used where the row budget is fixed and wrapping
/// must never grow the widget; [`wrap_all`] supplies an input-derived ceiling for scrolling
/// surfaces that preserve every character.
pub(crate) fn wrap_clamped(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut rows: Vec<String> = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() && rows.len() < max_lines {
        // Char-indexed throughout (a byte index from `str::rfind` would split
        // multi-byte text); the break is the last whitespace that fits, else a hard
        // cut for a single word longer than the row.
        let chars: Vec<char> = rest.chars().collect();
        if chars.len() <= width {
            rows.push(rest.to_string());
            rest = "";
            break;
        }
        let cut = chars[..=width]
            .iter()
            .rposition(|c| c.is_whitespace())
            .filter(|i| *i > 0)
            .unwrap_or(width);
        let head: String = chars[..cut].iter().collect();
        let consumed: usize = chars[..cut].iter().map(|c| c.len_utf8()).sum();
        rows.push(head.trim_end().to_string());
        rest = rest[consumed..].trim_start();
    }
    // Text left over with no rows to spare: mark the elision on the last row.
    if !rest.is_empty()
        && let Some(last) = rows.last_mut()
    {
        *last = truncate(&format!("{last} {rest}"), width);
    }
    rows
}
