//! The TURN ANCHOR on a real pane: does a working Standard row keep its `●`?
//!
//! Real tmux, because this is the wiring and not the predicate. `refresh/tests.rs` proves
//! `turn_anchor_step` and `turn_in_progress` in isolation; what no pure test can show is that the
//! dashboard reads the right file for the right row and that the veto survives the hop from one
//! observation to the next tick's. That hop is the whole mechanism.
//!
//! The pane is a REAL claude frame mid-tool-call: the bare composer is on screen with no spinner
//! above it, which is exactly why two byte-stable captures of a working agent confirmed idle.

use super::*;

/// Hold a static frame on a pane for the length of a test.
fn launch_pane(driver: &TmuxDriver, session: &str, root: &Path, pane: &str) {
    let fixture = root.join("claude-pane.txt");
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

/// Claude's bare composer with NOTHING above it — the frame a silent tool call leaves on screen,
/// and the frame `classify_pane` reads as Idle. Byte-stable across ticks, so the confirmation gate
/// passes and only the anchor can tell this apart from a finished turn.
const PANE_BARE_COMPOSER: &str = concat!(
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
);

#[test]
fn a_standard_row_mid_tool_call_keeps_working_until_the_hook_fires() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let paths = ProjectPaths::for_session(&root, "bot");
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();

    let socket = TmuxSocket::new("pmtui-turn-anchor");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    launch_pane(&driver, &session, &root, PANE_BARE_COMPOSER);

    let mut app = loop_app(&registry);
    app.socket = socket.name().to_string();

    // THE CONTROL, and the behaviour before this change: with no anchor, two byte-stable captures
    // of this frame are the only evidence there is, so the row reads idle and draws `○` on an agent
    // that is mid-tool-call. Asserted rather than assumed, because a test that only shows the fixed
    // case cannot show the fix mattered.
    app.refresh();
    app.refresh();
    assert_eq!(
        app.projects[0].agent_working,
        Some(false),
        "without an anchor the stable frame is all the evidence there is"
    );

    // Arm the anchor as a Busy tick would have: no turn completed (no signal file ⇒ 0) at the
    // moment this dashboard last saw the pane working.
    app.turn_anchor
        .insert("bot".to_string(), (0, SystemClock.now()));
    app.refresh();
    app.refresh();
    assert_eq!(
        app.projects[0].agent_working,
        Some(true),
        "the anchored turn has not ended, so the idle confirmation is withheld"
    );
    assert!(
        app.turn_anchor.contains_key("bot"),
        "an idle frame must not disarm the anchor — the next tick needs it"
    );

    // THE TURN ENDS. One byte from the engine's turn-end hook, the same unchanged frame, and the
    // row reads idle. Nothing else about the pane changed, which is the point.
    std::fs::write(paths.turn_signal(), ".").unwrap();
    app.refresh();
    assert_eq!(
        app.projects[0].agent_working,
        Some(false),
        "a completed turn lifts the veto and the stable frame decides"
    );

    let _ = driver.terminate(&session);
}

#[test]
fn an_attached_human_drops_the_anchor_rather_than_claiming_their_turn() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let paths = ProjectPaths::for_session(&root, "bot");
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();

    let socket = TmuxSocket::new("pmtui-turn-anchor-attached");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    launch_pane(&driver, &session, &root, PANE_BARE_COMPOSER);

    let mut app = loop_app(&registry);
    app.socket = socket.name().to_string();
    // The attach-intent marker is what `refresh` reads for `human_attached` without a real client.
    chat_lock::mark(
        &paths,
        std::process::id(),
        &session,
        socket.name(),
        SystemClock.now(),
    )
    .unwrap();
    app.turn_anchor
        .insert("bot".to_string(), (0, SystemClock.now()));
    app.refresh();

    assert!(app.projects[0].human_attached);
    assert!(
        !app.turn_anchor.contains_key("bot"),
        "a human drives the same hooked process, so this anchor's count means nothing"
    );

    let _ = driver.terminate(&session);
}
