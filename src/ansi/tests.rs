//! Tests for the escape parser. Every case here is a shape a REAL `capture-pane -e`
//! produced (or a truncation of one), so the file doubles as the record of what the
//! two engines emit: the SGR map, the style that carries across a wrapped row, the
//! OSC 8 wrapper, and the adversarial inputs that must degrade to plain text.

use super::*;

/// `(text, style)` for every span of every line — the shape the assertions below
/// compare against, so a test says exactly what reaches the screen.
fn spans(capture: &str) -> Vec<Vec<(String, Style)>> {
    styled_lines(capture)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| (s.content.to_string(), s.style))
                .collect()
        })
        .collect()
}

/// The plain text of each line, with all styling dropped.
fn text(capture: &str) -> Vec<String> {
    styled_lines(capture)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// The style of the FIRST span of the first line — the common single-run case.
fn first_style(capture: &str) -> Style {
    styled_lines(capture)
        .first()
        .and_then(|l| l.spans.first())
        .map(|s| s.style)
        .unwrap_or_default()
}

#[test]
fn plain_text_is_one_unstyled_span_per_row() {
    assert_eq!(
        spans("hello\nworld"),
        vec![
            vec![("hello".to_string(), Style::default())],
            vec![("world".to_string(), Style::default())],
        ]
    );
}

#[test]
fn row_count_matches_str_lines() {
    // A trailing newline adds no phantom row; an interior blank row is kept so the
    // caller's "one pane row == one screen row" budget stays exact.
    assert_eq!(text("a\nb\n").len(), 2);
    assert_eq!(text("a\n\nb").len(), 3);
    assert_eq!(text("a").len(), 1);
    assert!(text("").is_empty());
    // A row holding ONLY escapes is still a row.
    assert_eq!(text("a\n\x1b[31m\nb"), vec!["a", "", "b"]);
}

#[test]
fn every_supported_sgr_code_maps() {
    // Table-driven over the whole supported map: attributes, selective clears, the
    // basic and bright palettes, indexed and truecolor, and the fg/bg resets.
    let cases: &[(&str, Style)] = &[
        ("\x1b[0mx", SGR_RESET),
        ("\x1b[mx", SGR_RESET),
        ("\x1b[1mx", Style::default().add_modifier(Modifier::BOLD)),
        ("\x1b[2mx", Style::default().add_modifier(Modifier::DIM)),
        ("\x1b[3mx", Style::default().add_modifier(Modifier::ITALIC)),
        (
            "\x1b[4mx",
            Style::default().add_modifier(Modifier::UNDERLINED),
        ),
        (
            "\x1b[7mx",
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        (
            "\x1b[9mx",
            Style::default().add_modifier(Modifier::CROSSED_OUT),
        ),
        // Selective clears (codex territory; claude only ever sends a full `0m`).
        ("\x1b[1m\x1b[2m\x1b[22mx", Style::default()),
        ("\x1b[3m\x1b[23mx", Style::default()),
        ("\x1b[4m\x1b[24mx", Style::default()),
        ("\x1b[7m\x1b[27mx", Style::default()),
        ("\x1b[9m\x1b[29mx", Style::default()),
        // Basic fg/bg and their bright halves.
        ("\x1b[30mx", Style::default().fg(Color::Black)),
        ("\x1b[31mx", Style::default().fg(Color::Red)),
        ("\x1b[37mx", Style::default().fg(Color::Gray)),
        ("\x1b[90mx", Style::default().fg(Color::DarkGray)),
        ("\x1b[97mx", Style::default().fg(Color::White)),
        ("\x1b[40mx", Style::default().bg(Color::Black)),
        ("\x1b[47mx", Style::default().bg(Color::Gray)),
        ("\x1b[100mx", Style::default().bg(Color::DarkGray)),
        ("\x1b[107mx", Style::default().bg(Color::White)),
        // Indexed — claude's 22 fg / 5 bg codes all take this path.
        ("\x1b[38;5;246mx", Style::default().fg(Color::Indexed(246))),
        ("\x1b[48;5;22mx", Style::default().bg(Color::Indexed(22))),
        ("\x1b[48;5;52mx", Style::default().bg(Color::Indexed(52))),
        ("\x1b[38;5;0mx", Style::default().fg(Color::Indexed(0))),
        ("\x1b[38;5;255mx", Style::default().fg(Color::Indexed(255))),
        // Truecolor — codex's status line.
        (
            "\x1b[38;2;10;20;30mx",
            Style::default().fg(Color::Rgb(10, 20, 30)),
        ),
        (
            "\x1b[48;2;0;0;0mx",
            Style::default().bg(Color::Rgb(0, 0, 0)),
        ),
        // The fg/bg-only resets claude uses to close a colour run.
        ("\x1b[31m\x1b[39mx", Style::default().fg(Color::Reset)),
        ("\x1b[41m\x1b[49mx", Style::default().bg(Color::Reset)),
    ];
    for (raw, want) in cases {
        assert_eq!(first_style(raw), *want, "SGR mapping for {raw:?}");
    }
}

#[test]
fn full_reset_clears_fg_bg_and_modifiers_together() {
    // claude closes bold with a bare `0m` and emits ZERO selective clears, so a `0m`
    // that only cleared modifiers would leave its colours running forever.
    let s = first_style("\x1b[1m\x1b[38;5;246m\x1b[48;5;22m\x1b[0mx");
    assert_eq!(s.fg, Some(Color::Reset));
    assert_eq!(s.bg, Some(Color::Reset));
    assert!(s.add_modifier.is_empty(), "modifiers survived a 0m: {s:?}");
}

#[test]
fn compound_sgr_applies_every_parameter() {
    // codex emits these; claude never does. `0;1m` must reset AND go bold.
    let s = first_style("\x1b[0;1mx");
    assert_eq!(s.fg, Some(Color::Reset));
    assert!(s.add_modifier.contains(Modifier::BOLD));
    let s = first_style("\x1b[1;2mx");
    assert!(s.add_modifier.contains(Modifier::BOLD | Modifier::DIM));
    // A colour and an attribute in one list, colour last.
    let s = first_style("\x1b[1;38;5;153mx");
    assert!(s.add_modifier.contains(Modifier::BOLD));
    assert_eq!(s.fg, Some(Color::Indexed(153)));
    // …and colour first, so the parameters after a `38;5;N` still get read.
    let s = first_style("\x1b[38;5;153;1mx");
    assert!(s.add_modifier.contains(Modifier::BOLD));
    assert_eq!(s.fg, Some(Color::Indexed(153)));
}

#[test]
fn one_row_splits_into_a_span_per_style_run() {
    // claude's real shape: an attribute run closed by a full reset, then the fg
    // immediately re-established (`…1 ESC[0m ESC[38;5;246m file…`).
    let out = spans("\x1b[1m1\x1b[0m\x1b[38;5;246m file");
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0],
        vec![
            (
                "1".to_string(),
                Style::default().add_modifier(Modifier::BOLD)
            ),
            (
                " file".to_string(),
                Style::default()
                    .fg(Color::Indexed(246))
                    .bg(Color::Reset)
                    .add_modifier(Modifier::empty())
            ),
        ]
    );
}

#[test]
fn style_carries_across_a_wrapped_run_at_80_columns() {
    // THE load-bearing case. `-e` is a delta stream over the WHOLE capture, so a run
    // wider than the pane emits its SGR on the first row and nothing on the
    // continuation rows — and production panes are 80x24 (`launch_interactive`
    // passes no `-x`/`-y`), so this is the common case, not a corner.
    let row = "x".repeat(80);
    let capture = format!("\x1b[38;5;153m{row}\n{row}\ntail\x1b[39m done");
    let out = spans(&capture);
    assert_eq!(out.len(), 3, "three rows: {out:?}");
    let coloured = Style::default().fg(Color::Indexed(153));
    assert_eq!(out[0], vec![(row.clone(), coloured)]);
    // The continuation row carries NO SGR of its own and must still be coloured —
    // a per-line-reset parser renders it white, which IS the reported bug.
    assert_eq!(out[1], vec![(row.clone(), coloured)]);
    assert_eq!(
        out[2],
        vec![
            ("tail".to_string(), coloured),
            (" done".to_string(), Style::default().fg(Color::Reset)),
        ]
    );
}

#[test]
fn style_carries_across_a_row_that_holds_only_text() {
    // The verified 20-column round trip: SGR + 20 chars, a row with NO leading SGR,
    // then 7 chars and the closing `39m`.
    let out = spans("\x1b[38;5;153maaaaaaaaaaaaaaaaaaaa\nbbbbbbb\x1b[39m");
    let coloured = Style::default().fg(Color::Indexed(153));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], vec![("a".repeat(20), coloured)]);
    assert_eq!(out[1], vec![("bbbbbbb".to_string(), coloured)]);
}

#[test]
fn osc8_wrapper_is_stripped_and_the_link_text_kept() {
    // OSC 8 wraps filenames inside ordinary tool rows, not just the welcome banner,
    // and it is a TMUX feature — so it must be handled whatever the engine.
    let raw =
        "\u{1b}[1mWrite\u{1b}[0m(\u{1b}]8;id=ab;file:///tmp/p.txt\u{1b}\\p.txt\u{1b}]8;;\u{1b}\\)";
    assert_eq!(text(raw), vec!["Write(p.txt)"]);
    // The link text inherits the surrounding style — it gets no style of its own.
    // (Stripping the wrapper does split the run in three; they are not merged
    // because the OSC bytes sit between them, so merging would mean copying.)
    let out = spans(raw);
    let (bold, rest) = out[0].split_at(1);
    assert_eq!(
        bold[0],
        (
            "Write".to_string(),
            Style::default().add_modifier(Modifier::BOLD)
        )
    );
    let after_reset = Style::default().fg(Color::Reset).bg(Color::Reset);
    assert!(
        rest.iter().all(|(_, s)| *s == after_reset),
        "the link text must inherit the surrounding style: {out:?}"
    );
}

#[test]
fn osc_terminated_by_bel_is_also_stripped() {
    assert_eq!(text("a\u{1b}]8;;http://x\u{7}b"), vec!["ab"]);
}

#[test]
fn unterminated_osc_never_swallows_the_rest_of_the_pane() {
    // Give up at the row boundary: at worst the tail of ONE row is lost.
    assert_eq!(
        text("head\u{1b}]8;id=x;file:///tmp/p\nsecond row\nthird row"),
        vec!["head", "second row", "third row"]
    );
    // Unterminated at the very end of the capture, with nothing after it.
    assert_eq!(text("head\u{1b}]8;id=x"), vec!["head"]);
}

#[test]
fn a_capture_cut_mid_escape_degrades_without_panicking() {
    // tmux can hand back a capture truncated anywhere. Every prefix of a real,
    // fully-styled row must parse, and none may lose a row that follows.
    let full = "\u{1b}[1mA\u{1b}[0m\u{1b}[38;5;246mB\u{1b}]8;;file:///t\u{1b}\\C\u{1b}]8;;\u{1b}\\\n\u{1b}[31mD";
    for cut in 0..=full.len() {
        let Some(prefix) = full.get(..cut) else {
            continue; // not a char boundary — nothing to test
        };
        let out = text(prefix);
        // The invariant is "no panic, and never more rows than the input has".
        assert!(out.len() <= 2, "cut {cut} produced {out:?}");
    }
    // Specific truncations, spelled out.
    assert_eq!(text("hi\u{1b}"), vec!["hi"], "lone trailing ESC");
    assert_eq!(text("hi\u{1b}["), vec!["hi"], "bare CSI introducer");
    assert_eq!(text("hi\u{1b}[38;5;"), vec!["hi"], "CSI cut mid-parameters");
    assert_eq!(text("hi\u{1b}]"), vec!["hi"], "bare OSC introducer");
    assert_eq!(text("hi\u{1b}\\"), vec!["hi"], "bare ST");
}

#[test]
fn a_malformed_escape_cannot_eat_the_following_row() {
    // A CSI interrupted by the row break: the escape is dropped, the `\n` is still
    // a row break, and the next row renders in full.
    assert_eq!(text("a\u{1b}[38;5\nb"), vec!["a", "b"]);
}

#[test]
fn malformed_colour_parameters_cost_the_colour_not_the_text() {
    let cases: &[&str] = &[
        "\x1b[38;5;mx",          // no index at all
        "\x1b[38;5;999mx",       // out of u8 range
        "\x1b[38;5mx",           // truncated selector
        "\x1b[38mx",             // no selector
        "\x1b[38;9;1mx",         // unknown selector
        "\x1b[38;2;1;2mx",       // truecolor missing its blue
        "\x1b[38;2;1;2;9999mx",  // truecolor component out of range
        "\x1b[38:5:1mx",         // colon sub-parameter form (proven absent)
        "\x1b[99999999999999mx", // parameter too large for u32
    ];
    for raw in cases {
        let out = text(raw);
        assert_eq!(out, vec!["x"], "text was dropped for {raw:?}");
        assert_eq!(
            first_style(raw).fg,
            None,
            "a malformed colour must leave fg untouched: {raw:?}"
        );
    }
}

#[test]
fn a_very_long_parameter_list_is_handled() {
    let mut raw = String::from("\x1b[");
    for _ in 0..5000 {
        raw.push_str("1;");
    }
    raw.push_str("38;5;42mx");
    let s = first_style(&raw);
    assert!(s.add_modifier.contains(Modifier::BOLD));
    assert_eq!(s.fg, Some(Color::Indexed(42)));
}

#[test]
fn literal_text_that_merely_looks_like_an_escape_is_kept() {
    // No ESC byte ⇒ nothing to strip. This is what a transcript quoting ANSI codes
    // (or a `[38;5;1m` in a code block) looks like.
    let raw = "see [38;5;1m and ]8;;http://x for details";
    assert_eq!(text(raw), vec![raw.to_string()]);
    assert_eq!(first_style(raw), Style::default());
}

#[test]
fn non_sgr_csi_is_consumed_and_ignored() {
    // `-e` serialises tmux's cell grid, so these CANNOT appear (module docs). If one
    // ever did, it must not reach the screen as text and must not change any style.
    assert_eq!(text("a\u{1b}[2Jb\u{1b}[?25lc\u{1b}[10;20Hd"), vec!["abcd"]);
    assert_eq!(first_style("\u{1b}[2Jx"), Style::default());
}

#[test]
fn carriage_return_before_a_row_break_is_dropped() {
    assert_eq!(text("a\r\nb"), vec!["a", "b"]);
}

#[test]
fn has_visible_text_looks_past_the_escapes() {
    // The bug this guards: escapes are not whitespace, so `trim()` calls an
    // escapes-only capture non-empty and the preview then claims the pane has output.
    assert!(!has_visible_text(""));
    assert!(!has_visible_text("   \n \n"));
    assert!(
        !has_visible_text("\u{1b}[38;5;153m\u{1b}[0m\n\u{1b}[39m   \n"),
        "an escapes-only capture has no visible text"
    );
    assert!(
        !"\u{1b}[38;5;153m".trim().is_empty(),
        "…and `trim` disagrees"
    );
    assert!(has_visible_text("\u{1b}[31mx\u{1b}[0m"));
    assert!(has_visible_text("plain"));
}

#[test]
fn line_is_blank_matches_whitespace_only_rows() {
    let lines = styled_lines("a\n   \n\u{1b}[31m\n\u{1b}[31m b");
    assert_eq!(lines.len(), 4);
    assert!(!line_is_blank(&lines[0]));
    assert!(line_is_blank(&lines[1]), "whitespace-only row");
    assert!(line_is_blank(&lines[2]), "escapes-only row");
    assert!(!line_is_blank(&lines[3]));
}

#[test]
fn spans_borrow_the_capture_rather_than_copying_it() {
    // Zero-copy is a design promise (the render path parses the selected pane every
    // frame), so assert the `Cow` is actually borrowed.
    let capture = String::from("\u{1b}[31mred text");
    let lines = styled_lines(&capture);
    let span = &lines[0].spans[0];
    assert!(
        matches!(span.content, std::borrow::Cow::Borrowed(_)),
        "span copied the capture instead of borrowing it"
    );
}

#[test]
fn a_real_claude_row_shape_parses_end_to_end() {
    // Byte-exact but MINIMAL — the forms that actually occur, not a screen dump: a
    // reverse-video row, a diff pair using claude's two observed bg codes, and the
    // bold-tool-name + OSC-8-filename row. No machine-specific paths or model names.
    let capture = concat!(
        "\u{1b}[7m NORMAL \u{1b}[0m\n",
        "\u{1b}[48;5;22m+ added\u{1b}[49m\n",
        "\u{1b}[48;5;52m- removed\u{1b}[49m\n",
        "\u{1b}[2m\u{1b}[38;5;246m\u{1b}[1mWrite\u{1b}[0m(",
        "\u{1b}]8;;file:///p.txt\u{1b}\\p.txt\u{1b}]8;;\u{1b}\\)\n",
    );
    let out = spans(capture);
    assert_eq!(out.len(), 4, "{out:?}");
    assert_eq!(
        out[0][0],
        (
            " NORMAL ".to_string(),
            Style::default().add_modifier(Modifier::REVERSED)
        )
    );
    // Note the `fg(Reset)` on every row below: it is the row-1 `0m` still in force,
    // carried forward. That is faithful — a real terminal does exactly the same —
    // and it is the same carry the wrapped-run tests above rely on.
    assert_eq!(
        out[1][0],
        (
            "+ added".to_string(),
            Style::default().fg(Color::Reset).bg(Color::Indexed(22))
        )
    );
    assert_eq!(
        out[2][0],
        (
            "- removed".to_string(),
            Style::default().fg(Color::Reset).bg(Color::Indexed(52))
        )
    );
    // Dim + indexed fg + bold accumulate, then `0m` drops all three at once.
    assert_eq!(
        out[3][0],
        (
            "Write".to_string(),
            Style::default()
                .fg(Color::Indexed(246))
                .bg(Color::Reset)
                .add_modifier(Modifier::DIM | Modifier::BOLD)
        )
    );
    let tool_row: String = out[3].iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(tool_row, "Write(p.txt)", "OSC 8 wrapper stripped: {out:?}");
}

#[test]
fn a_real_codex_row_shape_parses_end_to_end() {
    // codex's distinguishing forms: compound SGR, italic, and truecolor. Its
    // truecolor is config-gated, so this asserts the PARSE, never its presence.
    let out =
        spans("\u{1b}[0;2mthinking\u{1b}[0m \u{1b}[3mnote\u{1b}[0m\n\u{1b}[38;2;90;90;90mstatus");
    assert_eq!(out.len(), 2);
    assert!(out[0][0].1.add_modifier.contains(Modifier::DIM));
    assert_eq!(out[0][0].0, "thinking");
    assert!(
        out[0]
            .iter()
            .any(|(t, s)| t == "note" && s.add_modifier.contains(Modifier::ITALIC)),
        "italic run missing: {out:?}"
    );
    assert_eq!(out[1][0].1.fg, Some(Color::Rgb(90, 90, 90)));
}
