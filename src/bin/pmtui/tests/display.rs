//! Pure display mappings and width-aware text rendering.

use super::*;

#[test]
fn no_wake_copy_distinguishes_known_and_unknown_cadence() {
    // `s` is the answer key; `a` is no longer bound in Normal mode.
    assert_eq!(
        no_wake_status("bot", Some(90)),
        "bot: no wake running right now — it runs every 90s; Enter to watch when one is live · s answer · d close"
    );
    assert_eq!(
        no_wake_status("bot", None),
        "bot: no wake running right now — Enter to watch when one is live · s answer · d close"
    );
}

#[test]
fn tier_risk_and_autonomy_labels_cover_every_value() {
    assert_eq!(tier_tag(Some(Tier::Autopilot)), "A");
    assert_eq!(tier_tag(Some(Tier::Standard)), "S");
    assert_eq!(tier_tag(None), "?");

    assert_eq!(risk_str(RiskClass::Low), "low");
    assert_eq!(risk_str(RiskClass::Medium), "medium");
    assert_eq!(risk_str(RiskClass::Hard), "hard");

    assert_eq!(tier_name(Tier::Autopilot), "autopilot");
    assert_eq!(tier_name(Tier::Standard), "standard");
    assert_eq!(
        autonomy_descriptor(Tier::Standard),
        "you drive it (pmd never types into it)"
    );
    assert_eq!(
        autonomy_descriptor(Tier::Autopilot),
        "hands-off (pmd drives it on its cadence)"
    );
}

#[test]
fn status_partition_tracks_pause_attention_activity_and_idle() {
    let mut paused = view("paused", Posture::Working, vec![]);
    paused.enabled = false;
    assert_eq!(status_category(&paused), 2);

    assert_eq!(status_category(&view("stuck", Posture::Stuck, vec![])), 3);
    assert_eq!(
        status_category(&view("waiting", Posture::NeedsYou, vec![])),
        0
    );

    let mut standard = view("standard", Posture::Monitoring, vec![]);
    standard.tier = Some(Tier::Standard);
    standard.session_live = false;
    assert_eq!(status_category(&standard), 2);
    standard.session_live = true;
    standard.agent_working = None;
    assert_eq!(status_category(&standard), 1);
    standard.agent_working = Some(false);
    assert_eq!(status_category(&standard), 2);

    let mut autopilot = view("autopilot", Posture::Monitoring, vec![]);
    autopilot.agent_working = Some(false);
    assert_eq!(status_category(&autopilot), 2);
    autopilot.agent_working = Some(true);
    assert_eq!(status_category(&autopilot), 1);
    autopilot.posture = Posture::Running;
    assert_eq!(status_category(&autopilot), 1);
    autopilot.posture = Posture::Fresh;
    assert_eq!(status_category(&autopilot), 2);
}

#[test]
fn glyphs_use_shape_and_colour_for_each_status_bucket() {
    for (category, glyph, colour) in [
        (0, "◐", agent_manager::theme::soft()),
        (1, "●", agent_manager::theme::live()),
        (2, "○", agent_manager::theme::rule()),
        (3, "✕", agent_manager::theme::hard()),
        (99, "○", agent_manager::theme::rule()),
    ] {
        assert_eq!(category_glyph(category), (glyph, colour));
    }

    let stuck = view("bot", Posture::Stuck, vec![]);
    assert_eq!(status_glyph(&stuck), ("✕", agent_manager::theme::hard()));
}

#[test]
fn truncation_and_text_width_count_terminal_columns() {
    assert_eq!(truncate("short", 8), "short");
    assert_eq!(truncate("abcdef", 4), "abc…");
    assert_eq!(truncate("abcdef", 0), "…");
    assert_eq!(truncate("a→b", 3), "a→b");
    assert_eq!(text_cols("a→b"), 3);

    // User-chosen labels may be CJK or emoji, which a terminal draws two cells wide. The
    // budget is columns, so a wide glyph that would straddle the ellipsis is dropped whole.
    assert_eq!(text_cols("发布协调"), 8);
    assert_eq!(text_cols("🚀 go"), 5);
    assert_eq!(truncate("发布协调工作流程", 16), "发布协调工作流程");
    assert_eq!(truncate("发布协调工作流程x", 16), "发布协调工作流…");
    assert_eq!(truncate("发布协调工作流程", 6), "发布…");
    assert_eq!(text_cols(&truncate("a发布协调", 5)), 4, "no half glyph");
    assert_eq!(truncate("🚀🚀🚀", 4), "🚀…");

    // A presentation sequence (base + U+FE0F) is two cells as drawn but 1+0 as separate chars,
    // so the cut walks graphemes the way ratatui's buffer does and never splits one.
    assert_eq!(text_cols("⚠\u{fe0f}"), 2);
    assert_eq!(
        truncate("⚠\u{fe0f}⚠\u{fe0f} production hotfix release", 16),
        "⚠\u{fe0f}⚠\u{fe0f} production…"
    );
    assert_eq!(
        truncate(&"❤\u{fe0f}".repeat(10), 16),
        format!("{}…", "❤\u{fe0f}".repeat(7))
    );
    assert_eq!(
        truncate(&"✔\u{fe0f}".repeat(9), 18),
        "✔\u{fe0f}".repeat(9),
        "nothing cut"
    );
    for label in [
        "⚠\u{fe0f}⚠\u{fe0f} production hotfix release",
        "✔\u{fe0f} release checklist done",
        "1\u{fe0f}\u{20e3}2\u{fe0f}\u{20e3}3\u{fe0f}\u{20e3} keycap countdown",
        "👩\u{200d}💻👩\u{200d}💻👩\u{200d}💻 pairing session",
    ] {
        assert!(text_cols(&truncate(label, 16)) <= 16, "{label}");
    }
}

#[test]
fn padding_fits_a_label_to_exact_columns() {
    assert_eq!(pad_cols("bot", 6), "bot   ");
    assert_eq!(pad_cols("发布", 6), "发布  ");
    assert_eq!(pad_cols("abcdefgh", 6), "abcde…");
    // A wide glyph that cannot fit before the ellipsis leaves one blank rather than
    // overflowing the field.
    assert_eq!(pad_cols("a发布协调", 5), "a发… ");
    for label in [
        "发布协调工作流程发布协调工作流程",
        "🚀 release 🚀🚀🚀🚀🚀🚀",
        "⚠\u{fe0f}⚠\u{fe0f} production hotfix release",
        "❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}",
        "1\u{fe0f}\u{20e3}2\u{fe0f}\u{20e3}3\u{fe0f}\u{20e3} keycap countdown",
        "ascii",
        "",
    ] {
        assert_eq!(text_cols(&pad_cols(label, 16)), 16, "{label}");
    }
}

#[test]
fn input_line_marks_the_caret_at_end_and_over_a_character() {
    let end = input_line(&Field::from("abc"), 8);
    assert_eq!(line_text(&end), "> abc_");
    assert!(!end.spans[2].style.add_modifier.contains(Modifier::REVERSED));

    let mut middle = Field::from("abc");
    middle.home();
    let middle = input_line(&middle, 8);
    assert_eq!(line_text(&middle), "> abc");
    assert_eq!(middle.spans[2].content, "a");
    assert!(
        middle.spans[2]
            .style
            .add_modifier
            .contains(Modifier::REVERSED)
    );
}

#[test]
fn goal_display_handles_empty_single_multiline_and_blank_text() {
    assert!(goal_field_display("").contains("required"));
    assert_eq!(goal_field_display("ship it"), "ship it_");
    assert_eq!(
        goal_field_display("\nfirst line\nsecond line"),
        "first line (+2 more lines)"
    );
    assert_eq!(goal_field_display("\n\n"), " (+1 more lines)");

    let long = format!("{}\nnext", "x".repeat(60));
    let shown = goal_field_display(&long);
    assert!(shown.starts_with(&format!("{}…", "x".repeat(47))));
    assert!(shown.ends_with("(+1 more lines)"));
}

#[test]
fn truncate_left_keeps_the_end_of_a_path() {
    // The candidate list shows whole paths that share a parent, so the TAIL is the only part worth
    // reading — cutting it would leave rows that look identical.
    assert_eq!(truncate_left("/a/b/project-alpha/", 12), "…ject-alpha/");
    assert_eq!(text_cols(&truncate_left("/a/b/project-alpha/", 12)), 12);
    // Nothing dropped, nothing marked.
    assert_eq!(truncate_left("/a/b/", 12), "/a/b/");
    assert_eq!(truncate_left("", 4), "");
    // A wide glyph is never split: dropping it whole keeps the result inside the budget.
    assert!(text_cols(&truncate_left("/a/\u{1f600}\u{1f600}/", 4)) <= 4);
    // A budget of zero still gets the marker that says text was dropped, and nothing more.
    assert_eq!(truncate_left("/a/b/", 0), "…");
}

#[test]
fn age_and_clock_labels_cover_boundaries_and_wraparound() {
    let now = 1_000_000;
    for (elapsed, expected) in [
        (0, "0s"),
        (59, "59s"),
        (60, "1m"),
        (3_600, "1h"),
        (86_400, "1d"),
        (604_800, "1w"),
        (31_536_000, "1y"),
    ] {
        assert_eq!(age_label(Some(now), now + elapsed), expected);
    }
    assert_eq!(age_label(None, now), "—");
    assert_eq!(age_label(Some(now + 1), now), "0s");
    assert_eq!(age_label(Some(0), i64::MAX), "99y");

    assert_eq!(fmt_clock(3_661, 0), "01:01:01");
    assert_eq!(fmt_clock(0, -3_600), "23:00:00");
    assert_eq!(fmt_clock(86_399, 3_600), "00:59:59");
    assert_eq!(clock_label(0).len(), 8);
}
