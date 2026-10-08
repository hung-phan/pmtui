//! EVERY CELL IS THE THEME'S. One sweep over every view and overlay, asserting no cell is left on the
//! terminal's own colours.
//!
//! The user asked for exactly this check — *"also remember to check all the texts as well to see if we
//! add the theme to everything"* — and it is worth a test rather than a look, because the failure is
//! invisible on the machine that ships it: an unstyled span inherits the terminal's ink, which on a dark
//! terminal with a dark theme looks perfect and on a light theme is white text on a white card.
//!
//! `Color::Reset` IS the failure, not the absence of a colour: ratatui patches cell styles, so a span
//! with no colour of its own keeps whatever the canvas painted, while `Reset` actively re-selects the
//! terminal's pair. The four places that produced it were `Clear` (the keybar row, the board detail
//! pane, the help card) and the agent's own `SGR 0`/`39`/`49` in a transcript.

use super::*;
use ratatui::style::Color;

/// One case of the sweep: a name for the failure message, and a way to build that state again.
type ViewCase = (&'static str, fn() -> UiMode);

/// Every state cheap to construct, named so a failure says which screen is wrong.
///
/// Built by a closure per case rather than a list of values, because `UiMode` is deliberately not
/// `Clone` and this sweep renders each state at more than one size.
fn every_view() -> Vec<ViewCase> {
    vec![
        ("sessions", || UiMode::Normal),
        ("tasks", || UiMode::Board),
        ("settings", || UiMode::Settings {
            cursor: 0,
            open: None,
        }),
        ("settings dropdown", || UiMode::Settings {
            cursor: 0,
            open: Some(3),
        }),
        ("create", || UiMode::Creating(CreateForm::new())),
        ("answer", || UiMode::Answering {
            input: "B".into(),
            choice: 0,
            scroll: 0,
        }),
        ("confirm", || UiMode::Confirming {
            id: "auth-rewrite".into(),
            session: "pmi-x".into(),
            what: Confirmable::Remove,
        }),
        ("goal", || UiMode::EditingGoal {
            id: "auth-rewrite".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "ship it".into(),
            input: goal_buf("ship it faster"),
            then_autopilot: false,
        }),
        ("cadence", || UiMode::EditingCadence {
            id: "auth-rewrite".into(),
            root: PathBuf::from("/nonexistent"),
            then_autopilot: false,
            current: 300,
            input: "600".into(),
        }),
        ("rename", || UiMode::Renaming {
            id: "auth-rewrite".into(),
            current: Some("auth-rewrite".into()),
            input: "auth".into(),
        }),
        ("send", || UiMode::Sending {
            target: SendTarget {
                id: "auth-rewrite".into(),
                root: PathBuf::from("/nonexistent"),
                session: "pm-auth".into(),
                agent_loop: true,
                driven: false,
                in_chat: false,
            },
            input: Composer::from_text("status?".into()),
        }),
        ("switcher", || UiMode::Switching {
            query: Field::default(),
            cursor: 0,
            items: Vec::new(),
        }),
        ("help", || UiMode::Help { scroll: 0 }),
    ]
}

/// The cells that are NOT the theme's, as `(row, column, which side)` — empty when the frame is wholly
/// themed.
fn untouched(terminal: &Terminal<TestBackend>) -> Vec<(u16, u16, &'static str)> {
    let buf = terminal.backend().buffer();
    let mut bad = Vec::new();
    for y in buf.area.top()..buf.area.bottom() {
        for x in buf.area.left()..buf.area.right() {
            let cell = &buf[(x, y)];
            if cell.fg == Color::Reset {
                bad.push((y, x, "foreground"));
            }
            if cell.bg == Color::Reset {
                bad.push((y, x, "background"));
            }
        }
    }
    bad
}

/// THE WHOLE FRAME, in every view: no cell keeps the terminal's colours.
#[test]
fn every_view_paints_every_cell_from_the_theme() {
    let projects = vec![
        view(
            "auth-rewrite",
            Posture::NeedsYou,
            vec![stop("s1", "ambiguity", RiskClass::Medium)],
        ),
        view("scratch", Posture::Working, vec![]),
    ];
    for (name, mode) in every_view() {
        // Two sizes, because the narrow one takes different branches — shed tiers, yielded controls, a
        // keybar down to key badges — and those branches draw their own spans.
        for (w, h) in [(120u16, 30u16), (60, 16)] {
            let app = app_with(projects.clone(), mode());
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| render(f, &app)).expect("render");
            let bad = untouched(&terminal);
            assert!(
                bad.is_empty(),
                "{name} at {w}x{h} left {} cell(s) on the terminal's own colours, first at {:?}:\n{}",
                bad.len(),
                bad.first(),
                screen_text(&terminal)
            );
        }
    }
}

/// The too-small state is a view too — it is the one a human sees while dragging a window edge.
#[test]
fn the_too_small_screen_is_themed() {
    let app = app_with(Vec::new(), UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(MIN_W - 1, MIN_H - 1)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    assert!(
        untouched(&terminal).is_empty(),
        "{}",
        screen_text(&terminal)
    );
}

/// A TRANSCRIPT's "default colour" is the theme's, not the terminal's.
///
/// An agent that resets its colours (`SGR 0`, `39`, `49` — every REPL does, constantly) was handing the
/// preview `Color::Reset`, which re-selects the terminal's pair over a themed pane. What the agent
/// actually chose is untouched.
#[test]
fn agent_output_resets_to_the_theme_not_the_terminal() {
    let capture = concat!(
        "\u{1b}[31mred stays red\u{1b}[0m\n",
        "plain agent text\n",
        "\u{1b}[39m\u{1b}[49mexplicitly default\n",
    );
    let (lines, _) = pane_window(capture, 8, 0);
    let spans: Vec<Span> = lines
        .iter()
        .flat_map(|line| line.spans.iter().cloned())
        .collect();
    assert!(!spans.is_empty());
    for span in &spans {
        assert_ne!(
            span.style.fg,
            Some(Color::Reset),
            "{:?} kept the terminal's ink",
            span.content
        );
        assert!(
            span.style.fg.is_some(),
            "{:?} has no ink at all",
            span.content
        );
        assert_ne!(span.style.bg, Some(Color::Reset), "{:?}", span.content);
    }
    // The agent's OWN colour survives — this is a pass-through, not a repaint.
    assert!(
        spans
            .iter()
            .any(|span| span.style.fg == Some(Color::Red)
                && span.content.contains("red stays red")),
        "{spans:?}"
    );
    // …and the plain rows carry the theme's ink.
    assert!(
        spans
            .iter()
            .any(|span| span.style.fg == Some(attention::text())
                && span.content.contains("plain agent text")),
        "{spans:?}"
    );
}
