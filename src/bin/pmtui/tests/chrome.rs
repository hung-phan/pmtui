//! The frame around the list: the header's need-you/stuck chips and the counts that
//! partition every row exactly once, the layout tiers that switch on width, the
//! full-screen wake view, and the renders that must not panic at any terminal size.

use super::*;

#[test]
fn header_shows_a_filled_need_you_chip_only_when_someone_needs_you() {
    // The dashboard was quieter than `notify-send`: `escalation::for_stops` already
    // fires "N decisions need you" while the bar's loudest pixels said ` pmtui `.
    let app = app_with(
        vec![
            view("asking", Posture::NeedsYou, vec![]),
            view("asking2", Posture::NeedsYou, vec![]),
        ],
        UiMode::Normal,
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(screen.contains("2 need you"), "no chip: {screen}");
    let chip = styles_under(&terminal, "2 need you").expect("chip not found");
    assert!(
        chip.iter()
            .all(|(fg, m)| *fg == agent_manager::theme::soft() && m.contains(Modifier::REVERSED)),
        "the chip is not a filled badge: {chip:?}"
    );

    // Zero is not news — the same rule the stuck count follows.
    let quiet = app_with(vec![view("busy", Posture::Working, vec![])], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &quiet)).expect("render");
    assert!(
        !screen_text(&terminal).contains("need you "),
        "a chip appeared with nobody needing anything"
    );
}

#[test]
fn the_header_chips_shape_follows_the_bucket_and_its_hue_follows_severity() {
    // A genuinely STUCK session gets the stuck bucket's shape, in red.
    let app = app_with(vec![view("wedged", Posture::Stuck, vec![])], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let chip = styles_under(&terminal, "1 need you").expect("chip not found");
    assert!(
        chip.iter()
            .all(|(fg, _)| *fg == agent_manager::theme::hard()),
        "{chip:?}"
    );
    assert!(screen_text(&terminal).contains("\u{2715} 1 need you"));

    // A needs-you row with a HARD STOP is red too — but it must keep the needs-you
    // SHAPE, because that is the glyph its own row draws. Choosing the stuck glyph
    // from the stop's severity reintroduced the header/row disagreement this
    // milestone exists to fix, and only a look at a real screen caught it.
    let asking = view(
        "asking",
        Posture::NeedsYou,
        vec![stop("s1", "publish", RiskClass::Low)],
    );
    let app = app_with(vec![asking], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("\u{25d0} 1 need you"),
        "the header borrowed another bucket's glyph: {screen}"
    );
    let chip = styles_under(&terminal, "1 need you").expect("chip not found");
    assert!(
        chip.iter()
            .all(|(fg, _)| *fg == agent_manager::theme::hard()),
        "a hard stop should still be red: {chip:?}"
    );
    // …and the row it summarises draws the same shape.
    assert!(project_row_text(&app.projects[0]).contains('\u{25d0}'));
}

#[test]
fn the_brand_chip_no_longer_sets_a_background() {
    // `fg(White).bg(Blue)` measured 1.06:1 on Catppuccin Mocha — the only background
    // in the binary, spent on a label that was invisible on most dark themes.
    let app = app_with(vec![view("x", Posture::Working, vec![])], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let buf = terminal.backend().buffer();
    let row0: Vec<_> = (buf.area.left()..buf.area.right())
        .filter_map(|x| buf.cell((x, buf.area.top())))
        .collect();
    let text: String = row0.iter().map(|c| c.symbol()).collect();
    let at = text.find("pmtui").expect("no brand chip");
    for c in &row0[at..at + 5] {
        // The chip adds no background OF ITS OWN: what is there is the theme's canvas, painted over
        // every cell by `render` before anything draws. The chip's fill is reverse video, which is the
        // point — a `.bg()` here would flip under the list's `highlight_style`.
        assert_eq!(
            c.bg,
            agent_manager::theme::base_bg(),
            "the brand chip set a background of its own"
        );
        assert!(
            c.modifier.contains(Modifier::REVERSED),
            "not a chip any more"
        );
    }
}

#[test]
fn stuck_has_its_own_glyph_not_just_its_own_colour() {
    // COLOUR-BLIND REDUNDANCY. `Stuck` and `NeedsYou` used to share `◐` and differ
    // only by hue, so on a terminal theme that flattens red and yellow — or for a
    // reader who cannot tell them apart — "the agent is wedged" and "the agent asked
    // you something" were the same row. The SHAPE has to differ.
    let stuck = view("wedged", Posture::Stuck, vec![]);
    let needs = view("asking", Posture::NeedsYou, vec![]);
    let (sg, sc) = status_glyph(&stuck);
    let (ng, nc) = status_glyph(&needs);
    assert_ne!(sg, ng, "stuck and needs-you must not share a glyph");
    assert_eq!((sg, sc), ("✕", agent_manager::theme::hard()));
    assert_eq!((ng, nc), ("◐", agent_manager::theme::soft()));
    assert!(project_row_text(&stuck).contains('✕'), "row glyph missing");
    // …and the two are now separate BUCKETS, so nothing counts one as the other.
    assert_ne!(status_category(&stuck), status_category(&needs));
}

#[test]
fn header_counts_and_row_glyphs_partition_every_row_exactly_once() {
    // THE HEADER/ROWS INVARIANT: one partition feeds both surfaces. Every row lands
    // in exactly one bucket (so the tallies sum to the row count), and every row
    // draws the glyph the header counts it under — which is precisely what a
    // `Stuck`-shaped bucket in one place and not the other would break.
    let live = view("live", Posture::Working, vec![]);
    let projects = vec![
        view("wedged", Posture::Stuck, vec![]),
        view("wedged2", Posture::Stuck, vec![]),
        view("asking", Posture::NeedsYou, vec![]),
        view("busy", Posture::Running, vec![]),
        view("done", Posture::Done, vec![]),
        live,
    ];
    let app = app_with(projects, UiMode::Normal);
    let (needs, running, idle, stuck) = counts(&app);
    assert_eq!(
        needs + running + idle + stuck,
        app.projects.len(),
        "a row fell into two buckets or none: {needs}/{running}/{idle}/{stuck}"
    );
    assert_eq!((needs, running, idle, stuck), (1, 2, 1, 2));
    for v in &app.projects {
        assert_eq!(
            status_glyph(v).0,
            category_glyph(status_category(v)).0,
            "row {} draws a glyph its bucket does not",
            v.id
        );
    }
    // And the header shows the tally, each with its bucket's glyph. There is NO separate
    // `need-you` tally any more — the loud CTA chip (`✕ 3 need you`, i.e. needs + stuck) is the
    // single need-you display; a dim `◐ 1 need-you` beside it was the redundant duplicate a user
    // flagged. So the CTA carries the count, `stuck` breaks out the wedged subset, and the tally
    // proper is running · stuck · idle.
    //
    // Read at 140 columns: the four top-right view/action controls claim ~50 columns of the right
    // edge, and this line is a clipped `Paragraph` whose documented sacrifice order is the socket
    // hint, then the tally, never the daemon chip. The whole tally therefore needs a wide terminal
    // once the fleet is this busy; what is under test here is the partition, not the clip boundary.
    let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    for want in ["✕ 3 need you", "● 2 running", "✕ 2 stuck", "○ 1 idle"] {
        assert!(screen.contains(want), "header missing {want:?}: {screen}");
    }
    assert!(
        !screen.contains("need-you"),
        "the redundant need-you tally survived: {screen}"
    );
}

#[test]
fn a_monitoring_autopilot_session_counts_as_running_not_idle() {
    // User: *"on the top, there is pmtui, 0 running, 1 idle. However, the claude session are
    // really running … i am not sure why you display idle."* A driven agent-loop session
    // parks `Monitoring` between nudges (it is NEVER `Running` — that variant is vestigial
    // ephemeral back-compat), so it used to fall into the idle bucket and the header read
    // `0 running, 1 idle` for a live autopilot session whose agent was really working. It is
    // LIVE now: counted running, drawing the green `●`.
    let mut driven = view("driven", Posture::Monitoring, vec![]);
    driven.mode = Mode::AgentLoop;
    driven.tier = Some(Tier::Autopilot);
    assert_eq!(
        status_category(&driven),
        1,
        "a driven Monitoring session is running/live, not idle"
    );
    assert_eq!(
        status_glyph(&driven).0,
        "●",
        "…and it draws the live glyph, not the idle ○"
    );
    let app = app_with(vec![driven], UiMode::Normal);
    assert_eq!(
        counts(&app),
        (0, 1, 0, 0),
        "one live autopilot session ⇒ 1 running, 0 idle"
    );

    // A created-but-never-run session (JobRun::Idle → Fresh) is, by contrast, genuinely idle.
    let mut fresh = view("new", Posture::Fresh, vec![]);
    fresh.mode = Mode::AgentLoop;
    fresh.tier = Some(Tier::Autopilot);
    assert_eq!(
        status_category(&fresh),
        2,
        "a created-but-never-run session is still idle"
    );
}

#[test]
fn a_driven_session_flips_to_idle_once_its_turn_ends_and_it_waits() {
    // User: *"after the claude finish and wait for, why don't we update the status"*.
    // A driven `Monitoring` session shows the live `●` WHILE its agent is mid-turn
    // (`agent_working == Some(true)`), then the distinct idle `○` once the turn ends and it
    // is waiting for the next nudge (`Some(false)`). An unknown sub-state (`None`, no hook)
    // keeps the m72 running default.
    let mk = |working| {
        let mut v = view("driven", Posture::Monitoring, vec![]);
        v.mode = Mode::AgentLoop;
        v.tier = Some(Tier::Autopilot);
        v.agent_working = working;
        v
    };

    let working = mk(Some(true));
    assert_eq!(status_category(&working), 1, "mid-turn ⇒ running/live");
    assert_eq!(status_glyph(&working).0, "●");

    let waiting = mk(Some(false));
    assert_eq!(
        status_category(&waiting),
        2,
        "turn finished, waiting for the next nudge ⇒ idle"
    );
    assert_eq!(
        status_glyph(&waiting).0,
        "○",
        "…and it draws the idle glyph"
    );

    let unknown = mk(None);
    assert_eq!(
        status_category(&unknown),
        1,
        "no turn-end hook ⇒ keep the m72 running default"
    );

    // The header agrees (needs, running, idle, stuck): working ⇒ 1 running, waiting ⇒ 1 idle.
    assert_eq!(
        counts(&app_with(vec![mk(Some(true))], UiMode::Normal)),
        (0, 1, 0, 0)
    );
    assert_eq!(
        counts(&app_with(vec![mk(Some(false))], UiMode::Normal)),
        (0, 0, 1, 0)
    );
}

#[test]
fn a_standard_row_buckets_on_live_repl_not_the_frozen_ledger() {
    // m74/m75. User: *"when i use standard, the status on the top bar doesn't really reflect
    // correctly."* pmd never drives a Standard agent-loop row (`daemon::pmd_drives_row(AgentLoop,
    // Standard) == false`), so its ledger posture is a FROZEN snapshot. The top bar must bucket a
    // Standard row on real session liveness (`session_live`, set by `refresh` from a live pmchat-/
    // pmloop- pane) like an Interactive row, NOT the posture — whatever the stale ledger reads.
    let mk = |posture, alive| {
        let mut v = view("std", posture, vec![]);
        v.mode = Mode::AgentLoop;
        v.tier = Some(Tier::Standard);
        v.session_live = alive;
        v
    };

    // A live (or detached-but-surviving) REPL ⇒ running (●), whatever the frozen posture says —
    // including a leftover `Monitoring`/`Running` from a former Autopilot stint.
    for posture in [
        Posture::Fresh,
        Posture::Monitoring,
        Posture::Running,
        Posture::Working,
    ] {
        let live = mk(posture, true);
        assert_eq!(
            status_category(&live),
            1,
            "a live Standard REPL ⇒ running ({posture:?})"
        );
        assert_eq!(status_glyph(&live).0, "●");
    }
    // Nothing up ⇒ idle (○), even if the frozen ledger still says Monitoring/Running.
    for posture in [
        Posture::Fresh,
        Posture::Monitoring,
        Posture::Running,
        Posture::Working,
    ] {
        let dead = mk(posture, false);
        assert_eq!(
            status_category(&dead),
            2,
            "no Standard REPL ⇒ idle ({posture:?})"
        );
        assert_eq!(status_glyph(&dead).0, "○");
    }
    // A blocked Standard row still wins the needs-you / stuck bucket even with a live REPL — the
    // autopilot-off honesty gate sits ABOVE the liveness split.
    let mut blocked = mk(Posture::NeedsYou, true);
    assert_eq!(
        status_category(&blocked),
        0,
        "a blocked Standard row needs you, live REPL or not"
    );
    blocked.posture = Posture::Stuck;
    assert_eq!(
        status_category(&blocked),
        3,
        "a stuck Standard row is stuck, live REPL or not"
    );

    // The header tally agrees (needs, running, idle, stuck).
    assert_eq!(
        counts(&app_with(vec![mk(Posture::Fresh, true)], UiMode::Normal)),
        (0, 1, 0, 0),
        "one live Standard session ⇒ 1 running"
    );
    assert_eq!(
        counts(&app_with(
            vec![mk(Posture::Monitoring, false)],
            UiMode::Normal
        )),
        (0, 0, 1, 0),
        "a stale-Monitoring Standard session with no live REPL ⇒ 1 idle, not running"
    );
}

#[test]
fn a_standard_row_reads_running_only_while_working_not_merely_alive() {
    // m76 — THE reported bug: an alive-but-idle claude read running (●). The Standard bucket is now
    // a claim about ACTIVITY, read from `agent_working` (set by `refresh` from pane classification),
    // NOT mere liveness: `●` = working, `○` = alive-but-idle-at-prompt or dead.
    let mk = |session_live, agent_working| {
        let mut v = view("std", Posture::Fresh, vec![]);
        v.mode = Mode::AgentLoop;
        v.tier = Some(Tier::Standard);
        v.session_live = session_live;
        v.agent_working = agent_working;
        v
    };

    // Live + actively working ⇒ running ●.
    assert_eq!(
        status_category(&mk(true, Some(true))),
        1,
        "working ⇒ running"
    );
    assert_eq!(status_glyph(&mk(true, Some(true))).0, "●");
    // Live + confirmed idle-at-prompt ⇒ idle ○. THIS is the bug the user reported.
    assert_eq!(
        status_category(&mk(true, Some(false))),
        2,
        "idle at its prompt ⇒ idle, not running"
    );
    assert_eq!(status_glyph(&mk(true, Some(false))).0, "○");
    // Live + not-yet-classified (None: first tick, or a capture error) ⇒ running ● — never a false idle.
    assert_eq!(
        status_category(&mk(true, None)),
        1,
        "a live row we can't classify yet stays running, not a false idle"
    );
    // Dead pane ⇒ idle ○, even if a stale working flag lingers.
    assert_eq!(
        status_category(&mk(false, Some(true))),
        2,
        "no live pane ⇒ idle"
    );
    assert_eq!(status_category(&mk(false, None)), 2);

    // The header tally agrees: working ⇒ 1 running, idle-at-prompt ⇒ 1 idle.
    assert_eq!(
        counts(&app_with(vec![mk(true, Some(true))], UiMode::Normal)),
        (0, 1, 0, 0)
    );
    assert_eq!(
        counts(&app_with(vec![mk(true, Some(false))], UiMode::Normal)),
        (0, 0, 1, 0)
    );
}

#[test]
fn header_hides_the_stuck_count_when_nothing_is_stuck() {
    // Zero is not news. Keeping that bucket absent leaves room for the daemon
    // state and the live running/idle counts on a constrained header.
    let app = app_with(vec![view("busy", Posture::Working, vec![])], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        !screen.contains("stuck"),
        "zero stuck is not news: {screen}"
    );
    assert!(
        screen.contains("pmd"),
        "the daemon chip keeps its room: {screen}"
    );
}

#[test]
fn renders_normal_and_overlay_without_panicking() {
    let stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    for mode in [
        UiMode::Normal,
        UiMode::Answering {
            input: "B".into(),
            choice: 0,
            scroll: 0,
        },
        UiMode::Creating(CreateForm::new()),
        UiMode::Confirming {
            id: "auth-rewrite".into(),
            session: "pmi-x".into(),
            what: Confirmable::Remove,
        },
        UiMode::EditingGoal {
            id: "auth-rewrite".into(),
            brief: PathBuf::from("/nonexistent/brief.md"),
            current: "ship it".into(),
            input: goal_buf("ship it faster"),
            then_autopilot: false,
        },
    ] {
        let app = app_with(
            vec![
                view("auth-rewrite", Posture::NeedsYou, stops.clone()),
                view("scratch", Posture::Done, vec![]),
            ],
            mode,
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).expect("render");
    }
}

#[test]
fn renders_loop_create_form_with_cadence_without_panicking() {
    // The form (Autonomy dial + Cadence row) must render for a loop session; the
    // cadence field is an editable value.
    let mut form = CreateForm::new();
    form.tier = Tier::Autopilot;
    form.goal = "watch the channel".into();
    form.field = 5; // focus the Cadence field
    let app = app_with(vec![], UiMode::Creating(form));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render loop form");
    let screen = screen_text(&terminal);
    assert!(screen.contains("Autonomy"), "autonomy dial shown");
    assert!(screen.contains("hands-off"), "autopilot descriptor shown");
    assert!(screen.contains("Cadence"), "cadence row shown");
}

#[test]
fn renders_empty_without_panicking() {
    let app = app_with(vec![], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render empty");
}

#[test]
fn renders_two_pane_layout_with_pane_titles() {
    let app = app_with(
        vec![
            view("auth-rewrite", Posture::Working, vec![]),
            view("scratch", Posture::Done, vec![]),
        ],
        UiMode::Normal,
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render two-pane");
    let screen = screen_text(&terminal);
    assert!(screen.contains("SESSIONS"), "SESSIONS pane title missing");
    // The preview pane is titled with the selected id.
    assert!(screen.contains("auth-rewrite"), "selected id not previewed");
}

#[test]
fn layout_tiers_switch_on_width() {
    // Wide: side-by-side. Medium: stacked. Narrow: list-only. Below the
    // minimum: one honest message.
    let dir = tempfile::tempdir().unwrap();
    let (app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "working", "seq": 1, "status": "still going" }"#),
    );
    let draw = |w: u16, h: u16| {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, &app)).expect("render tier");
        screen_text(&terminal)
    };

    // Wide (>= 80): two panes, preview holds the transcript's Log section.
    let wide = draw(100, 30);
    assert!(wide.contains("SESSIONS"), "wide: list missing");
    assert!(
        wide.contains(" preview "),
        "wide: Log section missing: {wide}"
    );
    assert!(
        wide.contains("All done here"),
        "wide: transcript missing: {wide}"
    );

    // Medium stacks the selected session's detail below the rail without an
    // extra view concept or Tab binding.
    let medium = draw(60, 20);
    assert!(medium.contains("SESSIONS"), "medium: list missing");
    assert!(
        medium.contains("All done here"),
        "medium: transcript missing: {medium}"
    );
    assert!(!medium.contains("Workspace"), "obsolete view tab: {medium}");

    // The wide split begins only when the fixed 44-column rail leaves at
    // least 52 columns for the selected session.
    let mut boundary = Terminal::new(TestBackend::new(WIDE_W - 1, 28)).unwrap();
    boundary.draw(|f| render(f, &app)).unwrap();
    let stacked = app.panes.get();
    assert!(stacked.detail.y > stacked.sessions.y, "{stacked:?}");
    let mut boundary = Terminal::new(TestBackend::new(WIDE_W, 28)).unwrap();
    boundary.draw(|f| render(f, &app)).unwrap();
    let split = app.panes.get();
    assert!(split.detail.x > split.sessions.x, "{split:?}");

    // Narrow keeps navigation usable by showing only the session rail.
    let narrow = draw(40, 20);
    assert!(narrow.contains("SESSIONS"), "narrow: list missing");
    assert!(
        !narrow.contains("All done here"),
        "narrow: transcript should be hidden: {narrow}"
    );

    // Below the minimum: the too-small message and nothing else.
    for (w, h) in [(20u16, 5u16), (23, 7), (30, 7)] {
        let tiny = draw(w, h);
        assert!(
            tiny.contains("Terminal too small"),
            "{w}x{h}: too-small message missing: {tiny}"
        );
        assert!(
            tiny.contains(&format!("min {MIN_W}x{MIN_H}")),
            "{w}x{h}: minimum not stated: {tiny}"
        );
        assert!(!tiny.contains("SESSIONS"), "{w}x{h}: list drawn anyway");
    }
}

/// THE BARS AND THE PANES SHARE A LEFT EDGE. The top bar was the only row on the dashboard that
/// began at column 0: its reversed ` pmtui ` chip hung over the `┌` of the pane beneath it, and
/// `pmtui` sat one column left of the pane title, the session rows and the keybar's first chip —
/// user: *"for the top bar pmtui, can you have padding left and right? i think it doesn't align
/// well with the panel bellow it"*.
///
/// Both views are checked, because they pad their panes differently (the session list by two
/// columns, a task card by one) and the bar is drawn by the same function for both. The RIGHT edge
/// is checked too: the padding is spent on both sides, so the top-right controls stop short of the
/// frame instead of running into it.
#[test]
fn the_top_bar_shares_its_left_edge_with_the_panes_below_it() {
    for (mode, view_name) in [(UiMode::Normal, "Sessions"), (UiMode::Board, "Tasks")] {
        let app = app_with(vec![view("bot", Posture::Working, vec![])], mode);
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rows: Vec<String> = screen_rows(&terminal)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        let bar = &rows[0];
        // COLUMNS, not byte offsets: a pane's `┌` is three bytes wide and one column wide, so
        // `str::find` alone would report the title two columns further right than the eye sees it.
        let col = |row: &str, needle: &str| {
            let byte = row
                .find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing from {row:?}"));
            row[..byte].chars().count()
        };
        // Where the TEXT starts on each row. The brand chip and the keybar's first chip both open
        // with a reversed SPACE, so their first non-blank character is the column the eye follows —
        // and it is the column a pane title occupies one cell past its corner.
        let brand = col(bar, "pmtui");
        // The leftmost pane's TITLE, whichever lane the Task view happens to lead with: the first
        // letter on the row, one cell past the frame's corner.
        let pane = rows[1]
            .chars()
            .position(char::is_alphanumeric)
            .unwrap_or_else(|| panic!("no pane title: {}", rows[1]));
        let keybar = rows[19]
            .chars()
            .position(|c| c != ' ')
            .unwrap_or_else(|| panic!("empty keybar: {}", rows[19]));
        assert_eq!(
            (brand, pane, keybar),
            (
                usize::from(BAR_PAD_X) + 1,
                usize::from(BAR_PAD_X) + 1,
                usize::from(BAR_PAD_X) + 1
            ),
            "{view_name}: the bar, the pane title and the keybar start in different columns:\n{bar}\n{}\n{}",
            rows[1],
            rows[19]
        );
        // The right side is padded as well: the last ink stops `BAR_PAD_X` short of the frame, and
        // the controls are there to prove the row actually reached that far.
        assert!(bar.contains("Switch"), "no controls to align: {bar}");
        assert_eq!(
            bar.trim_end().chars().count(),
            120 - usize::from(BAR_PAD_X),
            "{view_name}: the bar does not keep its right padding: {bar}"
        );
    }
}

/// A row with no columns to spare keeps all of them. Padding is a courtesy, and a bar that padded
/// itself out of existence would report nothing at all — so below the cost of the inset it is not
/// taken, on the rect and on the bare width alike (the two must agree, or the bar would draw at one
/// width while a caller reasoned about another).
#[test]
fn a_row_too_narrow_to_pad_keeps_every_column_it_has() {
    for width in [0, 1, 2 * BAR_PAD_X] {
        let area = Rect::new(3, 7, width, 1);
        assert_eq!(bar_inset(area), area, "width {width}: rect lost columns");
        assert_eq!(bar_width(width), width, "width {width}: width lost columns");
    }
    let roomy = Rect::new(3, 7, 40, 1);
    assert_eq!(
        bar_inset(roomy),
        Rect::new(3 + BAR_PAD_X, 7, 40 - 2 * BAR_PAD_X, 1),
        "a roomy row is inset on both sides"
    );
    assert_eq!(bar_width(40), 40 - 2 * BAR_PAD_X);
}

/// Where the view tabs live, and what they look like. A single row of identical chips
/// (`/ Switch · 1 Sessions · 2 Tasks · n New`) mixed WHERE YOU ARE with WHAT YOU CAN DO, so finding
/// your location meant reading the row — user: *"The indicator / Switch · 1 Sessions · 2 Tasks · n
/// New, is not a good UI design, you may need to have Sth like Tabs, so people don't need to
/// think"*. The two views are now TABS leading the header, and the active one is underlined, which
/// is pre-attentive and costs no extra row.
#[test]
fn the_view_tabs_lead_the_header_and_underline_the_active_view() {
    for on_board in [false, true] {
        let mode = || {
            if on_board {
                UiMode::Board
            } else {
                UiMode::Normal
            }
        };
        let (active, idle) = if on_board {
            ("2  Tasks", "1  Sessions")
        } else {
            ("1  Sessions", "2  Tasks")
        };
        let app = app_with(vec![view("bot", Posture::Working, vec![])], mode());
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let top = screen_rows(&terminal)[0].0.clone();

        // THE TABS LEAD: brand chip, then the tabs, then the state readout — not the action row.
        // The row opens with the bar's own padding (`BAR_PAD_X`), which is what lines its text up
        // with the pane titles below it.
        assert!(
            top.starts_with(&format!(
                "{} pmtui    1  Sessions   2  Tasks ",
                " ".repeat(usize::from(BAR_PAD_X))
            )),
            "the tabs do not lead the header: {top}"
        );
        assert!(
            top.find("1  Sessions") < top.find("pmd DOWN"),
            "the tabs must precede the state readout: {top}"
        );
        // The action row keeps only the ACTIONS.
        let actions = top.rfind('│').expect("actions rule");
        assert!(
            !top[actions..].contains("Sessions") && !top[actions..].contains("Tasks"),
            "a view is still a chip in the action row: {top}"
        );
        let new_label = if on_board { "+ Task" } else { "New" };
        assert!(
            top[actions..].contains("Switch") && top[actions..].contains(new_label),
            "{top}"
        );

        // ACTIVE = bold + cyan + UNDERLINED, on the label; inactive stays dim. Both key badges keep
        // the keybar's reversed-bold style, and neither badge is underlined while a label carries it.
        let on = styles_under_row(&terminal, "pmtui", active).expect(active);
        assert!(
            on[0].1.contains(Modifier::REVERSED | Modifier::BOLD)
                && !on[0].1.contains(Modifier::UNDERLINED),
            "{active} lost its plain key badge: {on:?}"
        );
        assert!(
            on[2..]
                .iter()
                .all(|(fg, m)| *fg == agent_manager::theme::accent()
                    && m.contains(Modifier::BOLD | Modifier::UNDERLINED)
                    && !m.contains(Modifier::DIM)),
            "{active} is not bold underlined cyan: {on:?}"
        );
        let off = styles_under_row(&terminal, "pmtui", idle).expect(idle);
        assert!(
            off[0].1.contains(Modifier::REVERSED | Modifier::BOLD),
            "{idle} lost its key badge: {off:?}"
        );
        assert!(
            off[2..]
                .iter()
                .all(|(_, m)| m.contains(Modifier::DIM) && !m.contains(Modifier::UNDERLINED)),
            "{idle} is not dim: {off:?}"
        );

        // A TAB NEVER MOVES UNDER THE POINTER: the tabs sit before the need-you chip, so their hit
        // regions are at the same columns whether or not the fleet is calling for a human.
        let hit = |app: &App, code: KeyCode| {
            app.top_hits
                .borrow()
                .iter()
                .find(|hit| hit.code == code)
                .map(|hit| hit.area)
                .unwrap_or_else(|| panic!("{code:?} publishes no hit region"))
        };
        let calm = (hit(&app, KeyCode::Char('1')), hit(&app, KeyCode::Char('2')));
        let needy = app_with(
            (0..12)
                .map(|i| view(&format!("ask{i}"), Posture::NeedsYou, vec![]))
                .collect(),
            mode(),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal.draw(|frame| render(frame, &needy)).unwrap();
        let loud = screen_rows(&terminal)[0].0.clone();
        assert!(loud.contains("need you"), "no CTA to move the tabs: {loud}");
        assert_eq!(
            calm,
            (
                hit(&needy, KeyCode::Char('1')),
                hit(&needy, KeyCode::Char('2'))
            ),
            "the need-you chip moved a tab: {loud}"
        );
        assert!(
            loud.starts_with(&format!(
                "{} pmtui    1  Sessions   2  Tasks ",
                " ".repeat(usize::from(BAR_PAD_X))
            )),
            "the CTA displaced the tabs: {loud}"
        );
    }
}

#[test]
fn the_view_numbers_switch_views_by_key_and_by_click_while_tab_still_toggles() {
    // `1`/`2` carry the affordance now, so they must SELECT a view — pressing the number of the
    // view you are already on is a no-op, not a toggle. Tab stays for muscle memory.
    let mut app = app_with(vec![view("bot", Posture::Working, vec![])], UiMode::Normal);
    for (code, want_board) in [
        (KeyCode::Char('1'), false), // already in Sessions
        (KeyCode::Char('2'), true),
        (KeyCode::Char('2'), true), // already in Tasks
        (KeyCode::Char('1'), false),
        (KeyCode::Tab, true),
        (KeyCode::Tab, false),
        (KeyCode::BackTab, true),
        (KeyCode::BackTab, false),
    ] {
        handle_key(&mut app, code, KeyModifiers::NONE);
        assert_eq!(
            matches!(app.mode, UiMode::Board),
            want_board,
            "{code:?} → {:?}",
            app.mode
        );
    }

    // `1` reaches the Session view from an OPEN card detail too, the one place the Task view's own
    // keys are otherwise inert.
    handle_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.board_detail_open);
    handle_key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    assert!(!app.board_detail_open);

    // The labels are clickable by construction: each control publishes a `KeyHit` carrying its own
    // KeyCode, and the click path routes through `handle_key`.
    let click = |app: &mut App, code: KeyCode| {
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let hit = app
            .top_hits
            .borrow()
            .iter()
            .find(|hit| hit.code == code)
            .map(|hit| hit.area)
            .unwrap_or_else(|| panic!("{code:?} is not a top control"));
        let mut handled = false;
        handle_event(
            app,
            Event::Mouse(ratatui::crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                // The label, not the badge: the whole control is one hit region.
                column: hit.x + hit.width - 1,
                row: hit.y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut handled,
        );
        assert!(handled, "{code:?} click was not routed");
    };
    click(&mut app, KeyCode::Char('2'));
    assert!(matches!(app.mode, UiMode::Board));
    click(&mut app, KeyCode::Char('2'));
    assert!(matches!(app.mode, UiMode::Board), "a click toggled instead");
    click(&mut app, KeyCode::Char('1'));
    assert!(matches!(app.mode, UiMode::Normal));
    click(&mut app, KeyCode::Char('1'));
    assert!(
        matches!(app.mode, UiMode::Normal),
        "a click toggled instead"
    );
}

/// The header's shed order under pressure. The tabs are LOCATION, so they survive longest: their
/// labels drop to badges (which keep underlining the active view) before the tally tail clips, and
/// `pmd DOWN` is never left as an ambiguous fragment beside anything that is drawn.
#[test]
fn the_header_sheds_tab_labels_before_the_tally_and_never_the_daemon_chip() {
    for on_board in [false, true] {
        for needy in [0usize, 12] {
            let mode = if on_board {
                UiMode::Board
            } else {
                UiMode::Normal
            };
            let app = app_with(
                (0..needy.max(1))
                    .map(|i| {
                        let posture = if needy == 0 {
                            Posture::Working
                        } else {
                            Posture::NeedsYou
                        };
                        view(&format!("s{i}"), posture, vec![])
                    })
                    .collect(),
                mode,
            );
            for width in [
                MIN_W, 28, 29, 30, 34, 35, 40, 50, 63, 71, 72, 73, 74, 80, 85, 86, 87, 100, 120,
            ] {
                let case = format!("board={on_board} needy={needy} at {width}");
                let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
                terminal.draw(|frame| render(frame, &app)).unwrap();
                let top = screen_rows(&terminal)[0].0.clone();
                let codes: Vec<KeyCode> =
                    app.top_hits.borrow().iter().map(|hit| hit.code).collect();
                let tabs: Vec<KeyCode> = codes
                    .iter()
                    .copied()
                    .filter(|code| {
                        matches!(
                            code,
                            KeyCode::Char('1') | KeyCode::Char('2') | KeyCode::Char('0')
                        )
                    })
                    .collect();

                // The tab tier is a function of WIDTH ALONE, so a tab cannot move sideways when the
                // fleet starts or stops calling for a human. The width that decides it is the BAR's
                // ([`bar_width`]), not the terminal's: the bar spends `BAR_PAD_X` on each side to
                // line up with the panes, and a tier measured on columns it does not have would
                // draw a label into the padding.
                assert_eq!(
                    top.contains("Sessions"),
                    bar_width(width) >= 84,
                    "{case}: wrong label tier: {top}"
                );
                assert_eq!(
                    tabs,
                    if bar_width(width) >= 33 {
                        vec![KeyCode::Char('1'), KeyCode::Char('2'), KeyCode::Char('0')]
                    } else {
                        Vec::new()
                    },
                    "{case}: wrong tab tier: {top}"
                );
                // Badge-only tabs still say which view you are in.
                if (33..84).contains(&bar_width(width)) {
                    let badge =
                        styles_under_row(&terminal, "pmtui", if on_board { " 2 " } else { " 1 " })
                            .unwrap_or_else(|| panic!("{case}: no active badge: {top}"));
                    assert!(
                        badge[1].1.contains(Modifier::UNDERLINED),
                        "{case}: the badge-only tier lost the location cue: {badge:?}"
                    );
                }
                // The action controls are two again, and they never clip the daemon chip.
                let actions: Vec<KeyCode> = codes
                    .into_iter()
                    .filter(|code| matches!(code, KeyCode::Char('/') | KeyCode::Char('n')))
                    .collect();
                assert!(
                    actions.is_empty() || actions == vec![KeyCode::Char('/'), KeyCode::Char('n')],
                    "{case}: partial actions {actions:?}"
                );
                if !actions.is_empty() || needy == 0 {
                    assert!(top.contains("pmd DOWN"), "{case}: {top}");
                }
            }
        }
    }
}

#[test]
fn narrow_dashboards_keep_new_switch_and_tasks_discoverable() {
    // Below the labelled width the two global actions stay on screen as clickable key badges
    // beside the view tabs and a whole daemon chip, the first-run list names `n`, and a
    // sparse keybar keeps its labels so `? Help` reads as help.
    let top_codes = |app: &App| {
        app.top_hits
            .borrow()
            .iter()
            .map(|hit| hit.code)
            .collect::<Vec<_>>()
    };
    let empty = app_with(Vec::new(), UiMode::Normal);
    for width in [KEYBAR_KEYS_W, 60, 71] {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|frame| render(frame, &empty)).unwrap();
        let rows = screen_rows(&terminal);
        let screen = screen_text(&terminal);
        assert!(rows[0].0.contains("pmd DOWN"), "{width}: {}", rows[0].0);
        assert_eq!(
            top_codes(&empty),
            vec![
                KeyCode::Char('1'),
                KeyCode::Char('2'),
                KeyCode::Char('0'),
                KeyCode::Char('/'),
                KeyCode::Char('n')
            ],
            "{width}: {}",
            rows[0].0
        );
        assert!(
            screen.contains("press n to create a session"),
            "{width}: {screen}"
        );
        assert!(
            rows.last().is_some_and(|(row, _)| row.contains("?  Help")),
            "{width}: {screen}"
        );
    }

    // A list-only terminal too narrow for the action badges still names the key, without clipping
    // it — and the view tabs, which are location rather than action, stay. 40 columns rather than the
    // old 30: the third tab raised the strip's floor ([`TAB_MIN_W`]), and below it the tabs yield too.
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    terminal.draw(|frame| render(frame, &empty)).unwrap();
    let screen = screen_text(&terminal);
    assert_eq!(
        top_codes(&empty),
        vec![KeyCode::Char('1'), KeyCode::Char('2'), KeyCode::Char('0')],
        "{screen}"
    );
    assert!(screen.contains("press n to create a session"), "{screen}");
    assert!(screen.contains("?  Help"), "{screen}");

    // Narrower than the strip's floor the tabs yield too, rather than leave `pmd DOWN` a fragment —
    // and the list keeps naming `n`, in its short form, because nothing else on screen now does.
    let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
    terminal.draw(|frame| render(frame, &empty)).unwrap();
    let screen = screen_text(&terminal);
    assert_eq!(top_codes(&empty), Vec::new(), "{screen}");
    assert!(screen.contains("pmd DOWN"), "{screen}");
    assert!(screen.contains("n: new session"), "{screen}");
    assert!(screen.contains("?  Help"), "{screen}");

    // The badges route through the same keys: a click on `/` opens the switcher.
    let mut dashboard = app_with(vec![view("bot", Posture::Working, vec![])], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal.draw(|frame| render(frame, &dashboard)).unwrap();
    let slash = dashboard
        .top_hits
        .borrow()
        .iter()
        .find(|hit| hit.code == KeyCode::Char('/'))
        .map(|hit| hit.area)
        .expect("Switch badge");
    let mut handled = false;
    handle_event(
        &mut dashboard,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: slash.x,
            row: slash.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(matches!(dashboard.mode, UiMode::Switching { .. }));

    // A loud need-you chip takes the room first: the action badges yield rather than clip the
    // daemon chip into an ambiguous fragment. The tabs stay — they are where you are.
    let needy = app_with(
        (0..12)
            .map(|i| view(&format!("ask{i}"), Posture::NeedsYou, vec![]))
            .collect(),
        UiMode::Normal,
    );
    for width in [KEYBAR_KEYS_W, 60, 71] {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|frame| render(frame, &needy)).unwrap();
        let top = &screen_rows(&terminal)[0].0;
        assert!(top.contains("pmd DOWN"), "{width}: {top}");
        let codes = top_codes(&needy);
        assert!(
            codes == vec![KeyCode::Char('1'), KeyCode::Char('2'), KeyCode::Char('0')]
                || codes
                    == vec![
                        KeyCode::Char('1'),
                        KeyCode::Char('2'),
                        KeyCode::Char('0'),
                        KeyCode::Char('/'),
                        KeyCode::Char('n')
                    ],
            "{width}: partial controls {codes:?}"
        );
    }
}

#[test]
fn help_stays_on_the_keybar_where_the_top_controls_yield() {
    // When a wide need-you chip leaves no room for the four top badges they yield together, and
    // the one surface still naming `n`, `/` and the view keys is Help, so its chip must survive
    // every width.
    let mut needy = app_with(
        (0..12)
            .map(|i| view(&format!("ask{i}"), Posture::NeedsYou, vec![]))
            .collect(),
        UiMode::Normal,
    );
    needy.status = "a long status message that competes with the keybar for its columns".into();
    for width in [MIN_W, 30, KEYBAR_KEYS_W, 60] {
        let bar = line_text(&keybar_line(&needy, width));
        assert!(bar.contains('?'), "{width}: {bar}");
    }
}

#[test]
fn the_task_view_keybar_keeps_its_way_back_where_the_top_controls_yield() {
    // The Task view binds no `?`, so when the top ACTION badges yield the lane keybar keeps `Esc`,
    // the way back to the Session view, whose keybar keeps Help. (The view tabs share `top_hits`
    // but are not the way back the keybar is standing in for: `Esc` tracks the actions, which is
    // what `top_controls_shown` reports.)
    for needs in [0, 12] {
        let posture = if needs == 0 {
            Posture::Working
        } else {
            Posture::NeedsYou
        };
        let mut board = app_with(
            (0..needs.max(1))
                .map(|i| view(&format!("ask{i}"), posture, vec![]))
                .collect(),
            UiMode::Board,
        );
        let mut yielded = false;
        for width in [MIN_W, 30, 40, KEYBAR_KEYS_W, 60, 71, 72, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal.draw(|frame| render(frame, &board)).unwrap();
            let rows = screen_rows(&terminal);
            let shown = board
                .top_hits
                .borrow()
                .iter()
                .any(|hit| hit.code == KeyCode::Char('n'));
            yielded |= !shown;
            let bar = &rows.last().unwrap().0;
            assert_eq!(
                bar.contains("Esc"),
                !shown,
                "{needs} needy at {width}: top={} bar={bar}",
                rows[0].0
            );
        }
        assert!(yielded, "{needs} needy: the badges never yielded");
        handle_key(&mut board, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(board.mode, UiMode::Normal));
    }
}

#[test]
fn renders_wake_view_full_screen_without_panicking() {
    // A WakeView over a session with a real stream-json log renders the readable
    // follow view full-screen (title + transcript tail + follow hint). A tiny 4x2
    // terminal must also lay out via the saturating height/index math.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot");
    std::fs::create_dir_all(sp.steps_dir()).unwrap();
    // A couple of hand-written stream-json event lines (chunk A fixture style).
    let log = concat!(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Working on the task"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false,"result":"All done here"}"#,
        "\n",
    );
    std::fs::write(sp.step_log(0), log).unwrap();

    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    app.mode = UiMode::WakeView {
        id: "bot".into(),
        paths: sp,
        scroll: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render wake view");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("Wake: bot"),
        "wake view title missing: {screen}"
    );
    assert!(
        screen.contains("All done here"),
        "transcript tail not shown: {screen}"
    );
    assert!(
        screen.contains("following"),
        "footer follow hint missing: {screen}"
    );

    // A 4x2 terminal must lay out without panicking (saturating math).
    let mut tiny = Terminal::new(TestBackend::new(4, 2)).unwrap();
    tiny.draw(|f| render(f, &app))
        .expect("render wake view tiny");
}

#[test]
fn renders_wake_view_no_wakes_placeholder_without_panicking() {
    // A WakeView over a session with no steps/ dir shows the `no wakes yet`
    // placeholder path — and must not panic.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let sp = ProjectPaths::for_session(&root, "bot"); // no steps/ created

    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    app.mode = UiMode::WakeView {
        id: "bot".into(),
        paths: sp,
        scroll: 0,
    };

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render wake view no wakes");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("no wakes yet"),
        "no-wakes placeholder missing: {screen}"
    );
}

#[test]
fn wake_view_key_handling_scrolls_and_exits() {
    // Scroll math is saturating and Esc/q return to Normal.
    let paths = ProjectPaths::new("/nonexistent");
    let mut app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::WakeView {
            id: "bot".into(),
            paths: paths.clone(),
            scroll: 0,
        },
    );
    // Up scrolls one line up from the tail.
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 1, .. }));
    // PageUp jumps ten more.
    handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 11, .. }));
    // End snaps back to following the tail.
    handle_key(&mut app, KeyCode::End, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 0, .. }));
    // Down at the tail saturates at 0 (no underflow).
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 0, .. }));
    // Esc returns to Normal.
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
    // Char('q') also returns to Normal from WakeView.
    app.mode = UiMode::WakeView {
        id: "bot".into(),
        paths,
        scroll: 0,
    };
    handle_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn wake_view_up_scrolls_clamp_to_last_rendered_max() {
    // Once a frame has rendered, `scroll_max` bounds the UP scrolls so
    // over-scroll can't run `scroll` unbounded (which would freeze Down/PgDn).
    let paths = ProjectPaths::new("/nonexistent");
    let mut app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::WakeView {
            id: "bot".into(),
            paths,
            scroll: 0,
        },
    );
    app.scroll_max.set(5);
    // PageUp jumps +10 but clamps to the max (5).
    handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 5, .. }));
    // PageUp again stays pinned at the max.
    handle_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 5, .. }));
    // Up at the max stays at the max (5 + 1 clamped back to 5).
    handle_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 5, .. }));
    // Down reduces scroll by one (no clamp needed).
    handle_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 4, .. }));
    // End snaps back to following the tail.
    handle_key(&mut app, KeyCode::End, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::WakeView { scroll: 0, .. }));
}

#[test]
fn renders_tiny_terminal_without_panicking() {
    // Every size across the responsive tiers must lay out (Min(0) body,
    // saturating widths/heights) without panicking, in both the empty and
    // populated cases: 1x1 and 20x5 hit the too-small path, 24x8/30x10 the
    // narrow list-only tier, 60x20 the stacked tier, 100x30 and 200x50 the wide
    // two-pane tier.
    let sizes = [
        (1u16, 1u16),
        (20, 5),
        (24, 8),
        (30, 10),
        (60, 20),
        (100, 30),
        (200, 50),
    ];
    let cases = [
        vec![],
        vec![view(
            "x",
            Posture::NeedsYou,
            vec![stop("s1", "publish", RiskClass::Hard)],
        )],
    ];
    for projects in cases {
        let app = app_with(projects, UiMode::Normal);
        for (w, h) in sizes {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| render(f, &app))
                .unwrap_or_else(|e| panic!("render {w}x{h}: {e}"));
        }
    }
    // The create overlay (Autonomy dial + Cadence row) must also lay out on a
    // tiny 20x5 area without panicking — centered_rect clamps to the area.
    let form = CreateForm::new();
    let app = app_with(vec![], UiMode::Creating(form));
    let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render tiny create overlay");
}

#[test]
fn renders_the_new_indicators_at_every_size_without_panicking() {
    // Same size sweep with THIS slice's additions live: the `pmd …` chip on the
    // status bar (a real, cached lock probe — the registry here exists, so it runs)
    // and a chatting agent-loop row's chip + park line. Both are plain spans on
    // clipped, non-wrapping widgets, and 1x1 must still be a no-panic frame.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut chatting = agent_loop_view("bot");
    chatting.human_attached = true;
    for daemon_up in [false, true] {
        // `Some` only in the up case, so the flock is held across those renders.
        let held = daemon_up.then(|| {
            lease::try_acquire(&lease::daemon_lock_path(&reg_path, "pm-test"))
                .unwrap()
                .expect("the daemon lock is free to pre-acquire")
        });
        let mut app = app_with(vec![chatting.clone()], UiMode::Normal);
        app.registry_path = reg_path.clone();
        for (w, h) in [
            (1u16, 1u16),
            (20, 5),
            (24, 8),
            (30, 10),
            (49, 12),
            (60, 20),
            (79, 24),
            (100, 30),
            (200, 50),
        ] {
            app.restart_daemon_watch(); // probe on every size, not once
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| render(f, &app))
                .unwrap_or_else(|e| panic!("render {w}x{h} (daemon_up={daemon_up}): {e}"));
        }
        drop(held);
    }
}
