//! The preview pane's report and transcript: the marker report, the live agent pane
//! mirrored into the buffer with the agent's own colours, and the ladder of fallbacks
//! when no pane is live, a capture fails, or the pane holds nothing but escapes.

use super::*;

#[test]
fn every_autopilot_event_kind_uses_the_shared_decision_tone() {
    use job::{AutopilotEventKind::*, HoldReason};

    for (kind, expected) in [
        (Nudged, agent_manager::theme::brand()),
        (Held(HoldReason::Busy), agent_manager::theme::rule()),
        (Reported(None), agent_manager::theme::accent()),
        (AutoAnswered(None), agent_manager::theme::accent_alt()),
        (SupervisorResolved(None), agent_manager::theme::accent_alt()),
        (Answered(None), agent_manager::theme::accent_alt()),
        (
            CadenceChanged("5m -> 1m".into()),
            agent_manager::theme::soft(),
        ),
        (Escalated(None), agent_manager::theme::soft()),
        (Stuck(None), agent_manager::theme::hard()),
        (Launched, agent_manager::theme::live()),
    ] {
        assert_eq!(autopilot_tone(&kind), expected, "{kind:?}");
    }
}

/// The tail-aligned window, with no scroll — the shape the pane shows until the human scrolls back.
///
/// A wrapper in the render module until every production caller started passing a scroll offset, at
/// which point it was a function kept alive only by the tests that called it. It lives here now.
fn pane_tail(capture: &str, want: usize) -> Vec<Line<'_>> {
    pane_window(capture, want, 0).0
}

#[test]
fn latest_step_seq_picks_max_from_logs_and_results() {
    let dir = tempfile::tempdir().unwrap();
    // `<n>.log` files: the max seq wins.
    for n in [0u64, 1, 2] {
        std::fs::write(dir.path().join(format!("{n}.log")), "x").unwrap();
    }
    assert_eq!(latest_step_seq(dir.path()), Some(2));
    // `<n>.result.json` files count too and can bump the max.
    std::fs::write(dir.path().join("3.result.json"), "{}").unwrap();
    assert_eq!(latest_step_seq(dir.path()), Some(3));
}

#[test]
fn latest_step_seq_none_for_empty_or_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(latest_step_seq(dir.path()), None, "empty dir");
    assert_eq!(
        latest_step_seq(&dir.path().join("does-not-exist")),
        None,
        "missing dir"
    );
}

#[test]
fn preview_shows_marker_report_and_log_tail() {
    // The PREVIEW is dominated by the agent's own output: the marker report
    // (state + status) and the tail of the wake transcript. The
    // report comes from `needs-you.json` — the ONLY report file the persistent
    // agent loop writes (`steps/<seq>.result.json` is never written, so reading
    // it left this line stuck on "no wakes yet").
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(
            r#"{ "state": "monitoring", "seq": 7, "next_check_s": 900, "status": "watching the channel" }"#,
        ),
    );
    // A CONSISTENT monitoring row: the marker says `monitoring`, so the ledger/posture would too.
    // (The head badge shows the state from the posture; the report line no longer surfaces a marker
    // state that DIVERGES from a calm posture — that path is only for a real harness escalation.)
    app.projects[0].posture = Posture::Monitoring;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render preview");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("bot · Wake #0 · open"),
        "preview attach title lost its source or open action: {screen}"
    );
    assert!(screen.contains("bot"), "selected id not in preview header");
    assert!(
        screen.contains("monitoring"),
        "marker report state not shown: {screen}"
    );
    assert!(
        screen.contains("watching the channel"),
        "marker status note not shown: {screen}"
    );
    assert!(
        screen.contains(" preview "),
        "log section missing: {screen}"
    );
    assert!(
        screen.contains("All done here"),
        "transcript tail not shown: {screen}"
    );
}

/// A JOB'S PANEL IS PROSE, NOT ITS WIRE FORMAT. A job runs `claude -p --output-format stream-json`, so
/// capturing its pane shows a wall of events — user: *"it use the text in raw transcript format and it is
/// hard to view"*. The preview renders the tee'd `job.log` instead, and says so in its title.
#[test]
fn a_jobs_preview_renders_its_stream_instead_of_showing_raw_json() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "kid", Tier::Standard);
    let paths = ProjectPaths::for_session(&root, "kid");
    std::fs::create_dir_all(paths.steps_dir()).unwrap();
    std::fs::write(
        paths.job_log(),
        concat!(
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Renamed the flag."}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","result":"the flag is renamed"}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut view = agent_loop_view("kid");
    view.job = true;
    let mut app = app_with(vec![view], UiMode::Normal);
    app.registry_path = reg_path;

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render preview");
    let screen = screen_text(&terminal);

    assert!(
        screen.contains("Renamed the flag."),
        "the agent's own prose is not shown: {screen}"
    );
    assert!(
        screen.contains("the flag is renamed"),
        "the final result line is not shown: {screen}"
    );
    assert!(
        !screen.contains(r#""type""#),
        "the raw event stream leaked into the panel: {screen}"
    );
    assert!(
        screen.contains("kid \u{b7} job"),
        "the title does not name the source: {screen}"
    );
}

/// A job with nothing in its log yet says so in its own words — neither "press m" (a job has no dial)
/// nor silence.
#[test]
fn a_job_with_an_empty_log_says_so_without_naming_autopilot() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "kid", Tier::Standard);
    let paths = ProjectPaths::for_session(&root, "kid");
    std::fs::create_dir_all(paths.steps_dir()).unwrap();
    let mut view = agent_loop_view("kid");
    view.job = true;
    let mut app = app_with(vec![view], UiMode::Normal);
    app.registry_path = reg_path;

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render preview");
    let screen = screen_text(&terminal);

    assert!(
        screen.contains("the job is over") || screen.contains("job is running"),
        "a job's empty log must explain itself: {screen}"
    );
    assert!(
        !screen.contains("m switches autopilot"),
        "a job has no autopilot dial to point at: {screen}"
    );
}

#[test]
fn preview_falls_back_to_ledger_last_status() {
    // A marker with no `status` still leaves the agent's last human-facing line
    // in the ledger (`last_status`, written by the harness on disposal) — the
    // preview surfaces it rather than dropping it on the floor.
    let dir = tempfile::tempdir().unwrap();
    let (app, sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "working", "seq": 3 }"#), // no status note
    );
    // Fixture-only ledger write (pmtui itself never writes state.json).
    let mut ledger = job::load(&sp).unwrap().unwrap();
    ledger.last_status = Some("indexed 412 files".into());
    job::save(&sp, &ledger).unwrap();

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render preview last_status");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("indexed 412 files"),
        "ledger last_status not surfaced: {screen}"
    );
    assert!(
        screen.contains("working"),
        "marker report state not shown: {screen}"
    );
}

#[test]
fn preview_report_is_empty_without_a_marker_no_placeholder() {
    // No `needs-you.json` and no ledger status → the report section shows NOTHING (no "no report yet",
    // no bare "status —" chip, not even the ` report ` rule). A brand-new session (esp. a Standard one
    // that has never run) has nothing to report, and the placeholder was clutter — user: *"when i
    // create a new standard session i see 'no report yet'. can you remove that"*. Must not panic.
    let dir = tempfile::tempdir().unwrap();
    let (app, _sp) = app_with_wake_fixture(dir.path(), None);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render preview no marker");
    let screen = screen_text(&terminal);
    assert!(
        !screen.contains("no report yet"),
        "the missing-report placeholder must be gone: {screen}"
    );
    assert!(
        !screen.contains(" report "),
        "an empty report section must not draw its rule either: {screen}"
    );
    // The transcript tail is independent of the marker and still renders.
    assert!(
        screen.contains("All done here"),
        "transcript tail not shown: {screen}"
    );

    // The user's exact case is a STANDARD (undriven) session: it must ALSO show an empty report
    // section — no "no report yet", and NOT the undriven "autopilot off — nothing is scheduled"
    // line either (that only heads a session that HAS a marker).
    let mut app = app;
    app.projects[0].tier = Some(Tier::Standard);
    terminal
        .draw(|f| render(f, &app))
        .expect("render standard, no marker");
    let screen = screen_text(&terminal);
    // "nothing is scheduled" is the report LINE (the head meta separately shows an `autopilot off`
    // field, which is fine); its absence proves the report body is empty, not just the placeholder.
    for absent in ["no report yet", " report ", "nothing is scheduled"] {
        assert!(
            !screen.contains(absent),
            "a brand-new Standard session's report section must be empty; saw {absent:?}: {screen}"
        );
    }
    assert!(
        screen.contains("All done here"),
        "transcript tail not shown for the standard case: {screen}"
    );
}

#[test]
fn preview_header_prioritizes_health_ownership_current_and_next() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "monitoring", "seq": 2, "status": "watching the channel" }"#),
    );
    let mut ledger = job::load(&sp).unwrap().unwrap();
    ledger.conversation_id = Some("conversation-123".into());
    job::save(&sp, &ledger).unwrap();

    app.projects[0].posture = Posture::Monitoring;
    app.projects[0].engine = Some(Engine::Codex);
    app.projects[0].step_id = 4;
    app.projects[0].next_action = "monitoring · check in 00:05:00".into();
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    let rows = screen_rows_styled(&terminal);

    let health_row = rows
        .iter()
        .position(|(row, _)| {
            row.contains("monitoring")
                && row.contains("control pmd")
                && row.contains("codex")
                && row.contains("autopilot")
        })
        .unwrap_or_else(|| panic!("health/ownership row is missing: {screen}"));
    let current_row = rows
        .iter()
        .position(|(row, _)| row.contains("current") && row.contains("watching the channel"))
        .unwrap_or_else(|| panic!("current row is missing: {screen}"));
    let next_row = rows
        .iter()
        .position(|(row, _)| row.contains("next") && row.contains("00:05:00"))
        .unwrap_or_else(|| panic!("next row is missing: {screen}"));
    assert!(
        health_row < current_row && current_row < next_row,
        "header hierarchy must be health/ownership, current, next: {screen}"
    );
    for clutter in ["step #", "conv ", " report "] {
        assert!(
            !screen.contains(clutter),
            "diagnostic/decorative header copy survived ({clutter:?}): {screen}"
        );
    }
    assert!(
        screen.contains("bot · Wake #0 · open"),
        "the attach title must remain intact: {screen}"
    );
    assert_ne!(
        app.preview_attach_hit.get(),
        Rect::ZERO,
        "the preview title must remain clickable"
    );
}

#[test]
fn a_paused_row_names_its_resume_key_instead_of_a_frozen_next_step() {
    // Nothing runs a paused session until Enter resumes it, so neither the ledger's frozen
    // schedule nor "you drive it" is the next thing that happens. Pause normally lands on
    // Standard, but a failed config write can leave the tier on Autopilot.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_wake_fixture(dir.path(), None);
    for tier in [Tier::Standard, Tier::Autopilot] {
        app.projects[0].enabled = false;
        app.projects[0].tier = Some(tier);
        app.projects[0].posture = Posture::Monitoring;
        app.projects[0].next_action = "monitoring · check in 42s".into();
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|f| render(f, &app)).expect("render");
        let screen = screen_text(&terminal);
        assert!(
            screen_rows(&terminal)
                .iter()
                .any(|(row, _)| row.contains("next") && row.contains("Enter resumes")),
            "{tier:?}: {screen}"
        );
        for stale in ["check in 42s", "you drive it"] {
            assert!(!screen.contains(stale), "{tier:?} kept {stale:?}: {screen}");
        }
    }
}

#[test]
fn preview_blocked_row_states_it_once_in_plain_language() {
    // The user's report: a blocked/needs-you row said "needs you" (badge) AND
    // "next: blocked \u{b7} confirm_done" (meta) AND "blocked   next-check: \u{2014}" (report) —
    // three overlapping ways to say one thing. The redesign: ONLY the head badge names the state,
    // and the report section carries the agent's actual words (`status`) — no repeated state word,
    // no dead timer, and no "waiting for you to answer" line that the badge + Stops already imply.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 3, "status": "ship it?" }"#),
    );
    app.projects[0].posture = Posture::NeedsYou;
    app.projects[0].tier = Some(Tier::Autopilot);
    app.projects[0].stops = vec![stop("s1", "confirm_done", RiskClass::Hard)];
    app.projects[0].oldest_stop_since = Some(SystemClock.now() - 60);
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render blocked");
    let screen = screen_text(&terminal);
    assert!(screen.contains("needs you"), "badge missing: {screen}");
    assert!(
        !screen.contains("next: blocked"),
        "the redundant raw `next: blocked \u{b7} confirm_done` survived: {screen}"
    );
    // The state is said ONCE, by the badge. The report line must not repeat it as `blocked`, and
    // must not add a `waiting for you to answer` line either — user: *"I still see 'waiting for you
    // to answer'. Why do we render that"*. Both are already implied by the badge + the Stops block.
    assert!(
        !screen.contains("blocked"),
        "the report line still repeats `blocked`, which the `needs you` badge already says: {screen}"
    );
    assert!(
        !screen.contains("waiting for you"),
        "the redundant `waiting for you to answer` report line survived: {screen}"
    );
    // The agent's own words still render as the current fact.
    assert!(
        screen.contains("ship it?"),
        "the agent's status note must still render as current: {screen}"
    );
    assert!(
        screen.contains("press s to answer"),
        "the Stops rule must point at `a`: {screen}"
    );
    assert!(
        screen_rows_styled(&terminal)
            .iter()
            .any(|(line, _)| line.contains("current") && line.contains("ship it?")),
        "the current fact lost its compact label: {screen}"
    );
    assert!(
        !screen.contains(" report "),
        "the decorative report divider must be gone: {screen}"
    );

    // A short pane keeps the same current fact without spending a row on a divider.
    let mut small = Terminal::new(TestBackend::new(100, 14)).unwrap();
    small.draw(|f| render(f, &app)).expect("render small");
    let screen_small = screen_text(&small);
    assert!(
        !screen_small.contains(" report "),
        "the report rule must not return on a short pane: {screen_small}"
    );
    assert!(
        screen_small.contains("ship it?"),
        "the report content must still render without its rule: {screen_small}"
    );
}

#[test]
fn preview_head_folds_the_status_glyph_into_the_filled_badge() {
    // On a filled (needs-you / stuck) row the status GLYPH used to render as its own span in front
    // of the reverse-video badge, so the icon floated OUTSIDE the background colour (user: *"when it
    // displays needs you … the icon renders separately and not in background color"*). The glyph now
    // lives INSIDE the fill: `◐ needs you` is one reverse-video chip.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 1, "status": "ship it?" }"#),
    );
    app.projects[0].posture = Posture::NeedsYou;
    app.projects[0].tier = Some(Tier::Autopilot);
    app.projects[0].stops = vec![stop("s1", "confirm_done", RiskClass::Hard)];
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render blocked head");
    // `◐ needs you` is contiguous ONLY in the preview head — the status bar reads `◐ 1 need you`
    // and the SESSIONS row puts the id between the glyph and the label — so this targets the head.
    let chip = styles_under(&terminal, "\u{25d0} needs you").expect("the filled head chip");
    assert!(
        chip.iter().all(|(_, m)| m.contains(Modifier::REVERSED)),
        "the status glyph must sit inside the badge's background fill, not float before it: {chip:?}"
    );
}

#[test]
fn preview_working_row_does_not_show_a_stale_blocked_state() {
    // A live wake is running (ledger `run` = Running → posture Running), but `needs-you.json` still
    // holds the PREVIOUS wake's `blocked` report until this wake writes a fresh one. The report
    // line's divergence badge painted that stale "blocked" ON TOP OF the agent's status note — user:
    // *"when the session is working, i see the status blocked on top of my status"*. The state word
    // is surfaced ONLY when the HARNESS escalated to an attention posture (needs-you / stuck) past
    // the agent's calmer word; a working posture is never an escalation, so the stale marker drops.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "blocked", "seq": 5, "status": "building the index" }"#),
    );
    app.projects[0].posture = Posture::Running; // a wake is actively running
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render working");
    let screen = screen_text(&terminal);
    assert!(
        !screen.contains("blocked"),
        "a stale `blocked` marker must not show over a working row: {screen}"
    );
    // The agent's own status note still renders — that IS the report.
    assert!(
        screen.contains("building the index"),
        "the status note must still render: {screen}"
    );
}

#[test]
fn preview_shows_the_agents_word_when_the_harness_escalated_past_it() {
    // The divergence badge is KEPT for the case it exists for: the harness escalated to an attention
    // posture (here `stuck`, via the stall backstop) while the agent's last marker still said
    // `monitoring`. Both are worth seeing — the head badge shows the harness's `stuck`, the report
    // line surfaces the agent's own `monitoring`.
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "monitoring", "seq": 6, "status": "polling CI" }"#),
    );
    app.projects[0].posture = Posture::Stuck;
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render escalated");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("stuck"),
        "the head badge must show the harness posture: {screen}"
    );
    assert!(
        screen.contains("monitoring"),
        "the report line must surface the agent's own word on a real escalation: {screen}"
    );
}

#[test]
fn preview_monitoring_report_does_not_repeat_the_state_badge() {
    // A REAL monitoring row: the posture is derived from the marker, so both are `monitoring`.
    // The head badge says it; the current line must not repeat it (user: *"why do i see 2
    // monitoring"*).
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _sp) = app_with_wake_fixture(
        dir.path(),
        Some(
            r#"{ "state": "monitoring", "seq": 4, "next_check_s": 900, "status": "polling for a reaction" }"#,
        ),
    );
    // What `view::agent_loop::agent_loop_posture` produces for a monitoring ledger.
    app.projects[0].posture = Posture::Monitoring;
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render monitoring");
    let screen = screen_text(&terminal);
    // The selected header names `monitoring` once; `current` carries only the agent's words.
    let n = screen.matches("monitoring").count();
    assert_eq!(
        n, 1,
        "expected one selected-header `monitoring`, not another copy in current; saw {n}:\n{screen}"
    );
    // `current` is a quiet hierarchy label, not another filled status badge. The semantic fill is
    // reserved for the actual attention badge above it, and the note remains plain foreground.
    let label = styles_under(&terminal, "current").expect("current label on screen");
    assert!(
        label
            .iter()
            .all(|(_, m)| m.contains(Modifier::DIM) && !m.contains(Modifier::REVERSED)),
        "the `current` label should stay quiet and unfilled: {label:?}"
    );
    let note = styles_under(&terminal, "polling for a reaction").expect("status note on screen");
    assert!(
        note.iter()
            .all(|(fg, m)| *fg == attention::text() && !m.contains(Modifier::BOLD)),
        "the status note text must stay plain — the theme's ordinary ink, no bold: {note:?}"
    );
}

#[test]
fn preview_log_mirrors_the_live_agent_pane() {
    // THE FIX: for an agent-loop session the agent's output lives on the tmux
    // pane of its ONE persistent `claude`, not in `steps/<seq>.log` (nothing
    // writes that on this path). So a live `pmloop-…` session's pane text IS the
    // Log section — and it WINS over a stale legacy transcript on disk.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let driver = FakePane::live(&session, PANE_FIXTURE);
    let app = pane_app(&reg_path, driver.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render live pane");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("busy_since never resets"),
        "live pane text not shown: {screen}"
    );
    assert!(
        screen.contains("auto mode on"),
        "pane footer (the 'is it working' signal) not shown: {screen}"
    );
    assert!(
        screen.contains("· live"),
        "title does not name the live-pane source: {screen}"
    );
    // The stale `steps/0.log` transcript must NOT be what we are showing.
    assert!(
        !screen.contains("All done here"),
        "stale step transcript beat the live pane: {screen}"
    );
    // Exactly ONE capture, against the selected row's `pmloop-…` session (never a capture per list
    // row), and DEEP ENOUGH TO SCROLL: the visible region plus `PREVIEW_SCROLLBACK` of history.
    //
    // This assertion used to read `(1..=30)` — "sized to the pane" — and that bound WAS the defect
    // the user reported as *"[the mouse scroll is] … not scroll all the ways"*. `pane_window` clamps
    // against the lines it is handed, so a pane-sized capture is a one-screen scroll ceiling wearing
    // a scroll bug's clothes. Still BOUNDED, because an unlimited `-S -` would hand the render path
    // megabytes on every frame — so the range moved rather than disappeared.
    let captures = driver.captures();
    assert_eq!(captures.len(), 1, "expected one capture: {captures:?}");
    assert_eq!(captures[0].0, session, "captured the wrong session");
    assert!(
        (PREVIEW_SCROLLBACK + 1..=PREVIEW_SCROLLBACK + 30).contains(&captures[0].1),
        "capture depth must be the pane PLUS PREVIEW_SCROLLBACK of history: {captures:?}"
    );
}

#[test]
fn preview_fits_the_agent_pane_to_the_panel_and_dedupes_across_frames() {
    // THE FEATURE: the dashboard reflows the DETACHED agent pane to the live preview width, so
    // the transcript fills its panel and follows a terminal resize (user: *"when i resize, can
    // it resize too"*). Proven at the render seam — a live agent-loop row resizes its `pmloop-`
    // session to the preview's inner width — and, the load-bearing half, only ONCE across two
    // identical frames. That dedupe is the whole "is it expensive?" answer: a steady dashboard
    // forks tmux zero extra times.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let driver = FakePane::live(&session, PANE_FIXTURE);
    let app = pane_app(&reg_path, driver.clone());

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render 1");
    // A second IDENTICAL frame must not resize again — same size, so the dedupe suppresses it.
    terminal.draw(|f| render(f, &app)).expect("render 2");

    // The pane's content width is the bordered block's inner width (`area.width - 2`). The preview
    // carries NO block-level padding (it is a content pane — see `render_preview`), just the border.
    let inner_w = app.panes.get().detail.width.saturating_sub(2);
    let loop_resizes = |d: &FakePane| -> Vec<(String, u16, u16)> {
        d.resizes()
            .into_iter()
            .filter(|(s, ..)| s == &session)
            .collect()
    };
    let after_two = loop_resizes(&driver);
    assert_eq!(
        after_two.len(),
        1,
        "the pane must be fit exactly ONCE across two identical frames (deduped): {:?}",
        driver.resizes()
    );
    let (_, cols, rows) = &after_two[0];
    assert_eq!(
        *cols, inner_w,
        "the pane must be fit to the preview's inner width ({inner_w}), got {cols}"
    );
    // The pane is fit above the preview viewport so an alternate-screen TUI has rows above the
    // visible tail. A fixed 50-row pane becomes unscrollable on a tall dashboard.
    let preview_inner_h = app.panes.get().detail.height.saturating_sub(2);
    let expected_rows = preview_pane_fit_rows(preview_inner_h, 0);
    assert_eq!(
        *rows, expected_rows,
        "the pane height must include viewport headroom: {after_two:?}"
    );
    assert!(
        *rows > preview_inner_h,
        "the fitted pane must be taller than the preview ({preview_inner_h}), got {rows}"
    );
    assert!(
        inner_w >= 40,
        "sanity: a 120-col terminal leaves a preview wider than the 40-col fit floor (was {inner_w})"
    );
    // A first fit must NOT drop scrollback. `clear-history` wipes the very scrollback the preview
    // scrolls through (`capture-pane -S`), so clearing on every fit left a freshly-viewed row
    // unscrollable — user: *"i cannot scroll in main panel or after enter the session"*. The fit
    // never clears; see `preview_fit_never_clears_the_scrollback_it_scrolls`.
    let loop_clears: Vec<_> = driver
        .clears()
        .into_iter()
        .filter(|s| s == &session)
        .collect();
    assert!(
        loop_clears.is_empty(),
        "a first fit must NOT clear scrollback (that killed the scroll): {:?}",
        driver.clears()
    );

    // After an attach, the pane was resized to the real terminal (window-size latest), so the
    // run loop calls `forget_agent_pane_fit`; the very next frame must re-fit it to the preview
    // rather than leaving it oversized. Model that: forget, draw, and a fresh resize appears.
    app.forget_agent_pane_fit();
    terminal.draw(|f| render(f, &app)).expect("render 3");
    assert_eq!(
        loop_resizes(&driver).len(),
        2,
        "forgetting the fit must make the next frame re-apply it: {:?}",
        driver.resizes()
    );
    // …and the post-attach re-fit is a fit at the SAME size, so it must NOT clear either — that
    // is exactly the "after enter the session" half of the scroll regression.
    assert!(
        driver.clears().iter().all(|s| s != &session),
        "the post-attach re-fit must not clear scrollback: {:?}",
        driver.clears()
    );
}

#[test]
fn preview_fit_never_clears_the_scrollback_it_scrolls() {
    // The fit must NEVER `clear_history` — not on a first fit, a row switch, a post-attach re-fit, OR
    // a genuine resize. `clear-history` wipes the tmux scrollback the preview scrolls through
    // (`capture-pane -S`), which is exactly what left the pane unscrollable (user: *"i cannot scroll
    // ... all the way to the top"* / *"we can ignore the resize if it breaks scrolling"*). A resize
    // PRESERVES scrollback (real-tmux `scrollback_depth_reaches_the_top_and_survives_a_resize`), so a
    // reflow needs no repaint. Modelled by drawing the same app at two terminal widths — the second is
    // a genuine resize of the selected row — and asserting zero clears throughout.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let driver = FakePane::live(&session, PANE_FIXTURE);
    let app = pane_app(&reg_path, driver.clone());
    let session_clears =
        |d: &FakePane| -> usize { d.clears().iter().filter(|s| *s == &session).count() };

    let mut wide = Terminal::new(TestBackend::new(120, 30)).unwrap();
    wide.draw(|f| render(f, &app)).expect("render wide");
    assert_eq!(
        session_clears(&driver),
        0,
        "the first fit must not clear scrollback: {:?}",
        driver.clears()
    );

    // A narrower terminal → a narrower preview → a genuine resize of the SAME row. It re-fits the
    // WIDTH but must STILL not clear.
    let mut narrow = Terminal::new(TestBackend::new(100, 30)).unwrap();
    narrow.draw(|f| render(f, &app)).expect("render narrow");
    assert_eq!(
        session_clears(&driver),
        0,
        "a resize must NOT clear scrollback — that is the wipe that killed the scroll: {:?}",
        driver.clears()
    );
    // And the resize really did re-fit (so the assertion above is not vacuous): the width changed.
    let widths: Vec<u16> = driver
        .resizes()
        .into_iter()
        .filter(|(s, ..)| s == &session)
        .map(|(_, c, _)| c)
        .collect();
    assert!(
        widths.len() >= 2 && widths.first() != widths.last(),
        "the narrower terminal should have re-fit the pane to a new width: {widths:?}"
    );
}

#[test]
fn preview_capture_depth_follows_the_scroll_so_the_top_is_reachable() {
    // The window `pane_window` opens can never reach further back than the capture handed to it, so
    // a FIXED capture depth was a hard scroll floor — user: *"user should be able to scroll all the
    // way to the top"*. The depth now grows with `detail_scroll`, so each page up reveals another
    // page until tmux's own history runs out. Proven by the depth pmtui asks tmux for at two offsets:
    // scrolling 500 lines back must deepen the capture by exactly 500.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let driver = FakePane::live(&session, PANE_FIXTURE);
    let mut app = pane_app(&reg_path, driver.clone());
    // The last depth pmtui asked tmux to capture for the selected row's pane.
    let last_depth = |d: &FakePane| -> usize {
        d.captures()
            .into_iter()
            .filter(|(s, _)| s == &session)
            .map(|(_, n)| n)
            .next_back()
            .expect("a capture for the selected row")
    };

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.detail_scroll = 0;
    terminal.draw(|f| render(f, &app)).expect("render at tail");
    let at_tail = last_depth(&driver);
    let tail_fit_rows = driver
        .resizes()
        .into_iter()
        .rfind(|(s, ..)| s == &session)
        .map(|(_, _, rows)| rows)
        .expect("fit selected pane at tail");

    app.detail_scroll = 500;
    terminal
        .draw(|f| render(f, &app))
        .expect("render scrolled up");
    let scrolled = last_depth(&driver);
    let scrolled_fit_rows = driver
        .resizes()
        .into_iter()
        .rfind(|(s, ..)| s == &session)
        .map(|(_, _, rows)| rows)
        .expect("fit selected pane while scrolled");

    assert_eq!(
        scrolled,
        at_tail + 500,
        "the capture depth must deepen with the scroll offset so the top stays reachable \
         (at tail {at_tail}, scrolled 500 → {scrolled})"
    );
    assert!(
        scrolled_fit_rows > tail_fit_rows,
        "alternate-screen headroom must grow with the scroll: tail={tail_fit_rows}, scrolled={scrolled_fit_rows}"
    );
    // And following the tail stays CHEAP — the deep capture is only paid while scrolled up.
    app.detail_scroll = 0;
    terminal
        .draw(|f| render(f, &app))
        .expect("render back at tail");
    assert_eq!(
        last_depth(&driver),
        at_tail,
        "returning to the tail must drop the capture back to the shallow follow depth"
    );
}

#[test]
fn changing_session_selection_resets_preview_scroll_to_the_live_tail() {
    let mut app = app_with(
        vec![
            view("first", Posture::Fresh, vec![]),
            view("second", Posture::Fresh, vec![]),
        ],
        UiMode::Normal,
    );
    app.detail_scroll = 42;
    app.detail_max.set(120);

    app.move_sel(1);

    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("second")
    );
    assert_eq!(app.detail_scroll, 0);
    assert_eq!(app.detail_max.get(), 0);
}

#[test]
fn preview_pane_fit_keeps_chunked_headroom_and_a_bounded_maximum() {
    assert_eq!(preview_pane_fit_rows(0, 0), 100);
    assert_eq!(preview_pane_fit_rows(77, 0), 200);
    assert_eq!(preview_pane_fit_rows(77, 1), 200);
    assert_eq!(preview_pane_fit_rows(77, 24), 250);
    assert_eq!(preview_pane_fit_rows(77, usize::MAX), 2_000);
}

#[test]
fn tall_preview_wheel_scrolls_after_alternate_screen_headroom_is_fitted() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let pane = (1..=200)
        .map(|row| format!("ALT ROW {row:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let driver = FakePane::live(&session, &pane);
    let mut app = pane_app(&reg_path, driver);
    let mut terminal = Terminal::new(TestBackend::new(140, 79)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();

    assert!(
        app.detail_max.get() > 0,
        "a tall preview must retain scroll headroom"
    );
    let detail = app.panes.get().detail;
    let mut handled_key = false;
    assert!(handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: detail.x + detail.width / 2,
            row: detail.y + detail.height / 2,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled_key,
    ));
    assert!(handled_key);
    assert_eq!(app.detail_scroll, 3);

    terminal.draw(|frame| render(frame, &app)).unwrap();
    assert!(screen_text(&terminal).contains("↑3"));
}

#[test]
fn preview_log_paints_the_agents_own_colour_into_the_buffer() {
    // THE FEATURE: the pane mirrors the agent's colours. Asserted on BUFFER CELLS,
    // not on text — a parser that got the styles right but a render path that dropped
    // them would still show flat white, which is the bug being fixed.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), false);
    let app = pane_app(&reg_path, FakePane::live(&session, PANE_FIXTURE_STYLED));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render styled pane");

    // Escapes are never text: no SGR parameters and no `file://` URL on screen…
    let screen = screen_text(&terminal);
    assert!(
        !screen.contains("38;5;153") && !screen.contains("[1m"),
        "raw SGR escapes leaked onto the screen: {screen}"
    );
    assert!(
        !screen.contains("file://") && !screen.contains("]8;"),
        "the OSC 8 wrapper leaked onto the screen: {screen}"
    );
    // …and the link TEXT survives, glued back to the row it belongs to.
    assert!(
        screen.contains("Write(p.txt)"),
        "OSC 8 link text was dropped: {screen}"
    );

    let rows = screen_rows(&terminal);
    // The row that DID carry the SGR.
    let (_, first_fg) = rows
        .iter()
        .find(|(t, _)| t.contains("reading the stall backstop"))
        .expect("the coloured row should render");
    assert!(
        first_fg.contains(&Color::Indexed(153)),
        "the agent's indexed colour never reached the buffer"
    );
    // The CONTINUATION row, which carries no SGR of its own. This is the assertion a
    // per-line-reset parser fails — and failing it is exactly "renders white".
    let (_, cont_fg) = rows
        .iter()
        .find(|(t, _)| t.contains("wrapped continuation"))
        .expect("the continuation row should render");
    assert!(
        cont_fg.contains(&Color::Indexed(153)),
        "a wrapped continuation row lost its colour — the reported bug"
    );
}

#[test]
fn styled_pane_never_panics_on_a_tiny_or_narrow_terminal() {
    // A pane full of escape sequences must not change the layout's panic-safety. 1x1
    // is the degenerate floor; the narrow widths straddle the layout tiers.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), false);
    let app = pane_app(&reg_path, FakePane::live(&session, PANE_FIXTURE_STYLED));
    for (w, h) in [
        (1u16, 1u16),
        (2, 3),
        (20, 5),
        (49, 8),
        (50, 8),
        (80, 24),
        (100, 30),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, &app))
            .unwrap_or_else(|e| panic!("render styled pane {w}x{h}: {e}"));
    }
}

#[test]
fn an_escapes_only_pane_is_not_mistaken_for_output() {
    // The capture is non-empty BYTES but empty SCREEN. `text.trim().is_empty()` reads
    // it as output and pins the Log section to a pane with nothing on it, hiding both
    // the legacy step transcript and the honest "up but quiet" placeholder.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let escapes_only = "\u{1b}[38;5;153m\u{1b}[0m\n\u{1b}[39m   \n";
    assert!(
        !escapes_only.trim().is_empty(),
        "fixture must be non-empty to `trim` or it proves nothing"
    );
    let app = pane_app(&reg_path, FakePane::live(&session, escapes_only));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render escapes-only pane");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("All done here"),
        "an escapes-only capture beat the step transcript: {screen}"
    );
    assert!(
        screen.contains("Wake #0"),
        "the title still claims a live pane source: {screen}"
    );
}

#[test]
fn preview_log_falls_back_to_the_step_transcript_when_no_pane_is_live() {
    // REGRESSION GUARD for `Mode::Auto`/legacy sessions: with no live `pmloop-…`
    // session but a `steps/<seq>.log` on disk, the Log section is still the old
    // `stream_json::render_transcript` tail, and the title still names the wake.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), true);
    let driver = FakePane::default(); // nothing alive
    let app = pane_app(&reg_path, driver.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render step fallback");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("All done here"),
        "step transcript tail not shown: {screen}"
    );
    assert!(
        screen.contains("Wake #0"),
        "title does not name the wake source: {screen}"
    );
    // A dead session is never captured — the `has-session` probe short-circuits.
    assert!(
        driver.captures().is_empty(),
        "captured a dead session: {:?}",
        driver.captures()
    );
    assert!(!session.is_empty());
}

#[test]
fn preview_log_placeholder_is_honest_about_why_it_is_empty() {
    // Neither source: the placeholder must say which situation this is. "no wakes
    // yet" was the old lie — it read the same whether the agent was down or just
    // quiet, and on the persistent path it NEVER stopped saying it.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, session) = loop_pane_fixture(dir.path(), false);

    // (1) nothing running at all.
    let app = pane_app(&reg_path, FakePane::default());
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render dead placeholder");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("agent not running"),
        "down-agent placeholder missing: {screen}"
    );
    assert!(
        !screen.contains("no wakes yet"),
        "the misleading 'no wakes yet' line is back: {screen}"
    );
    // It must also name a key that EXISTS. `A` has been unbound since tier and
    // autopilot were consolidated onto `m`, and this line renders on rows whose tier
    // chip reads `[A]` — so the old copy told the human to press the chip.
    assert!(
        screen.contains("m switches autopilot"),
        "the placeholder must name the real autopilot key: {screen}"
    );
    assert!(
        !screen.contains("A toggles"),
        "the unbound `A` key is back in the placeholder: {screen}"
    );

    // (2) the agent IS up, its pane is simply still empty — a different fact.
    let app = pane_app(&reg_path, FakePane::live_quiet(&session));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render quiet placeholder");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("agent is running"),
        "live-but-quiet placeholder missing: {screen}"
    );
}

#[test]
fn preview_log_degrades_when_the_pane_capture_fails() {
    // A broken/unavailable tmux must never blank or panic the dashboard: a failing
    // `capture_tail` falls through to the step log, and with no step log to the
    // placeholder.
    let dir = tempfile::tempdir().unwrap();

    // No step log → the honest placeholder.
    let (reg_path, session) = loop_pane_fixture(dir.path(), false);
    let app = pane_app(&reg_path, FakePane::capture_fails(&session));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render failed capture");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("agent is running"),
        "failed capture did not degrade to the placeholder: {screen}"
    );

    // With a step log → the transcript fallback, still no panic.
    let dir2 = tempfile::tempdir().unwrap();
    let (reg_path2, session2) = loop_pane_fixture(dir2.path(), true);
    let app = pane_app(&reg_path2, FakePane::capture_fails(&session2));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render failed capture with step log");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("All done here"),
        "failed capture did not fall back to the transcript: {screen}"
    );
}

#[test]
fn pane_tail_trims_trailing_blanks_and_keeps_the_tail() {
    // Trailing blank rows (a pane taller than its content) are dropped so the tail
    // sits flush; the window is the LAST `want` rows of what is left. Normal interior
    // spacing and Claude's own chrome are preserved — only oversized blank runs are compacted.
    let capture = "a\n\nb\nc\n\n   \n\n";
    assert_eq!(rows(&pane_tail(capture, 10)), vec!["a", "", "b", "c"]);
    assert_eq!(rows(&pane_tail(capture, 2)), vec!["b", "c"]);
    // Degenerate sizes/content can't panic.
    assert!(pane_tail(capture, 0).is_empty());
    assert!(pane_tail("", 5).is_empty());
    assert!(pane_tail("  \n \n", 5).is_empty());
    assert_eq!(rows(&pane_tail("only", usize::MAX)), vec!["only"]);
    // Rows carry the agent's style, and a row whose only content is escapes counts
    // as blank — so an escapes-only tail is trimmed exactly like a whitespace one.
    let styled = pane_tail("\u{1b}[38;5;153mkeep\n\u{1b}[39m\n", 5);
    assert_eq!(rows(&styled), vec!["keep"]);
    assert_eq!(styled[0].spans[0].style.fg, Some(Color::Indexed(153)));
}

#[test]
fn pane_tail_compacts_a_sparse_claude_alternate_screen() {
    let capture = format!(
        "Claude Code\n\n❯ tell me a joke\n\n● skeleton joke\n\n✻ done\n{}────────────────\n❯\n────────────────\nauto mode on\n",
        "\n".repeat(180)
    );

    let visible = rows(&pane_tail(&capture, 20));

    assert!(visible.iter().any(|line| line.contains("skeleton joke")));
    assert!(visible.iter().any(|line| line.contains("auto mode on")));
    assert!(
        !visible
            .windows(3)
            .any(|rows| rows.iter().all(|line| line.is_empty())),
        "oversized alternate-screen blank run was not compacted: {visible:?}"
    );
}

#[test]
fn the_preview_says_a_chat_is_holding_a_driven_row_instead_of_a_bare_idle() {
    // User: *"I switch a standard to autopilot, and it says it will take effect next tick, but that
    // never happens. next displays idle"*.
    //
    // Both halves of that were real: pmd was deferring on a live chat (fixed by handing the
    // conversation over on the flip), and the UI said `next: idle` — true about the LEDGER and
    // silent about the world, because `drive` returns from the chat gate before it touches `run`.
    // A row that is driven-but-held must say what is in the way, or the human is left pressing a
    // dial that reports success and changes nothing.
    let mut held = autopilot_loop_view("bot");
    held.human_attached = true;
    held.next_action = "idle".into();
    let mut free = autopilot_loop_view("bot");
    free.next_action = "idle".into();

    let screen_of = |v: ProjectView| {
        let app = app_with(vec![v], UiMode::Normal);
        let mut t = Terminal::new(TestBackend::new(140, 24)).unwrap();
        t.draw(|f| render(f, &app)).expect("render");
        screen_text(&t)
    };

    let held_screen = screen_of(held);
    assert!(
        held_screen.contains("human attached"),
        "a driven row with an attached human must SAY so: {held_screen}"
    );

    // And the message is specific to that state — an ordinary driven row still reports its ledger.
    let free_screen = screen_of(free);
    assert!(
        !free_screen.contains("human attached"),
        "no human is attached here: {free_screen}"
    );
    assert!(
        free_screen.contains("idle"),
        "an ordinary driven row still reports what the ledger says: {free_screen}"
    );
}

#[test]
fn the_empty_log_placeholder_does_not_tell_an_autopilot_row_to_press_m() {
    // The window after `m`, after `r`, and after Enter-on-a-paused-row: autopilot is ON, pmd has not
    // launched the agent yet, and the Log section said "agent not running — m switches autopilot" —
    // pointing at the dial the human had just turned, and reading as "nothing is happening" when the
    // launch was 500ms away. Same class as every other message in this codebase that outlived its
    // situation.
    let dir = tempfile::tempdir().unwrap();
    // The TIER comes from the view, not from disk: `pmd_drives_row` is what the placeholder branches
    // on, and `render` reads it off the row it is drawing.
    let (reg_path, _session) = loop_pane_fixture(dir.path(), false);

    // DRIVEN: nothing alive, autopilot on.
    let mut app = pane_app(&reg_path, FakePane::default());
    app.projects[0].tier = Some(Tier::Autopilot);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render driven");
    let driven = screen_text(&t);
    assert!(
        driven.contains("pmd is starting the agent"),
        "a driven row must say pmd is starting it: {driven}"
    );
    assert!(
        !driven.contains("m switches autopilot"),
        "…and must not point at the dial already turned: {driven}"
    );

    // UNDRIVEN: the original message is the right one — `m` really is the way to make it run.
    app.projects[0].tier = Some(Tier::Standard);
    let mut t2 = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t2.draw(|f| render(f, &app)).expect("render undriven");
    let undriven = screen_text(&t2);
    assert!(
        undriven.contains("m switches autopilot"),
        "an undriven row keeps the honest instruction: {undriven}"
    );
    assert!(
        !undriven.contains("pmd is starting the agent"),
        "…and must not claim a launch nobody is performing: {undriven}"
    );
}

/// The status the user was shown being cut off, verbatim from their report.
const LONG_STATUS: &str = "New 10-joke target met: round 4 delivered a labeled 2-per-style sampler \
     (dad/one-liner/observational/anti-joke/single-theme) because the earlier batches skewed to one \
     style";

#[test]
fn the_agents_own_status_wraps_instead_of_losing_its_second_half() {
    // User: *"status  New 10-joke target met: round 4 delivered a labeled 2-per-style sampler … beca…"*
    // → *"It gets cutoff. I think we should have a better status line"*. This is the ONLY channel for
    // "what is the agent doing / why did it stop", so the truncated half was the answer.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _session) = loop_pane_fixture(dir.path(), false);
    // `reg_with_agent_loop` roots the session at `<dir>/<id>`.
    let sp = ProjectPaths::for_session(dir.path().join("bot"), "bot");
    let mut l = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    l.last_status = Some(LONG_STATUS.into());
    job::save(&sp, &l).unwrap();

    let app = pane_app(&reg_path, FakePane::default());
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render tall");
    let tall = screen_text(&t);
    assert!(
        tall.contains("New 10-joke target met"),
        "the head of the status must still be there: {tall}"
    );
    assert!(
        tall.contains("skewed to one") || tall.contains("earlier batches"),
        "…and so must its SECOND HALF, which is the part that was lost: {tall}"
    );

    // A SHORT pane keeps the single row it always had: the head's rows come out of the log region, so
    // wrapping there would collapse the transcript — the budget in `preview_status_rows` is the point.
    let mut t2 = Terminal::new(TestBackend::new(120, 14)).unwrap();
    t2.draw(|f| render(f, &app)).expect("render short");
    let short = screen_text(&t2);
    assert!(
        short.contains("New 10-joke target met"),
        "a short pane still shows the start: {short}"
    );
    assert!(
        !short.contains("skewed to one style"),
        "…but must not steal the transcript's rows to finish the sentence: {short}"
    );
    assert!(
        short.contains('\u{2026}'),
        "and a clipped status must ADMIT it was clipped: {short}"
    );

    // The budget itself, at the two thresholds that matter.
    assert_eq!(
        preview_status_rows(12),
        1,
        "a 12-row pane has nothing to spare"
    );
    assert_eq!(preview_status_rows(28), PREVIEW_STATUS_MAX_ROWS);
}

#[test]
fn preview_head_has_a_spawned_fact_line_from_parent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_wake_fixture(dir.path(), None);
    app.projects[0].spawned_by = Some("parent-id".into());
    app.projects[0].spawned_by_label = Some("Release lead".into());
    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|f| render(f, app)).expect("render");
        terminal
    };

    let terminal = draw(&app);
    let screen = screen_text(&terminal);
    let rows = screen_rows(&terminal);
    let health = rows
        .iter()
        .position(|(row, _)| row.contains("control pmd"))
        .unwrap_or_else(|| panic!("health row missing: {screen}"));
    let lineage = rows
        .iter()
        .position(|(row, _)| row.contains("spawned  from Release lead"))
        .unwrap_or_else(|| panic!("spawned fact line missing: {screen}"));
    let next = rows
        .iter()
        .position(|(row, _)| row.contains("next "))
        .unwrap_or_else(|| panic!("next row missing: {screen}"));
    assert!(health < lineage && lineage < next, "{screen}");
    assert!(
        !screen.contains("parent-id"),
        "the label, not the raw id: {screen}"
    );
    let label = styles_under(&terminal, "spawned ").unwrap();
    assert!(
        label.iter().all(|(_, m)| m.contains(Modifier::DIM)),
        "the fact label recedes like every other fact label: {label:?}"
    );

    // Fork lineage wins when a row carries both links.
    app.projects[0].forked_from = Some("child".into());
    let screen = screen_text(&draw(&app));
    assert!(
        screen.contains("fork     from child · shared directory"),
        "{screen}"
    );
    assert!(!screen.contains("spawned "), "{screen}");
}

#[test]
fn a_staged_row_head_reads_starting_and_names_the_spawn_request_as_its_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_wake_fixture(dir.path(), None);
    app.projects[0].enabled = false;
    app.projects[0].tier = Some(Tier::Standard);
    app.projects[0].spawn_staged = true;
    app.projects[0].spawned_by = Some("parent".into());
    app.projects[0].spawned_by_label = Some("parent".into());
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    let rows = screen_rows(&terminal);
    assert!(
        rows.iter()
            .any(|(row, _)| row.contains(" starting… ") && row.contains("control you")),
        "{screen}"
    );
    assert!(
        rows.iter().any(
            |(row, _)| row.contains("next") && row.contains("being created by a spawn request")
        ),
        "{screen}"
    );
    // Every start path refuses a staged row, so neither the pause word nor its resume key shows.
    assert!(!screen.contains("paused"), "{screen}");
    assert!(!screen.contains("Enter resumes"), "{screen}");
}

#[test]
fn a_staged_rows_empty_preview_and_shelf_name_the_spawn_request_not_m_or_message() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _session) = loop_pane_fixture(dir.path(), false);
    let mut app = pane_app(&reg_path, FakePane::default());
    app.projects[0].enabled = false;
    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(140, 48)).unwrap();
        terminal.draw(|f| render(f, app)).expect("render");
        screen_text(&terminal)
    };

    // Control: the same row merely paused names `m` and offers a Message, both of which act.
    let paused = draw(&app);
    assert!(paused.contains("m switches autopilot"), "{paused}");
    assert!(paused.contains("message this session"), "{paused}");

    // Staged: `m` and `s` refuse it until its spawn request launches it, so neither is named.
    app.projects[0].spawn_staged = true;
    let staged = draw(&app);
    assert!(
        staged.contains("nothing to show yet — being created by a spawn request"),
        "the empty preview must name what the row is waiting on: {staged}"
    );
    assert!(
        staged.contains("a spawn request is creating it · message it once it starts"),
        "the shelf must name why `s` refuses: {staged}"
    );
    for refused in ["m switches autopilot", "message this session"] {
        assert!(!staged.contains(refused), "names {refused}: {staged}");
    }
}
