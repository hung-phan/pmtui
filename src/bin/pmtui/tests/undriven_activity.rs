//! A row pmd does not drive takes its STATUS from its pane, not from a ledger nothing advances.
//!
//! An autopilot session's ledger and `needs-you.json` marker are written by the harness. Turn
//! autopilot off and both freeze exactly where they were — so a question the human then answered in
//! the terminal keeps `needs you` on the row, the board column and the header's call-to-action count
//! while the agent works (user: *"if my session is on autopilot before but now it is off, when it is
//! working, it still displays under needs you instead of working"*).
//!
//! These use a REAL tmux pane, because the whole point is that the live capture is the newer
//! evidence; a fake that returns a canned classification would assert the fixture, not the rule.

use super::*;

/// A pane a human can see a turn running in: claude's own interrupt hint, which
/// [`tmux::classify_pane`] reads as Busy.
const BUSY_PANE: &str = "\u{2733} Thinking\u{2026} (esc to interrupt)\n";

/// A bare claude prompt — Busy's opposite, and only trusted after two byte-stable captures.
const IDLE_PANE: &str = "> \n";

/// A session parked on one open stop, as autopilot left it.
fn blocked_ledger(paths: &ProjectPaths) {
    let mut ledger = AgentLoopState::fresh(Engine::Claude, Some(300), 1000);
    ledger.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    ledger.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(paths, &ledger).unwrap();
}

/// Hold `pane` on screen in a real tmux session for the row to be captured from.
fn hold_pane(driver: &TmuxDriver, session: &str, root: &Path, pane: &str) {
    let fixture = root.join("pane.txt");
    std::fs::write(&fixture, pane).unwrap();
    driver
        .launch_interactive(
            session,
            root,
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("cat {}; sleep 3600", fixture.display()),
            ],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();
}

/// One refreshed row, from a registry at `tier` whose session's pane holds `pane`. `settles` is how
/// many refreshes to run: an Idle pane is believed only after the two-capture stability gate.
///
/// `socket` is per TEST, not per file: `TmuxSocket`'s drop kills its whole server, so two tests
/// sharing a name would tear down each other's panes as they finish in parallel.
fn row_for(
    dir: &Path,
    socket: &str,
    tier: Tier,
    pane: &str,
    settles: u32,
    marker: Option<&str>,
) -> ProjectView {
    let (registry, root) = reg_with_tier(dir, "bot", tier);
    let paths = ProjectPaths::for_session(&root, "bot");
    blocked_ledger(&paths);
    if let Some(body) = marker {
        std::fs::write(paths.needs_you(), body).unwrap();
    }
    let socket = TmuxSocket::new(socket);
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    hold_pane(&driver, &session, &root, pane);

    let mut app = loop_app(&registry);
    app.socket = socket.name().to_string();
    for _ in 0..settles {
        app.refresh();
    }
    let view = app.projects[0].clone();
    let _ = driver.terminate(&session);
    view
}

#[test]
fn a_standard_rows_working_pane_outranks_its_frozen_needs_you() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let view = row_for(
        dir.path(),
        "pmtui-undriven-busy",
        Tier::Standard,
        BUSY_PANE,
        1,
        None,
    );

    assert_eq!(view.agent_working, Some(true), "the pane is mid-turn");
    assert_eq!(
        view.posture,
        Posture::Working,
        "a working agent is working, whatever the frozen ledger still says"
    );
    assert_eq!(view.next_action, "working");
    assert_eq!(board_column(&view), BoardColumn::Working);
    assert_eq!(
        status_category(&view),
        1,
        "and it is counted among the live sessions, not the ones asking for a human"
    );
    // THE QUESTION IS STILL OPEN. Only the claim that the harness is waiting on a human went: the
    // stop is what the human types an answer to.
    assert_eq!(
        view.stops.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["stop-1"]
    );
}

#[test]
fn a_standard_rows_blocked_marker_also_yields_to_a_working_pane() {
    if !tmux_available() {
        return;
    }
    // The OTHER source of a frozen `needs you`: with autopilot off nothing folds the agent's own
    // `needs-you.json` into the ledger, so `view::read_agent_loop` forces the posture straight from
    // the marker (leaving `stops` empty on purpose). That marker is just as stale as the ledger once
    // the agent is working again.
    let dir = tempfile::tempdir().unwrap();
    let marker = r#"{ "state": "blocked", "seq": 3, "status": "ship it?" }"#;
    let view = row_for(
        dir.path(),
        "pmtui-undriven-marker",
        Tier::Standard,
        BUSY_PANE,
        1,
        Some(marker),
    );

    assert_eq!(view.posture, Posture::Working, "next: {}", view.next_action);
    assert_eq!(board_column(&view), BoardColumn::Working);
}

#[test]
fn a_standard_rows_idle_pane_leaves_needs_you_where_it_is() {
    if !tmux_available() {
        return;
    }
    // The correction is LIVE ACTIVITY, not autopilot being off. An agent sitting at its prompt with
    // an unanswered question is exactly the row that should be asking for a human.
    let dir = tempfile::tempdir().unwrap();
    let view = row_for(
        dir.path(),
        "pmtui-undriven-idle",
        Tier::Standard,
        IDLE_PANE,
        2,
        None,
    );

    assert_eq!(view.agent_working, Some(false), "a settled bare prompt");
    assert_eq!(view.posture, Posture::NeedsYou);
    assert_eq!(board_column(&view), BoardColumn::NeedsYou);
    assert_eq!(status_category(&view), 0);
}

#[test]
fn an_autopilot_rows_needs_you_survives_a_working_pane() {
    if !tmux_available() {
        return;
    }
    // A DRIVEN row's ledger is not frozen — pmd wrote that stop this tick and will clear it when the
    // answer lands — so the escalation is current and must keep its place. Were this corrected too,
    // the one column a human watches would lose the rows autopilot raised a question on.
    let dir = tempfile::tempdir().unwrap();
    let view = row_for(
        dir.path(),
        "pmtui-undriven-driven",
        Tier::Autopilot,
        BUSY_PANE,
        1,
        None,
    );

    assert_eq!(view.agent_working, Some(true));
    assert_eq!(view.posture, Posture::NeedsYou);
    assert_eq!(board_column(&view), BoardColumn::NeedsYou);
}
