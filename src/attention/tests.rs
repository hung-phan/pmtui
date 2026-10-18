use super::*;
use crate::registry::Mode;
use crate::state::{Stop, Tier};
use ratatui::text::Span;

fn stop(kind: &str, risk_class: RiskClass) -> Stop {
    Stop {
        id: format!("stop-{kind}"),
        kind: kind.into(),
        risk_class,
        question: String::new(),
        options: Vec::new(),
        context_ref: None,
        status: "open".into(),
    }
}

fn view(posture: Posture) -> ProjectView {
    ProjectView {
        id: "bot".into(),
        display_name: None,
        work_summary: None,
        project_name: None,
        forked_from: None,
        incomplete_fork: false,
        spawned_by: None,
        spawned_by_label: None,
        spawn_staged: false,
        job: false,
        job_commit: None,
        job_branch: None,
        enabled: true,
        tier: Some(Tier::Autopilot),
        posture,
        next_action: String::new(),
        step_id: 1,
        last_activity: None,
        stops: Vec::new(),
        oldest_stop_since: None,
        mode: Mode::AgentLoop,
        engine: None,
        session_live: false,
        human_attached: false,
        agent_working: None,
        autopilot_events: Vec::new(),
        turn_trace: Vec::new(),
        decision_digest: crate::job::DecisionCounters::default(),
        advice_inflight: None,
        decider_live: false,
        advice_queue: Vec::new(),
        decider_runs: Vec::new(),
        decider_engine: Some(crate::registry::Engine::Claude),
        decider_model: None,
    }
}

#[test]
fn every_glyph_is_one_column() {
    for g in GLYPHS {
        assert_eq!(
            Span::raw(*g).width(),
            1,
            "{g:?} is not one display column — `text_cols` would undercount it \
             and the row truncator would evict a trailing chip"
        );
        assert_eq!(g.chars().count(), 1, "{g:?} is more than one char");
    }
}

#[test]
fn level_is_hard_for_stuck_with_no_stops() {
    // The bug this module exists to close: a driver-Timeout `Stuck` row carries no
    // stops at all, so a stops-only derivation called it Soft and painted the SOFT
    // yellow fill under the stuck bucket's red `✕` — the glyph and the fill
    // disagreeing about severity on one row.
    let v = view(Posture::Stuck);
    assert!(v.stops.is_empty());
    assert_eq!(level(&v), Level::Hard);
    // The HUE is the theme's failure role, not a named ANSI slot: chrome takes every colour
    // from `crate::theme` now, so this asserts the ROLE the level maps to.
    assert_eq!(level(&v).hue(), Some(crate::theme::hard()));
}

#[test]
fn needs_you_is_soft_until_a_stop_escalates_to_hard() {
    let mut v = view(Posture::NeedsYou);
    v.stops = vec![stop("ambiguity", RiskClass::Medium)];
    assert_eq!(level(&v), Level::Soft);
    // `confirm_done` is forced Hard by `policy::effective_risk` no matter what risk
    // the agent declared, so the row must follow the POLICY, not the claim.
    v.stops = vec![stop("confirm_done", RiskClass::Low)];
    assert_eq!(level(&v), Level::Hard);
}

#[test]
fn a_disabled_row_is_off_however_loudly_it_would_otherwise_shout() {
    let mut v = view(Posture::Stuck);
    v.enabled = false;
    assert_eq!(level(&v), Level::Off);
    assert!(!level(&v).is_filled());
    // …but the level underneath survives, so the row keeps its hue.
    assert_eq!(level_active(&v), Level::Hard);
    let s = label_style_for(&v);
    assert_eq!(s.fg, Some(crate::theme::hard()));
    assert!(
        !s.add_modifier.contains(Modifier::REVERSED),
        "a disabled row must not carry a fill under the whole-row DIM"
    );
}

#[test]
fn working_is_live_and_never_filled() {
    assert_eq!(level(&view(Posture::Working)), Level::Live);
    assert_eq!(level(&view(Posture::Running)), Level::Live);
    assert!(!Level::Live.is_filled());
    // …and it finally has a foreground of its own; it used to be `DIM` with none,
    // which is the same rendering as `done`.
    assert_eq!(Level::Live.label_style().fg, Some(crate::theme::live()));
}

#[test]
fn calm_and_idle_are_untouched() {
    assert_eq!(level(&view(Posture::Fresh)), Level::Calm);
    assert_eq!(level(&view(Posture::Monitoring)), Level::Calm);
    assert_eq!(level(&view(Posture::Done)), Level::Calm);
    assert_eq!(Level::Calm.label_style(), text_dim());
}

#[test]
fn no_chrome_style_sets_an_absolute_background() {
    // The whole rule: a background is reached by REVERSING the theme's own pair,
    // never by naming one. A `.bg()` here would invert under the list's
    // `highlight_style` exactly when the row is selected.
    for (name, s) in [
        ("fill_hard", fill_hard()),
        ("fill_soft", fill_soft()),
        ("text_live", text_live()),
        ("text_dim", text_dim()),
        ("chip_brand", chip_brand()),
    ] {
        assert!(s.bg.is_none(), "{name} sets a background colour");
        // Anything meant to read as a field must actually reverse; anything else
        // must not, or plain text would look like a badge.
        let reversed = s.add_modifier.contains(Modifier::REVERSED);
        let is_fill = matches!(name, "fill_hard" | "fill_soft" | "chip_brand");
        assert_eq!(reversed, is_fill, "{name}: REVERSED is wrong for its role");
    }
}

// `the_badges_are_the_same_width_so_severity_cannot_be_shortened` lived here. It guarded a
// real hazard — ` !!!` truncated to ` !!` IS the milder badge — that cannot occur now that
// there is no badge to truncate.

#[test]
fn the_bucket_glyphs_are_the_shipped_partition() {
    // Pinned deliberately: `status_category` in pmtui feeds BOTH the header counts
    // and the row glyphs from this table, and a fifth glyph (say, hard-vs-soft
    // needs-you) would break that one-row-one-bucket invariant.
    assert_eq!(bucket_glyph(0), ("◐", crate::theme::soft()));
    assert_eq!(bucket_glyph(1), ("●", crate::theme::live()));
    assert_eq!(bucket_glyph(2), ("○", crate::theme::rule()));
    assert_eq!(bucket_glyph(3), ("✕", crate::theme::hard()));
    // …and the four are distinguishable, whatever the theme: the partition is only readable
    // if each bucket's hue differs from the others'.
    let hues: std::collections::HashSet<String> =
        (0..4).map(|b| format!("{:?}", bucket_glyph(b).1)).collect();
    assert_eq!(hues.len(), 4, "two buckets share a hue: {hues:?}");
}

/// EVERY LEVEL'S HUE, including the two the suite never asked for.
///
/// These were uncovered production lines hiding behind a measurement artefact: while this test module
/// lived inside `attention.rs`, its own lines counted toward that file's coverage, so the file passed
/// the 95% gate with `Level::hue`'s `Soft` and `Live` arms never evaluated. Moving the tests out made
/// the gap visible. The hues matter because severity is carried by hue as well as by glyph and word.
#[test]
fn every_level_reports_the_hue_it_speaks_in() {
    assert_eq!(Level::Hard.hue(), Some(crate::theme::hard()));
    assert_eq!(Level::Soft.hue(), Some(crate::theme::soft()));
    assert_eq!(Level::Live.hue(), Some(crate::theme::live()));
    assert_eq!(Level::Calm.hue(), None, "calm has no hue of its own");
    assert_eq!(Level::Off.hue(), None, "and neither does off");
    // Hard and Soft must not collapse into one another: on the row that difference IS the message.
    assert_ne!(Level::Hard.hue(), Level::Soft.hue());
}

/// A DISABLED ROW KEEPS ITS HUE AND DROPS ITS FILL, down both paths of `label_style_for`.
///
/// `DIM` stacked on a `BOLD` reverse-video field emits `\e[1m\e[2m`, whose text intensity is
/// implementation-defined — so a disabled row with a fill could render BRIGHTER than an active one.
/// That is why `Off` resolves through the level underneath, and why both arms of that lookup are
/// asserted here: a row whose underlying level has a hue, and one whose does not.
#[test]
fn a_disabled_row_keeps_the_hue_underneath_and_never_a_fill() {
    // `Off` itself is always the dim text style, never a fill.
    assert_eq!(Level::Off.label_style(), text_dim());
    assert!(!Level::Off.is_filled());

    // Disabled over a level that HAS a hue: the hue survives, the fill does not.
    let mut needs_you = view(Posture::NeedsYou);
    needs_you.enabled = false;
    assert_eq!(level(&needs_you), Level::Off);
    let style = label_style_for(&needs_you);
    assert_eq!(style.fg, Some(crate::theme::soft()), "the hue underneath");
    assert_eq!(
        style.bg, None,
        "and no fill, whatever the level beneath was"
    );

    // Disabled over a level with NO hue: the other arm, which falls back to dim text.
    let mut calm = view(Posture::Monitoring);
    calm.enabled = false;
    assert_eq!(level_active(&calm), Level::Calm);
    assert_eq!(label_style_for(&calm), text_dim());

    // An ENABLED row still answers from its own level, so the `Off` branch is the only special one.
    let live = view(Posture::Working);
    assert_eq!(label_style_for(&live), Level::Live.label_style());
}
