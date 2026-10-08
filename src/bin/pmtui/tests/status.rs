//! What the dashboard says about state: the status a row reads off its ledger, the
//! daemon's up/down/unknown report, and the rule that every normal-mode dead end leaves
//! a status behind instead of pressing nothing.

use super::*;

#[test]
fn cycle_tier_without_a_selection_or_registry_entry_explains_itself() {
    // Both dead ends used to be bare `return`s: the key looked broken. Match
    // `cycle_tier`'s style and always leave a status behind.
    let mut app = app_with(vec![], UiMode::Normal);
    app.cycle_tier();
    assert!(
        app.status.contains("nothing is selected"),
        "no selected row explains itself: {}",
        app.status
    );

    // A selected row whose id is absent from the registry (the registry changed
    // under a stale view).
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let mut app = app_with(vec![agent_loop_view("ghost")], UiMode::Normal);
    app.registry_path = reg_path;
    app.cycle_tier();
    assert!(
        app.status.contains("ghost is gone from the list"),
        "a row missing from the registry explains itself: {}",
        app.status
    );
}

#[test]
fn request_attach_without_a_selection_or_registry_entry_explains_itself() {
    // The SAME two dead ends as `cycle_tier`'s, on the key people press most: both
    // were bare `return`s, so Enter looked broken ("i don't see anything is
    // running"). Always leave a status behind.
    let mut app = app_with(vec![], UiMode::Normal);
    app.request_attach();
    assert!(
        app.status.contains("nothing is selected"),
        "no selected row explains itself: {}",
        app.status
    );
    assert!(
        app.status.contains("Enter"),
        "the refusal names the key that was pressed: {}",
        app.status
    );

    // A rendered row that is absent from the freshly-loaded registry. This one also
    // has to REFRESH: without it the stale row stays on screen and Enter keeps doing
    // nothing forever, however many times it is pressed.
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    Registry::default().save(&reg_path).unwrap();
    let mut app = app_with(vec![agent_loop_view("ghost")], UiMode::Normal);
    app.registry_path = reg_path;
    app.request_attach();
    assert!(
        app.status.contains("ghost is gone from the list"),
        "a row missing from the registry explains itself: {}",
        app.status
    );
    assert!(
        app.projects.is_empty(),
        "the stale row must be refreshed AWAY, not left on screen to be re-pressed"
    );
    // …and the proof it is gone: a SECOND Enter behaves differently from the first
    // (the empty-selection refusal), rather than repeating the same silent no-op.
    app.request_attach();
    assert!(
        app.status.contains("nothing is selected"),
        "the second Enter must not repeat the stale-row refusal: {}",
        app.status
    );
}

#[test]
fn a_stale_row_is_refreshed_away_rather_than_left_to_be_re_pressed() {
    // A row on screen but absent from the registry is the "this key does nothing,
    // forever" shape: with no `refresh()` the row survives and every later press
    // takes the same dead branch. `cycle_tier`/`selected_brief`/`remove_project`
    // already refreshed; `Enter` and `d` did not. The whole family must agree.
    for key in [
        KeyCode::Enter,
        KeyCode::Char('m'),
        KeyCode::Char('d'),
        KeyCode::Char('g'),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("registry.json");
        Registry::default().save(&reg_path).unwrap();
        let mut app = app_with(vec![autopilot_loop_view("ghost")], UiMode::Normal);
        app.registry_path = reg_path;
        handle_key(&mut app, key, KeyModifiers::NONE);
        assert!(
            app.status.contains("ghost") && app.status.contains("gone from the list"),
            "{key:?} must name the stale row: {}",
            app.status
        );
        assert!(
            app.projects.is_empty(),
            "{key:?} left the stale row on screen to be pressed again"
        );
    }
}

#[test]
fn every_normal_mode_dead_end_leaves_a_status_behind() {
    // The class of bug, guarded once: a Normal-mode key pressed with NOTHING
    // selected must never be a silent no-op. The autonomy dial (now `m`) had this
    // right and `Enter`/`d` did not. The status is cleared first so an unchanged
    // one is a failure. `r` is here too: it is the newest key, and a brand-new key with
    // no empty-list refusal is exactly how this class of bug got in the first time.
    for key in ['\n', 'd', 'R', 'm', 'g', 's', 'r', 'p'] {
        let mut app = app_with(vec![], UiMode::Normal);
        app.status = String::new();
        let code = if key == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(key)
        };
        handle_key(&mut app, code, KeyModifiers::NONE);
        assert!(
            !app.status.is_empty(),
            "{key:?} with nothing selected left the human no feedback at all"
        );
        assert!(
            app.status.contains("nothing is selected"),
            "{key:?} names the dead end: {}",
            app.status
        );
    }
}

#[test]
fn agent_loop_row_status_reflects_ledger_state() {
    // For a DRIVEN (Autopilot) row — the tier `loop_view` seeds — the row/preview reflects the
    // ledger `run`: Running → working, Monitoring → a check-in countdown, Blocked → the
    // attention badge + the stop in the preview. (A Standard row is human-driven and its ledger
    // is a frozen snapshot, so it reflects live REPL liveness instead — see the m74 Standard
    // tests; this test deliberately uses a driven row where the ledger IS the truth.)
    let (_d, v) = loop_view(
        job::JobRun::Running {
            seq: 0,
            session: "pmj-bot-0".into(),
            deadline: 9999,
        },
        vec![],
        1500,
    );
    assert_eq!(v.posture, Posture::Running);
    assert!(
        project_row_text(&v).contains("offline"),
        "{}",
        project_row_text(&v)
    );
    assert_eq!(v.next_action, "working");

    let (_d, v) = loop_view(job::JobRun::Monitoring { until: 1800 }, vec![], 1500);
    assert_eq!(v.posture, Posture::Monitoring);
    assert!(
        project_row_text(&v).contains("offline"),
        "{}",
        project_row_text(&v)
    );
    assert!(
        v.next_action.contains("monitoring") && v.next_action.contains("00:05:00"),
        "a monitoring hint with the remaining time: {}",
        v.next_action
    );

    let (_d, v) = loop_view(
        job::JobRun::Blocked {
            stop_ids: vec!["stop-1".into()],
            since: 1000,
        },
        vec![open_stop("stop-1", pmstate::StopKind::Publish)],
        1500,
    );
    assert_eq!(v.posture, Posture::NeedsYou);
    let row = project_row_text(&v);
    assert!(
        row.contains("needs you"),
        "the row says it needs you: {row}"
    );
    // The SEVERITY is the preview's job now (the `!!!` badge went at the user's request), and
    // the preview assertion below is what covers it.
    assert_eq!(attention::level(&v), attention::Level::Hard);
    // The preview renders the first stop (its kind, as OpenStop has no question) — ON A DRIVEN ROW.
    let mut driven = v.clone();
    driven.tier = Some(Tier::Autopilot);
    let app = app_with(vec![driven], UiMode::Normal);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render preview");
    assert!(
        screen_text(&terminal).contains("publish"),
        "the first stop shows in the preview"
    );

    // AND NOT ON AN UNDRIVEN ONE. User: *"when autopilot is off, the answer panel still shows. This
    // isn't correct, since user will drive, we don't need to do anything here"*. `a` refuses on such a
    // row (nothing reads `answers.json` for it), so a block of lettered options was offering a choice
    // no key would take — and the human drives that session in the chat, where the question already is.
    let mut standard = v.clone();
    standard.tier = Some(Tier::Standard);
    let off = app_with(vec![standard], UiMode::Normal);
    let mut t2 = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t2.draw(|f| render(f, &off))
        .expect("render undriven preview");
    let screen = screen_text(&t2);
    assert!(
        !screen.contains("publish"),
        "an undriven row must not show an answer panel nothing can answer: {screen}"
    );
    // …and the head must not count the queue either: no `waiting`/`decision` badge.
    assert!(
        !screen.contains("1 decision"),
        "nor a decisions badge for a queue nobody serves: {screen}"
    );
}

#[test]
fn preview_keeps_the_full_hms_countdown_visible_at_110_columns() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring {
        until: SystemClock.now() + 300,
    };
    job::save(&paths, &ledger).unwrap();
    let app = loop_app(&reg_path);
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render preview");
    let screen = screen_text(&terminal);
    // THE SHAPE, NOT THE INSTANT. `until` is `now() + 300` and the clock keeps running: loading the
    // registry and drawing the frame took over a second on CI under instrumentation, which rendered
    // `00:04:59` and failed an assertion on the literal `00:05:00`. The regression this test exists for
    // is CLIPPING — a countdown cut short by a narrow preview — so it asserts eight characters of
    // `HH:MM:SS` after the label, with the minute still reading four or five. The exact formatting is
    // pinned deterministically by `loop_view_reports_posture_and_next_action`, which uses a fixed clock.
    let label = "check in ";
    let at = screen
        .find(label)
        .unwrap_or_else(|| panic!("no countdown in the preview at all: {screen}"));
    let hms: String = screen[at + label.len()..].chars().take(8).collect();
    let shaped = hms.len() == 8
        && hms.bytes().enumerate().all(|(i, b)| match i {
            2 | 5 => b == b':',
            _ => b.is_ascii_digit(),
        })
        && hms.starts_with("00:0")
        && matches!(&hms[3..5], "04" | "05");
    assert!(
        shaped,
        "the preview clipped the countdown at a supported width: {hms:?} in {screen}"
    );
}

#[test]
fn refresh_marks_a_live_standard_repl_running_even_without_a_chat_marker() {
    // m75. THE COMMON Standard flow — "runs from create" (also `r`/resume) via
    // `start_undriven_session` — launches a live `pmchat-` REPL but writes NO chat_lock marker,
    // and the `s` send delivers straight into that live REPL. The marker-gated `human_attached` is
    // blind to it, so the row read idle while claude was alive and working — and stayed idle
    // right after a send (user: *"when i use standard and use send feature, i don't see the status
    // update correctly on claude"*). refresh must set `session_live` from a REAL, marker-independent
    // pane probe so the row buckets running.
    //
    // Needs a REAL tmux server: `refresh` builds its own `TmuxDriver::with_socket(self.socket)`
    // (not the injectable preview seam), so a FakeDriver cannot inject this liveness.
    if !tmux_available() {
        eprintln!("skipping standard-send status test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // Standard AgentLoop
    std::fs::create_dir_all(&root).unwrap();
    let socket = TmuxSocket::new("pmtui-std-send"); // RAII: kills the server + removes the socket on drop
    let driver = TmuxDriver::with_socket(socket.name());
    let chat_session = session_name("bot", &root);
    // A live pmchat- REPL, and deliberately NO chat.json marker (the create/restart path writes none).
    driver
        .launch_interactive(
            &chat_session,
            &root,
            &["sh".to_string()],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.refresh();
    let v = app.projects[0].clone();
    let _ = driver.terminate(&chat_session);

    assert!(
        v.session_live,
        "a live pmchat- REPL sets session_live even with no chat marker"
    );
    assert!(
        !v.human_attached,
        "human_attached stays false without a marker — its badge semantics are unchanged"
    );
    assert_eq!(
        status_category(&v),
        1,
        "a live Standard session ⇒ running (●), not idle"
    );
}

#[test]
fn refresh_marks_a_standard_row_running_off_a_leftover_loop_pane() {
    // m75 secondary: an Autopilot→Standard flip leaves a live `pmloop-` behind (pmd stops driving
    // it but never reaps it). session_live must reflect that live loop pane too, so `s`-sending to
    // it shows running rather than idle. Real tmux, same reason as above.
    if !tmux_available() {
        eprintln!("skipping standard loop-pane status test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    std::fs::create_dir_all(&root).unwrap();
    let socket = TmuxSocket::new("pmtui-std-loop");
    let driver = TmuxDriver::with_socket(socket.name());
    let loop_session = session_name("bot", &root);
    driver
        .launch_interactive(
            &loop_session,
            &root,
            &["sh".to_string()],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.refresh();
    let v = app.projects[0].clone();
    let _ = driver.terminate(&loop_session);

    assert!(
        v.session_live,
        "a live pmloop- sets session_live for a Standard row"
    );
    assert_eq!(status_category(&v), 1, "⇒ running (●)");
}

#[test]
fn a_standard_row_with_nothing_alive_is_idle() {
    // The other half of the contract, unit-level: with neither pane alive, refresh's real probe
    // finds nothing on the (empty) test socket, so `session_live` stays false and the row is idle
    // (○). The fix must not paint every Standard row running. Needs no live tmux — the point is the
    // ABSENCE of a live session, which an empty socket already gives.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = app_with(vec![], UiMode::Normal);
    app.registry_path = reg_path;
    app.refresh();

    let v = &app.projects[0];
    assert!(!v.session_live, "no live pane ⇒ session_live false");
    assert_eq!(status_category(v), 2, "nothing alive ⇒ idle (○)");
}

#[test]
fn refresh_reads_an_idle_claude_prompt_as_idle_not_running() {
    // m76 — THE reported bug end to end: an alive-but-idle claude read running (●). A Standard row
    // now buckets on ACTIVITY. refresh classifies the live pane's CONTENT; an idle claude sitting at
    // a bare `❯` prompt, byte-stable across the two-observation confirmation gate, reads idle (○) —
    // NOT running. Real tmux, because refresh builds its own TmuxDriver and captures the pane.
    if !tmux_available() {
        eprintln!("skipping idle-status test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot"); // Standard AgentLoop
    std::fs::create_dir_all(&root).unwrap();
    let idle_file = dir.path().join("idle.txt");
    std::fs::write(&idle_file, IDLE_CLAUDE_PANE).unwrap();
    let socket = TmuxSocket::new("pmtui-idle-status");
    let driver = TmuxDriver::with_socket(socket.name());
    let chat = session_name("bot", &root);
    // Freeze the pane on an idle claude prompt: print the fixture, then sleep so the content is stable.
    driver
        .launch_interactive(
            &chat,
            &root,
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("cat {}; sleep 3600", idle_file.display()),
            ],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    // TWO refreshes: the confirmation gate needs two byte-stable Idle observations before it flips ○.
    app.refresh();
    app.refresh();
    let v = app.projects[0].clone();
    let _ = driver.terminate(&chat);

    assert!(v.session_live, "the pane is alive");
    assert_eq!(
        v.agent_working,
        Some(false),
        "an idle bare prompt, byte-stable across two ticks, is idle"
    );
    assert_eq!(
        status_category(&v),
        2,
        "⇒ idle (○), NOT running — the exact reported symptom, fixed"
    );
}

#[test]
fn refresh_reads_a_working_claude_pane_as_running() {
    // The other half: a claude that IS working — its pane carries the `esc to interrupt` busy marker
    // — reads running (●) on the first tick (no gate needed for Busy). Guards against the fix
    // overcorrecting into "everything idle". Real tmux, same reason as above.
    if !tmux_available() {
        eprintln!("skipping working-status test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    std::fs::create_dir_all(&root).unwrap();
    let busy_file = dir.path().join("busy.txt");
    // A working claude screen: a streaming spinner line + the `esc to interrupt` busy marker.
    std::fs::write(
        &busy_file,
        "\u{273b} Quantumizing\u{2026} (3s \u{b7} \u{2193} 5 tokens)\n(esc to interrupt)\n",
    )
    .unwrap();
    let socket = TmuxSocket::new("pmtui-busy-status");
    let driver = TmuxDriver::with_socket(socket.name());
    let chat = session_name("bot", &root);
    driver
        .launch_interactive(
            &chat,
            &root,
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("cat {}; sleep 3600", busy_file.display()),
            ],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.refresh();
    let v = app.projects[0].clone();
    let _ = driver.terminate(&chat);

    assert!(v.session_live, "the pane is alive");
    assert_eq!(v.agent_working, Some(true), "a busy pane is working");
    assert_eq!(status_category(&v), 1, "⇒ running (●)");
}

#[test]
fn refresh_busy_autopilot_pane_overrides_a_completed_old_turn() {
    // A completed pmd-nudged turn leaves `size > baseline` (`Some(false)`), but the SAME
    // persistent terminal can start a later turn without another pmd nudge. The live Busy pane
    // must override that stale "last turn finished" fact or an actively working Autopilot row
    // stays idle until its next marker.
    if !tmux_available() {
        eprintln!("skipping Autopilot live-status test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    std::fs::create_dir_all(&root).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring { until: 9999 };
    ledger.turn_count_at_nudge = Some(1);
    job::save(&paths, &ledger).unwrap();
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::write(paths.turn_signal(), "..").unwrap(); // size 2 > baseline 1 ⇒ old turn done

    let busy_file = dir.path().join("busy.txt");
    std::fs::write(
        &busy_file,
        "\u{273b} Cooking\u{2026} (5m \u{b7} \u{2193} 7.6k tokens)\n",
    )
    .unwrap();
    let socket = TmuxSocket::new("pmtui-autopilot-busy");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    driver
        .launch_interactive(
            &session,
            &root,
            &[
                "sh".to_string(),
                "-c".to_string(),
                format!("cat {}; sleep 3600", busy_file.display()),
            ],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.refresh();
    let v = app.projects[0].clone();
    let _ = driver.terminate(&session);

    assert_eq!(
        v.agent_working,
        Some(true),
        "the live Busy pane must override the completed old turn"
    );
    assert_eq!(
        status_category(&v),
        1,
        "active Autopilot work ⇒ running (●)"
    );
}

#[test]
fn refresh_dead_autopilot_pane_overrides_an_outstanding_old_turn() {
    // `has-session` remains true for a remain-on-exit corpse, and its last frame can still
    // look idle. The pane-dead probe must override a stale outstanding-turn baseline or the
    // dashboard claims a process that has already exited is working.
    if !tmux_available() {
        eprintln!("skipping Autopilot dead-pane status test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    std::fs::create_dir_all(&root).unwrap();
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.run = job::JobRun::Monitoring { until: 9999 };
    ledger.turn_count_at_nudge = Some(1);
    job::save(&paths, &ledger).unwrap();
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    std::fs::write(paths.turn_signal(), ".").unwrap(); // size == baseline ⇒ stale working

    let socket = TmuxSocket::new("pmtui-autopilot-dead");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("bot", &root);
    driver
        .launch_interactive(
            &session,
            &root,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "printf '> \\n'; cat".to_string(),
            ],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();
    let target = format!("={session}:");
    let remain = std::process::Command::new("tmux")
        .args([
            "-L",
            socket.name(),
            "set-option",
            "-t",
            &target,
            "remain-on-exit",
            "on",
        ])
        .status()
        .unwrap();
    assert!(remain.success(), "set remain-on-exit");
    let eof = std::process::Command::new("tmux")
        .args(["-L", socket.name(), "send-keys", "-t", &target, "C-d"])
        .status()
        .unwrap();
    assert!(eof.success(), "send EOF");
    for _ in 0..100 {
        if driver.pane_dead(&session).unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        driver.pane_dead(&session).unwrap_or(false),
        "pane became dead"
    );

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();
    app.refresh();
    let v = app.projects[0].clone();
    let _ = driver.terminate(&session);

    assert!(!v.session_live, "a dead pane is not a live session");
    assert_eq!(v.agent_working, Some(false), "a dead pane is idle");
    assert_eq!(status_category(&v), 2, "dead Autopilot pane ⇒ idle (○)");
}

// --- S9: compose the session brief in $EDITOR (Ctrl+E) -----------------------
