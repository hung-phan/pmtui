//! The cadence: the chip and key that exist only on Autopilot, the prompt `m` opens when
//! a session has none, the bounds `c` clamps to, and the timer a change restarts instead
//! of waiting out the old one.

use super::*;

#[test]
fn the_cadence_chip_and_key_are_autopilot_only() {
    // *"make the bottom menu to only display cadence if we change to autopilot"*. Both halves,
    // because a hidden chip over a working key is the same inconsistency in reverse: a human
    // who typed `c` from memory would get a dial that moves a number pmd never reads.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_tier(dir.path(), "bot", Tier::Standard);

    // STANDARD: no chip…
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();
    let bar = line_text(&keybar_line(&app, 260));
    assert!(
        !bar.contains("Cadence"),
        "a Standard row must not advertise the heartbeat dial: {bar}"
    );
    // …and the key REFUSES, naming what makes it matter (the way `a`'s refusal names `Enter`).
    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "{:?}", app.mode);
    assert!(
        app.status.contains("autopilot") && app.status.contains('m'),
        "the refusal must name the key that makes cadence matter: {}",
        app.status
    );

    // AUTOPILOT: both come back.
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    assert!(line_text(&keybar_line(&app, 260)).contains("Cadence"));
    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::EditingCadence { .. }),
        "{:?}",
        app.mode
    );
}

#[test]
fn m_prompts_for_the_cadence_when_the_session_has_none() {
    // *"we need to prompt user for cadence if the value is not set"*. A Standard session has no
    // cadence at all now (the create form does not ask), so `m` chains TWO prompts: goal, then
    // heartbeat, then the flip. Asserted as a sequence, because the value of the chain is that
    // nothing is written until the end of it.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    // No cadence anywhere: the state a Standard create leaves behind.
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.cadence_s = None;
        job::save(&sp, &l).unwrap();
        let mut reg = Registry::load(&reg_path).unwrap();
        reg.projects[0].cadence_s = None;
        reg.save(&reg_path).unwrap();
        state::write_control(&sp, &state::Control::default()).unwrap();
    }
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);

    // 1. `m` asks for the goal.
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(
        matches!(
            app.mode,
            UiMode::EditingGoal {
                then_autopilot: true,
                ..
            }
        ),
        "{:?}",
        app.mode
    );
    // 2. Saving it hands off to the CADENCE prompt instead of flipping.
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        matches!(
            app.mode,
            UiMode::EditingCadence {
                then_autopilot: true,
                ..
            }
        ),
        "an unset cadence must be asked for before the flip: {:?}",
        app.mode
    );
    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Standard,
        "nothing may be written until the chain finishes"
    );

    // 3. Typing one lands BOTH the cadence and the flip.
    for c in "10m".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        state::read_control(&sp).unwrap().human_cadence_s,
        Some(600),
        "{}",
        app.status
    );
    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot,
        "{}",
        app.status
    );
    assert!(
        app.status.contains("Autopilot") && app.status.contains("10m"),
        "the status reports both halves: {}",
        app.status
    );
    assert!(app.spawned_pmd.borrow().is_empty(), "no daemon spawned");
}

#[test]
fn the_cadence_prompt_adopts_the_default_on_an_empty_save_and_still_flips() {
    // An empty save in `m`'s cadence prompt means "take the default", not "keep the unset value
    // I was just asked about" — otherwise the human is asked again on the next flip for
    // something they already waved through. It is WRITTEN, so the question is settled.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.cadence_s = None;
        job::save(&sp, &l).unwrap();
        let mut reg = Registry::load(&reg_path).unwrap();
        reg.projects[0].cadence_s = None;
        reg.save(&reg_path).unwrap();
        state::write_control(&sp, &state::Control::default()).unwrap();
    }
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // goal kept
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // cadence: take the default

    assert_eq!(
        state::read_control(&sp).unwrap().human_cadence_s,
        Some(job_engine::DEFAULT_CADENCE_S),
        "the default must be RECORDED, not left unset: {}",
        app.status
    );
    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot
    );
    assert!(app.status.contains("default"), "{}", app.status);
}

#[test]
fn an_existing_goal_can_be_kept_when_turning_autopilot_on() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);
    app.mode = UiMode::EditingGoal {
        id: "bot".into(),
        brief: sp.brief(),
        current: "goal".into(),
        input: goal_buf(""),
        then_autopilot: true,
    };

    app.submit_goal_edit();

    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot,
        "{}",
        app.status
    );
    assert!(
        app.status.contains("goal kept"),
        "an empty save must distinguish keeping the existing goal: {}",
        app.status
    );
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap(),
        "goal",
        "keeping the goal must not rewrite its contents"
    );
}

#[test]
fn cancelling_the_cadence_prompt_leaves_autopilot_off() {
    // The chain's second exit. `Esc` here must say the DIAL did not move — the human pressed
    // `m` to turn autopilot on, and "cadence unchanged" would read as "only a number was
    // dropped". The GOAL it already saved stays saved, which is why this cancel is cheap.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.cadence_s = None;
        job::save(&sp, &l).unwrap();
        let mut reg = Registry::load(&reg_path).unwrap();
        reg.projects[0].cadence_s = None;
        reg.save(&reg_path).unwrap();
        state::write_control(&sp, &state::Control::default()).unwrap();
    }
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    for c in "a fresh goal".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);

    assert!(matches!(app.mode, UiMode::Normal));
    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Standard,
        "cancelling the heartbeat prompt must leave autopilot OFF: {}",
        app.status
    );
    assert!(
        app.status.contains("autopilot") && app.status.contains("Standard"),
        "…and say so: {}",
        app.status
    );
    // The goal it had already written is NOT rolled back — a cancel loses the flip, not work.
    // (The field opened SEEDED with the brief on disk, so the typing APPENDED to it; that
    // seeding is the point of `m`'s prompt, so the concatenation here is the correct result.)
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap().trim(),
        "goala fresh goal"
    );
}

#[test]
fn turning_autopilot_on_clears_a_stale_nudge_watermark() {
    // *"when i switch to autopilot mode, pmd doesn't drive it immediately"* — reported AFTER m41
    // re-armed the park, because there are TWO brakes and m41 released one.
    //
    // `nudged_at_seq` is the engine's "I spoke to this agent and it has not reported back" mark,
    // and `idle_observed` honours it by re-parking through `busy_recheck` — which only gives up
    // after DEFAULT_STALL_BUSY_S (1800s). A session nudged during an EARLIER autopilot stint,
    // switched to Standard before it answered, then switched back, is therefore held for half an
    // hour by a nudge it can no longer answer. Re-arming the park alone just made pmd look sooner
    // and re-park.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        // Nudged at seq 5, and the agent never reported above it: `last_marker_seq <= nudged_at_seq`
        // is exactly the condition `idle_observed` reads as "still working".
        l.nudged_at_seq = Some(5);
        l.last_marker_seq = 5;
        l.run = job::JobRun::Monitoring {
            until: SystemClock.now() + 3600,
        };
        job::save(&sp, &l).unwrap();
    }
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot,
        "precondition: the flip landed ({})",
        app.status
    );
    let l = job::load(&sp).unwrap().unwrap();
    assert!(
        state::read_control(&sp).unwrap().wake_generation > 0,
        "turning autopilot on must request an immediate pmd drive"
    );
    assert_eq!(l.nudged_at_seq, Some(5), "pmtui never rewrites the ledger");
    // The agent's own progress mark is NOT rewritten — that is the anti-replay watermark for
    // `needs-you.json`, and forging it would let an already-disposed marker be disposed twice.
    assert_eq!(
        l.last_marker_seq, 5,
        "the marker watermark is the engine's, not ours"
    );
}

#[test]
fn turning_autopilot_on_cancels_a_pending_check_in() {
    // *"I start with a standard, and later i swap to autopilot. The pmd doesn't send any request
    // to my claude/codex"*. The session was parked on a `Monitoring{until}` an HOUR out (an
    // agent's own `next_check_s` nap, taken while nothing was driving it), and
    // `JobScheduler::tick` returns early until that instant — so the flip changed the dial and
    // pmd would not look at the row again until the old nap expired.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    let far = SystemClock.now() + 3600;
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.run = job::JobRun::Monitoring { until: far };
        job::save(&sp, &l).unwrap();
    }
    let _held = hold_current_daemon(&reg_path, "pm-test");
    let mut app = loop_app(&reg_path);

    // `m`, then answer both prompts (goal is seeded; the cadence is already set here).
    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot,
        "precondition: the flip landed ({})",
        app.status
    );
    assert!(
        state::read_control(&sp).unwrap().wake_generation > 0,
        "pmtui records a wake request for pmd"
    );
    assert_eq!(
        job::load(&sp).unwrap().unwrap().run,
        job::JobRun::Monitoring { until: far },
        "pmtui never rewrites the ledger"
    );
}

#[test]
fn a_cadence_change_restarts_the_timer_instead_of_waiting_out_the_old_one() {
    // *"changing the cadence needs to cancel current check in and start again. for some reason,
    // the agent thinks it need to check in 1 h, i update it to 1m and it doesn't take effect"*.
    // Exactly that: the agent had parked an hour out, and the new interval was only consulted
    // when the OLD park expired.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let far = SystemClock.now() + 3600;
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.run = job::JobRun::Monitoring { until: far };
        job::save(&sp, &l).unwrap();
    }
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;

    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    for c in "1m".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        state::read_control(&sp).unwrap().human_cadence_s,
        Some(60),
        "{}",
        app.status
    );
    assert_eq!(
        job::load(&sp).unwrap().unwrap().run,
        job::JobRun::Monitoring { until: far },
        "pmd applies the request on its next tick"
    );
}

#[test]
fn a_cadence_change_never_disturbs_a_blocked_session() {
    // The one park that must survive: a session waiting on a human answer. Re-arming it would
    // turn a pending decision into a heartbeat and drop the stop the human is looking at.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let sp = ProjectPaths::for_session(&root, "bot");
    let blocked = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1234,
    };
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.run = blocked.clone();
        job::save(&sp, &l).unwrap();
    }
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    for c in "1m".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    let l = job::load(&sp).unwrap().unwrap();
    assert_eq!(
        state::read_control(&sp).unwrap().human_cadence_s,
        Some(60),
        "the dial still moves: {}",
        app.status
    );
    assert_eq!(
        l.run, blocked,
        "a BLOCKED session must keep waiting on its human — only a Monitoring park re-arms"
    );
}

#[test]
fn the_preview_hides_the_schedule_when_nothing_drives_the_row() {
    // *"When i swap the mode to standard from autopilot, i don't want check in or timer in the
    // ui"*. And it was not merely clutter: `next: monitoring · check in 3377s · every 1m`
    // describes a heartbeat that, on Standard, nobody runs — a countdown to an event that will
    // not happen. The posture stays (the agent really did report `monitoring`); the numbers go.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let sp = ProjectPaths::for_session(&root, "bot");
    std::fs::write(
        sp.needs_you(),
        r#"{"state":"monitoring","seq":7,"next_check_s":3600,"status":"napping"}"#,
    )
    .unwrap();
    {
        let mut l = job::load(&sp).unwrap().unwrap();
        l.run = job::JobRun::Monitoring {
            until: SystemClock.now() + 3377,
        };
        l.cadence_s = Some(60);
        job::save(&sp, &l).unwrap();
    }
    let mut app = App::new(reg_path, "pm-test".into());
    app.selected = 0;
    let mut t = Terminal::new(TestBackend::new(140, 24)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&t);

    assert!(
        !screen.contains("check in"),
        "an undriven row must not count down to a check-in: {screen}"
    );
    assert!(
        !screen.contains("next-check"),
        "…nor show the agent's self-scheduled nap as a schedule: {screen}"
    );
    assert!(
        !screen.contains("every 1m"),
        "…nor an interval nothing is counting: {screen}"
    );
    // What it says INSTEAD, on the HEAD meta line: who is driving. The report section no longer adds
    // an "autopilot off — nothing is scheduled" line (user: *"That section should only be used for
    // important status for the user"*) — the head's `autopilot off` / `next: … you drive it` says it.
    assert!(screen.contains("you drive it"), "{screen}");
    assert!(
        !screen.contains("nothing is scheduled"),
        "the report section must not restate the schedule: {screen}"
    );
    // The agent's own reported state survives — that part was never a lie.
    assert!(screen.contains("monitoring"), "{screen}");
}

#[test]
fn the_preview_shows_the_cadence_so_an_adaptive_change_cannot_be_invisible() {
    // Two parties can now move this dial — `c`, and the agent itself via
    // `WakeReport::cadence_s`. A setting that changes behind the human's back and is
    // readable nowhere is the failure this pane exists to prevent, so the effective
    // interval is ON SCREEN. Read from the LEDGER, which is what `job_engine` consults.
    let dir = tempfile::tempdir().unwrap();
    // AUTOPILOT: the cadence is the interval pmd counts, so it is shown for a row pmd drives.
    // The undriven case is `the_preview_hides_the_schedule_when_nothing_drives_the_row`.
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut led = state::read_json::<AgentLoopState>(&paths.pmstate()).unwrap();
    led.cadence_s = Some(5400); // as if the agent had just re-timed itself
    state::write_json_atomic(&paths.pmstate(), &led).unwrap();

    let mut app = App::new(reg_path, "pm-test".into());
    app.selected = 0;
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("every 1h30m"),
        "the preview must show the effective cadence: {screen}"
    );
}

#[test]
fn parse_cadence_accepts_what_a_human_would_type_and_refuses_the_rest() {
    for (input, want) in [
        // A bare number is SECONDS, matching the on-disk field and the create form.
        ("600", 600),
        ("600s", 600),
        ("10m", 600),
        ("1h", 3600),
        ("1h30m", 5400),
        ("2h15m30s", 8130),
        // Case and spaces are noise, not meaning: `1H 30M` is someone typing quickly.
        ("1H 30M", 5400),
        (" 90 ", 90),
    ] {
        assert_eq!(parse_cadence(input), Ok(want), "{input:?}");
    }
    // Every refusal must name the input AND the accepted forms, because "invalid" leaves
    // the human guessing at a syntax the field never showed them.
    for bad in ["", "   ", "abc", "5 min", "1h30", "mh", "1x", "-5"] {
        let e = parse_cadence(bad).expect_err(&format!("{bad:?} should be refused"));
        assert!(!e.is_empty(), "{bad:?} refused with an empty message");
    }
    // Overflow is a refusal-or-saturate question, never a panic or a wrap: `u64::MAX`
    // hours must not become a tiny number the clamp would then happily accept.
    let huge = parse_cadence(&format!("{}h", u64::MAX)).expect("saturates, does not wrap");
    assert!(
        huge >= job_engine::CADENCE_MAX_S,
        "an absurd request must stay absurd so the clamp catches it: {huge}"
    );
}

#[test]
fn c_writes_the_cadence_the_engine_actually_reads_and_says_what_landed() {
    // The HUMAN half of *"i want to have a way to update cadence as well"*.
    //
    // The load-bearing assertion is WHICH FILE moved: `job_engine` reads
    // `ledger.cadence_s`, so a `c` that only updated the registry seed would report
    // success while pmd carried on at the old rhythm.
    let dir = tempfile::tempdir().unwrap();
    // AUTOPILOT ON DISK, not just in the injected view: `c` is autopilot-gated since m40, and
    // the second press below comes after a `refresh()` that rebuilds every row from the
    // registry + config — so a view-only tier would evaporate mid-test.
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot); // seeded at 300s
    let paths = ProjectPaths::for_session(&root, "bot");
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path.clone();

    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    let UiMode::EditingCadence { id, current, .. } = &app.mode else {
        panic!("`c` must open the cadence field, got {:?}", app.mode);
    };
    assert_eq!(id, "bot");
    assert_eq!(
        *current, 300,
        "the field must open on the EFFECTIVE cadence"
    );

    for c in "45m".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal), "{:?}", app.mode);
    assert_eq!(
        state::read_control(&paths).unwrap().human_cadence_s,
        Some(2700),
        "the pmtui-owned control is what moved"
    );
    assert_eq!(
        Registry::load(&reg_path).unwrap().projects[0].cadence_s,
        Some(2700),
        "and the registry seed follows, so a fresh row does not show a stale interval"
    );
    assert!(
        app.status.contains("45m"),
        "the status must name the adopted interval in the same words the field used: {}",
        app.status
    );

    // A MISTYPE keeps the overlay open with the text intact — closing it would make the
    // human retype the whole thing to fix one character.
    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    for c in "5 min".chars() {
        handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let UiMode::EditingCadence { input, .. } = &app.mode else {
        panic!(
            "a parse failure must keep the field open, got {:?}",
            app.mode
        );
    };
    assert_eq!(
        input.as_str(),
        "5 min",
        "the typed text must survive a refusal"
    );
    assert!(
        app.status.contains("10m") || app.status.contains("1h30m"),
        "the refusal must show the accepted forms: {}",
        app.status
    );

    // An EMPTY save keeps the current value, exactly like the goal field. Two
    // identical-looking inline fields must not disagree about what Enter-on-empty means.
    handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.status.contains("kept"), "{}", app.status);
    assert_eq!(
        state::read_control(&paths).unwrap().human_cadence_s,
        Some(2700),
        "an empty save must not touch the control"
    );
}

#[test]
fn c_clamps_to_the_same_bounds_the_agent_gets_and_admits_it() {
    // ONE bound for both routes to this dial. A human able to set 5 seconds while the
    // agent is clamped to 60 would make the harness's own safety argument a formality.
    for (typed, want, must_mention) in [
        ("5", job_engine::CADENCE_MIN_S, "1m"),
        ("48h", job_engine::CADENCE_MAX_S, "24h"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        // Autopilot on disk: `c` is autopilot-gated (the cadence is pmd's heartbeat).
        let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot);
        let paths = ProjectPaths::for_session(&root, "bot");
        let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
        app.registry_path = reg_path;
        handle_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        for c in typed.chars() {
            handle_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            state::read_control(&paths).unwrap().human_cadence_s,
            Some(want),
            "{typed} must be clamped on disk"
        );
        // ADMITS it: a field that silently substitutes a different number reads as having
        // ignored you, which is how a human ends up believing a cadence they never set.
        assert!(
            app.status.contains(must_mention) && app.status.contains("outside"),
            "{typed}: the status must say it was clamped and to what: {}",
            app.status
        );
    }
}

#[test]
fn c_refuses_a_row_pmd_does_not_drive() {
    // The cadence is the heartbeat pmd nudges on, so on a Standard row (which pmd never
    // drives) moving the number is a value nothing reads. `c` refuses and names the key
    // that makes it matter (`m`), rather than half-working.
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal); // Standard by default
    app.begin_cadence_edit();
    assert!(
        matches!(app.mode, UiMode::Normal),
        "`c` opened a field on a row pmd does not drive"
    );
    assert!(
        app.status.contains("autopilot") && app.status.contains("press m first"),
        "{}",
        app.status
    );
    // And the empty list explains itself rather than doing nothing at all.
    let mut app = app_with(vec![], UiMode::Normal);
    app.begin_cadence_edit();
    assert!(app.status.contains("nothing is selected"), "{}", app.status);
}

#[test]
fn the_cadence_overlay_shows_the_current_value_the_forms_and_the_bounds() {
    // The three things a human cannot guess: what it is now, what syntax is accepted, and
    // what range will be honoured. A field missing any of them invites `5 min` and then
    // rejects it, or silently clamps and looks broken.
    let app = app_with(
        vec![agent_loop_view("bot")],
        UiMode::EditingCadence {
            id: "bot".into(),
            root: PathBuf::from("/nonexistent"),
            current: 300,
            input: Field::new(),
            then_autopilot: false,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    for want in [
        "Cadence \u{b7} bot",
        "checks in every 5m (300s)",
        "seconds, or 10m / 1h30m",
        "1m..24h",
        "enter save",
    ] {
        assert!(screen.contains(want), "overlay missing {want:?}: {screen}");
    }
}

#[test]
fn turning_autopilot_on_hands_a_live_chat_back_to_pmd() {
    // User: *"I switch a standard to autopilot, and it says it will take effect next tick, but that
    // never happens. next displays idle"*.
    //
    // `chat_lock::is_active` defers pmd on a merely-ALIVE chat session — "ALIVE => defer, full stop.
    // No age cap, no reap" — and `JobScheduler::drive` returns from that gate BEFORE it touches
    // `run`, so the ledger stays `Idle` and the dial changes nothing, forever. The existing
    // `chat_lock_active_defers_the_initial_launch` pins that half in the engine; this pins the half
    // pmtui owes it. m43 made this the DEFAULT case by starting every Standard session in a REPL.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let chat = session_name("bot", &root);
    // The chat pane is ALIVE and nobody is attached — exactly what a create-then-detach leaves.
    let pane = FakePane::sendable(&chat, IDLE_CLAUDE_PANE);
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = reg_path.clone();
    app.refresh();
    let _held = hold_current_daemon(&reg_path, "pm-test");

    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    let sp = ProjectPaths::for_session(&root, "bot");
    assert_eq!(
        state::read_json::<Config>(&sp.config()).unwrap().autonomy,
        Tier::Autopilot,
        "precondition: the flip landed ({})",
        app.status
    );
    assert_eq!(
        pane.terminated(),
        Vec::<String>::new(),
        "a mode switch must preserve the one terminal: {}",
        app.status
    );
    assert!(
        !sp.chat_lock().exists(),
        "no detached-session marker is needed"
    );
}

#[test]
fn turning_autopilot_on_will_not_kill_a_chat_someone_is_sitting_in() {
    // The one chat that must survive the flip: one a human is ATTACHED to from another terminal.
    // Terminating it would yank a live pane out from under them. pmd then genuinely cannot start
    // until they leave, so the status SAYS so rather than flipping the dial silently and leaving the
    // row at `next: idle` — the very failure this pair of tests exists to prevent.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Standard);
    let chat = session_name("bot", &root);
    let pane = FakePane::sendable(&chat, IDLE_CLAUDE_PANE).with(|p| p.attached = true);
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = reg_path.clone();
    app.refresh();
    let _held = hold_current_daemon(&reg_path, "pm-test");

    handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

    assert!(
        pane.terminated().is_empty(),
        "an ATTACHED chat must not be killed: {:?}",
        pane.terminated()
    );
    assert!(
        app.status.contains("Autopilot"),
        "the mode changes while pmd defers to the attached client: {}",
        app.status
    );
}
