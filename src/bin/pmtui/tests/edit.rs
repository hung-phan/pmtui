//! Handing the brief to `$EDITOR`: the PLAIN-text seed it is opened with, the buffer coming
//! back verbatim (a `#` line is content now, not a comment), the atomic write that leaves no
//! temp file behind, and the ledger a goal edit never touches.

use super::*;

#[test]
fn brief_from_editor_buffer_keeps_everything_and_trims_outer() {
    // Plain text editor now: NOTHING is stripped — a `#` line is content — and only the
    // surrounding whitespace/blank lines are trimmed. User: *"it should be a simple text editor.
    // the goal should have all the string user wants to put in"*.
    let raw = "\n\n  # a real heading\nShip the widget.\n\t# a note\n\n";
    assert_eq!(
        brief_from_editor_buffer(raw),
        "# a real heading\nShip the widget.\n\t# a note"
    );
}

#[test]
fn brief_from_editor_buffer_preserves_multiline_body_including_hashes() {
    // A multi-paragraph brief keeps its interior newlines/blank lines AND its `#` lines; only
    // the ends are trimmed.
    let raw = "\
Goal: land the feature.
# a heading the user typed on purpose

Constraints: no downtime.
Context: see RFC-12.
";
    let out = brief_from_editor_buffer(raw);
    assert_eq!(
        out,
        "Goal: land the feature.\n# a heading the user typed on purpose\n\nConstraints: no downtime.\nContext: see RFC-12."
    );
    assert!(
        out.contains("# a heading"),
        "a # line is kept, not stripped: {out}"
    );
}

#[test]
fn brief_from_editor_buffer_empty_only_when_blank() {
    // Whitespace-only still parses to empty, which run() treats as "keep the previous goal".
    // But a buffer of `#` lines is real content now, NOT empty.
    assert!(brief_from_editor_buffer("   \n\n\t\n").is_empty());
    assert_eq!(brief_from_editor_buffer("# just this\n"), "# just this");
}

#[test]
fn brief_editor_seed_is_plain_text_with_no_guidance() {
    // The seed is JUST the current goal (plus a trailing newline) — no `#`-comment guidance
    // that would round-trip back into the goal.
    let seed = brief_editor_seed("watch the deploy channel");
    assert_eq!(seed, "watch the deploy channel\n");
    assert!(!seed.contains('#'), "no guidance seeded: {seed}");
    assert_eq!(
        brief_from_editor_buffer(&seed),
        "watch the deploy channel",
        "seed round-trips back to just the goal"
    );
    // Empty goal → empty buffer, which parses back to empty ⇒ keep the previous goal.
    assert_eq!(brief_editor_seed(""), "");
    assert!(brief_from_editor_buffer("").is_empty());
}

#[test]
fn brief_write_is_atomic_and_leaves_no_temp_file() {
    // The bug this feature had to fix first: pmtui used to write `brief.md` with a
    // plain (truncate-then-fill) `fs::write` while `JobScheduler::nudge` re-reads it
    // every heartbeat via `unwrap_or_default()` — so a nudge landing mid-write saw an
    // empty brief and the agent silently lost its mandate. Every write now goes
    // through `state::write_text_atomic` (temp in the same dir + rename), which means
    // (a) the target holds the WHOLE content, and (b) no temp file is left behind for
    // the next reader/lister to trip over.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    let long = "ship the thing\n\nconstraints:\n- no flags\n- keep tests green\n".repeat(64);
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        &long,
        Some(300),
        1000,
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap(),
        long,
        "the brief holds the full content, byte for byte"
    );

    // A second, larger write over the top (the `g` path) must also land whole.
    let replaced = format!("{long}\nand also: re-aimed mid-flight\n");
    assert_eq!(
        apply_goal_edit(&sp.brief(), &replaced).unwrap(),
        GoalEdit::Written {
            lines: replaced.lines().count()
        }
    );
    assert_eq!(std::fs::read_to_string(sp.brief()).unwrap(), replaced);

    // No stray `.brief.md.tmp.*` (or any other temp) survives in the directory.
    let strays: Vec<String> = std::fs::read_dir(sp.state_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(strays.is_empty(), "temp files left behind: {strays:?}");
}

#[test]
fn goal_edit_rewrites_brief_and_never_touches_the_ledger() {
    // THE INVARIANT: pmtui must NEVER write `state.json`. A goal edit rewrites
    // `brief.md` and nothing else — no budget reset, no clearing of a Blocked park
    // (the human's `a` answer path is what resumes that), no ledger write at all.
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "old goal",
        Some(300),
        1000,
    )
    .unwrap();
    job::save(&sp, &AgentLoopState::fresh(Engine::Claude, Some(300), 1000)).unwrap();

    // Park the session Blocked so we would NOTICE an accidental un-blocking write.
    let mut l = job::load(&sp).unwrap().unwrap();
    l.run = job::JobRun::Blocked {
        stop_ids: vec!["stop-1".into()],
        since: 1000,
    };
    l.open_stops = vec![open_stop("stop-1", pmstate::StopKind::Ambiguity)];
    job::save(&sp, &l).unwrap();

    let ledger_before = std::fs::read(sp.pmstate()).unwrap();
    let mtime_before = std::fs::metadata(sp.pmstate()).unwrap().modified().unwrap();
    let cfg_before = std::fs::read(sp.config()).unwrap();

    assert_eq!(
        apply_goal_edit(&sp.brief(), "a brand new goal").unwrap(),
        GoalEdit::Written { lines: 1 }
    );

    assert_eq!(
        std::fs::read_to_string(sp.brief()).unwrap(),
        "a brand new goal",
        "the goal the next nudge will read"
    );
    assert_eq!(
        std::fs::read(sp.pmstate()).unwrap(),
        ledger_before,
        "state.json bytes must be untouched — pmtui is never a ledger writer"
    );
    assert_eq!(
        std::fs::metadata(sp.pmstate()).unwrap().modified().unwrap(),
        mtime_before,
        "state.json was not even re-written with identical bytes"
    );
    assert_eq!(
        std::fs::read(sp.config()).unwrap(),
        cfg_before,
        "config.json (the tier dial) is not collateral either"
    );
    // The park itself is intact: still Blocked, still holding its open stop.
    let after = job::load(&sp).unwrap().unwrap();
    assert!(
        matches!(after.run, job::JobRun::Blocked { .. }),
        "a goal edit must not un-block a parked session"
    );
    assert_eq!(after.open_stops.len(), 1, "the open stop still awaits `a`");
}

#[test]
fn goal_edit_unchanged_writes_nothing() {
    // Saving the buffer untouched (modulo the trailing newline an editor may add)
    // is a no-op: no write at all, so a concurrent nudge cannot even observe a
    // rename. run() reports "goal unchanged".
    let dir = tempfile::tempdir().unwrap();
    let sp = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &sp,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "same goal",
        Some(300),
        1000,
    )
    .unwrap();
    let mtime_before = std::fs::metadata(sp.brief()).unwrap().modified().unwrap();

    assert_eq!(
        apply_goal_edit(&sp.brief(), "same goal\n").unwrap(),
        GoalEdit::Unchanged,
        "whitespace-only differences are not a change"
    );
    assert_eq!(
        std::fs::metadata(sp.brief()).unwrap().modified().unwrap(),
        mtime_before,
        "an unchanged goal leaves brief.md untouched"
    );
}

#[test]
fn g_opens_the_field_and_the_editor_chord_hands_the_brief_to_the_editor() {
    // The user's shape, end to end: `g` opens the inline field seeded from the file
    // the loop actually reads, and `^E` from INSIDE it queues the editor against that
    // same per-session `brief.md`. There is no second key and no second code path.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "bot", Tier::Autopilot); // seeds brief "goal"
    let sp = ProjectPaths::for_session(&root, "bot");

    let mut app = loop_app(&reg_path);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    match &app.mode {
        UiMode::EditingGoal {
            id, brief, current, ..
        } => {
            assert_eq!(id, "bot");
            assert_eq!(brief, &sp.brief(), "targets the per-session brief.md");
            assert_eq!(current, "goal", "seeded from what is on disk");
        }
        other => panic!("`g` should open the inline field, got {other:?}"),
    }
    assert!(
        app.pending_brief_edit.is_none(),
        "`g` must not reach $EDITOR on its own any more"
    );

    press_editor_chord(&mut app);
    let req = app
        .pending_brief_edit
        .as_ref()
        .expect("^X^E inside the field queues the editor");
    assert_eq!(req.goal, "goal", "seeded with the brief that is on disk");
    match &req.target {
        BriefEditTarget::Session {
            id,
            brief,
            then_autopilot,
        } => {
            assert_eq!(id, "bot");
            assert_eq!(brief, &sp.brief());
            // `g`'s field is not `m`'s: escalating it must not carry a tier flip.
            assert!(!then_autopilot);
        }
        BriefEditTarget::CreateForm => panic!("^E must target the live session"),
    }
    // The overlay closes first: `run()` restores the terminal into whatever mode is
    // current when the editor returns.
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn g_on_a_row_with_no_registry_entry_explains_itself() {
    // A selected row whose registry entry is gone has no brief path to write, so `g`
    // refuses with an explanatory status (the `cycle_tier` dead-end style) instead of
    // a bare return — and queues nothing, and does not panic.
    let mut app = app_with(vec![autopilot_loop_view("bot")], UiMode::Normal);
    app.registry_path = PathBuf::from("/nonexistent/registry.json");
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        app.status.contains("bot") && app.status.contains("gone from the list"),
        "explanatory refusal, got {:?}",
        app.status
    );
    assert!(
        app.pending_brief_edit.is_none(),
        "nothing queued for a row with no brief path"
    );

    // Nothing selected at all is the other dead end: also a status, also no panic.
    let mut app = app_with(vec![], UiMode::Normal);
    handle_key(&mut app, KeyCode::Char('g'), KeyModifiers::NONE);
    assert!(
        app.status.contains("nothing is selected"),
        "empty-list refusal, got {:?}",
        app.status
    );
    assert!(app.pending_brief_edit.is_none());
}

#[test]
fn directive_edit_round_trips_and_rescind_is_idempotent() {
    assert_eq!(
        directive_from_editor_buffer("\n# keep this\nno deletes\n"),
        "# keep this\nno deletes"
    );
    assert_eq!(directive_editor_seed("no deletes"), "no deletes\n");
    assert_eq!(directive_editor_seed(" \n"), "");

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &paths,
        Tier::Autopilot,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        0,
    )
    .unwrap();

    assert_eq!(
        apply_directive_edit(&paths.directive(), "no deletes\nwithout approval").unwrap(),
        DirectiveEdit::Written { lines: 2 }
    );
    assert_eq!(
        apply_directive_edit(&paths.directive(), "\nno deletes\nwithout approval\n").unwrap(),
        DirectiveEdit::Unchanged
    );
    assert!(rescind_directive(&paths.directive()).unwrap());
    assert!(!rescind_directive(&paths.directive()).unwrap());
}

#[test]
fn edit_file_errors_name_the_target_path() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.brief()).unwrap();
    std::fs::create_dir_all(paths.directive()).unwrap();

    let goal_error = apply_goal_edit(&paths.brief(), "new goal")
        .unwrap_err()
        .to_string();
    assert!(goal_error.contains(&paths.brief().display().to_string()));

    let directive_error = apply_directive_edit(&paths.directive(), "new directive")
        .unwrap_err()
        .to_string();
    assert!(directive_error.contains(&paths.directive().display().to_string()));

    let rescind_error = rescind_directive(&paths.directive())
        .unwrap_err()
        .to_string();
    assert!(rescind_error.contains(&paths.directive().display().to_string()));
}

#[test]
fn cadence_parser_accepts_composites_and_explains_invalid_input() {
    for (input, expected) in [
        ("90", 90),
        ("90s", 90),
        ("1H 30M", 5_400),
        ("2h15m30s", 8_130),
    ] {
        assert_eq!(parse_cadence(input), Ok(expected));
    }

    for input in ["", "1x", "mh", "1h30"] {
        assert!(!parse_cadence(input).unwrap_err().is_empty());
    }
    assert_eq!(parse_cadence(&format!("{}h", u64::MAX)).unwrap(), u64::MAX);
}

#[test]
fn start_driving_increments_only_the_wake_request() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &paths,
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(450),
        0,
    )
    .unwrap();

    start_driving_now(&paths).unwrap();
    start_driving_now(&paths).unwrap();

    assert_eq!(
        state::read_control(&paths).unwrap(),
        state::Control {
            human_cadence_s: Some(450),
            wake_generation: 2,
        }
    );
}

#[test]
fn start_driving_reports_an_unreadable_control_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(paths.control(), "{not json").unwrap();

    let error = start_driving_now(&paths).unwrap_err().to_string();
    assert!(error.contains("read"), "{error}");
    assert!(
        error.contains(&paths.control().display().to_string()),
        "{error}"
    );
}

#[test]
fn cadence_edit_updates_control_and_registry_then_becomes_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    seed_agent_loop(
        &paths,
        Tier::Autopilot,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        0,
    )
    .unwrap();
    let registry_path = dir.path().join("registry.json");
    let mut registry = Registry::default();
    registry.projects.push(ProjectEntry {
        id: "bot".into(),
        display_name: None,
        root: dir.path().to_path_buf(),
        enabled: true,
        mode: Mode::AgentLoop,
        engine: Some(Engine::Claude),
        worker_model: None,
        initial_prompt: None,
        task_title: None,
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s: Some(300),
    });
    registry.save(&registry_path).unwrap();

    match apply_cadence_edit(&paths, &registry_path, "bot", 0).unwrap() {
        CadenceEdit::Written { secs, clamped } => {
            assert_eq!(secs, job_engine::CADENCE_MIN_S);
            assert!(clamped);
        }
        CadenceEdit::Unchanged(_) => panic!("the cadence changed"),
    }
    assert_eq!(
        state::read_control(&paths).unwrap().human_cadence_s,
        Some(job_engine::CADENCE_MIN_S)
    );
    assert_eq!(
        Registry::load(&registry_path).unwrap().projects[0].cadence_s,
        Some(job_engine::CADENCE_MIN_S)
    );
    assert!(matches!(
        apply_cadence_edit(
            &paths,
            &registry_path,
            "bot",
            job_engine::CADENCE_MIN_S
        )
        .unwrap(),
        CadenceEdit::Unchanged(secs) if secs == job_engine::CADENCE_MIN_S
    ));
}

#[test]
fn cadence_edit_reports_an_unreadable_control_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(paths.control(), "{not json").unwrap();

    let error = match apply_cadence_edit(&paths, &dir.path().join("registry.json"), "bot", 300) {
        Ok(_) => panic!("an unreadable control file must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("read"), "{error}");
    assert!(
        error.contains(&paths.control().display().to_string()),
        "{error}"
    );
}

const CONTROL_WRITE_FAILURE_PROBE: &str = "PM_TEST_CONTROL_WRITE_FAILURE_PROBE";

fn run_control_write_failure_probe() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    std::fs::write(
        paths.control(),
        r#"{"human_cadence_s":300,"wake_generation":0}"#,
    )
    .unwrap();

    for nonce in 0..=1 {
        let blocker = paths
            .state_dir()
            .join(format!(".control.json.tmp.{}.{nonce}", std::process::id()));
        std::fs::create_dir(blocker).unwrap();
    }

    let driving_error = start_driving_now(&paths).unwrap_err().to_string();
    let cadence_error =
        match apply_cadence_edit(&paths, &dir.path().join("registry.json"), "bot", 600) {
            Ok(_) => panic!("a blocked atomic replacement must fail"),
            Err(error) => error.to_string(),
        };

    for error in [&driving_error, &cadence_error] {
        assert!(error.contains("write"), "{error}");
        assert!(
            error.contains(&paths.control().display().to_string()),
            "{error}"
        );
    }
    assert_eq!(
        state::read_control(&paths).unwrap(),
        state::Control {
            human_cadence_s: Some(300),
            wake_generation: 0,
        },
        "failed replacements leave the original control request intact"
    );
}

#[test]
fn control_write_failures_are_reported_by_both_edit_paths() {
    if std::env::var_os(CONTROL_WRITE_FAILURE_PROBE).is_some() {
        run_control_write_failure_probe();
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::edit::control_write_failures_are_reported_by_both_edit_paths",
            "--nocapture",
        ])
        .env(CONTROL_WRITE_FAILURE_PROBE, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "control-write probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
