//! Autopilot OFF means OFF (m15): [`pmd_drives_row`] itself, the tier a config
//! that merely parses defaults to, and the sweep-level proof that flipping the
//! per-session tier stops and resumes the driving with no daemon restart — while
//! never killing the session the human is taking over.

use super::*;

// --- autopilot OFF means OFF (m15) -----------------------------------------

/// A pane sitting at a bare prompt (`classify_pane` ⇒ `Idle`), so a DRIVEN session
/// really would nudge it — which is what makes the "no nudge" assertions below
/// non-vacuous.
const IDLE_PANE: &str = "did some work\n❯ ";

/// Seed an agent-loop session at `tier` with a fresh Idle ledger, and return its
/// registry entry plus its `pmloop-` session name.
fn loop_session_at(root: &std::path::Path, id: &str, tier: Tier) -> (ProjectEntry, String) {
    seed_agent_loop_config(root, id, tier);
    crate::job::save(
        &ProjectPaths::for_session(root, id),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    (
        agent_loop_entry(id, root),
        crate::tmux::session_name(id, root),
    )
}

/// Flip the per-session `config.autonomy` on disk, exactly as pmtui's `m`
/// (`cycle_tier`) does — an atomic write to the SAME file the gate reads.
fn flip_tier(root: &std::path::Path, id: &str, tier: Tier) {
    let paths = ProjectPaths::for_session(root, id);
    let mut c: Config = state::read_json(&paths.config()).unwrap();
    c.autonomy = tier;
    state::write_json_atomic(&paths.config(), &c).unwrap();
}

#[test]
fn pmd_drives_row_is_agent_loop_only_and_driving_is_opt_in() {
    // Autopilot drives, Standard does not — the one mode is agent-loop.
    assert!(pmd_drives_row(Mode::AgentLoop, Some(Tier::Autopilot)));
    assert!(!pmd_drives_row(Mode::AgentLoop, Some(Tier::Standard)));
    // DRIVING IS OPT-IN: only an explicit Autopilot drives, so an unreadable tier counts
    // as OFF. This assertion is the reverse of what it used to be, on purpose — the old
    // direction let any config this schema could not parse keep pmd typing into a session
    // the human had switched off, with no key able to stop it, which is what *"why my
    // session with autopilot off receive the prompt"* turned out to be.
    //
    // Of the two ways to be wrong, only one puts keystrokes into a live agent.
    assert!(
        !pmd_drives_row(Mode::AgentLoop, None),
        "an unreadable tier must not be read as permission to drive"
    );
}

#[test]
fn a_config_that_parses_always_has_a_tier_and_it_is_standard_by_default() {
    // The OTHER half of the fix, and the half that closes the trap in practice: the
    // reason a tier came back `None` at all was that `Config::autonomy` had no serde
    // default, so a file merely MISSING the field failed to parse. Everything that parses
    // must now yield a real tier — and it must be the OFF one, because a config that does
    // not ask for hands-off driving has not asked for it.
    for (name, json) in [
        ("no autonomy field", r#"{"step_timeout_s":1800}"#),
        ("empty object", r#"{}"#),
        // A `config.json` written by another tool against its own schema — the exact
        // shape that used to make pmd drive a switched-off session.
        (
            "a foreign schema",
            r#"{"heartbeat":{"interval_s":300},"stuck_threshold":3}"#,
        ),
    ] {
        let c: Config =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{name} must parse now, got {e}"));
        assert_eq!(c.autonomy, Tier::Standard, "{name} must default to OFF");
        assert!(
            !pmd_drives_row(Mode::AgentLoop, Some(c.autonomy)),
            "{name} must not be driven"
        );
    }
    // An EXPLICIT autopilot is still honoured — the default must not swallow the opt-in.
    let c: Config = serde_json::from_str(r#"{"autonomy":"autopilot"}"#).unwrap();
    assert_eq!(c.autonomy, Tier::Autopilot);
    assert!(pmd_drives_row(Mode::AgentLoop, Some(c.autonomy)));
}

#[test]
fn standard_agent_loop_row_is_never_launched_or_nudged() {
    // Autopilot OFF on a fresh agent-loop row: pmd must not launch its `pmloop-`
    // session at all, and must not type a single key into anything.
    let dir = tempfile::tempdir().unwrap();
    let (p, sess) = loop_session_at(dir.path(), "bot", Tier::Standard);

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    // Several sweeps, past the cadence, so a tier-blind heartbeat would certainly
    // have fired by now.
    for t in [1000, 1500, 2000, 4000] {
        clock.set(t);
        daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    }

    assert!(
        driver.launched().is_empty(),
        "autopilot OFF must not launch the session: {:?}",
        driver.launched()
    );
    assert!(
        driver.sent_keys().is_empty(),
        "autopilot OFF must not type into anything: {:?}",
        driver.sent_keys()
    );
    assert!(
        !driver.is_alive(&sess).unwrap(),
        "no {sess} may exist for an undriven row"
    );
    assert_eq!(notif.count(), 0, "and nothing is escalated");
    // The gate skips the whole tick, so the ledger is untouched — still the fresh
    // Idle the create wrote (NOT parked `Monitoring`, which would be pmd claiming a
    // cadence it is not keeping).
    let l = crate::job::load(&ProjectPaths::for_session(dir.path(), "bot"))
        .unwrap()
        .unwrap();
    assert_eq!(l.run, crate::job::JobRun::Idle, "the ledger is left alone");
}

#[test]
fn flipping_autopilot_off_stops_nudging_on_the_next_sweep_and_keeps_the_session() {
    // THE BUG, end to end at the sweep level: `m` off must stop the driving on the
    // NEXT sweep with no daemon restart — and must NOT kill the agent the human is
    // about to take over by hand.
    let dir = tempfile::tempdir().unwrap();
    let (p, sess) = loop_session_at(dir.path(), "bot", Tier::Autopilot);

    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();

    // Sweep 1 launches the persistent session (cold-start grace, no nudge).
    daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    assert!(driver.is_alive(&sess).unwrap(), "precondition: it launched");
    driver.set_tail(&sess, IDLE_PANE);
    // Past the grace, then two sweeps to earn the confirmation gate's two
    // consecutive Idle observations ⇒ one real nudge lands.
    for t in [1010, 1020] {
        clock.set(t);
        daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    }
    let nudges_while_on = driver.sent_keys().len();
    assert!(
        nudges_while_on > 0,
        "precondition: autopilot ON really does nudge (otherwise the assertion \
         below is vacuous)"
    );

    // The human presses `m`. Nothing else changes — same daemon, same runner, same
    // registry; only the per-session config on disk.
    flip_tier(dir.path(), "bot", Tier::Standard);
    for t in [1030, 1400, 2000, 5000] {
        clock.set(t);
        daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    }

    assert_eq!(
        driver.sent_keys().len(),
        nudges_while_on,
        "no further nudge may arrive after autopilot is turned off: {:?}",
        driver.sent_keys()
    );
    assert!(
        driver.is_alive(&sess).unwrap(),
        "turning autopilot off must NOT kill the live session — the human keeps it \
         and drives it by hand"
    );

    // …and flipping it back ON resumes nudging on the next sweeps (proving the gate
    // is a live read of the config, not a one-way latch).
    //
    // The agent REPORTS first: since m38 the harness will not nudge again until the outstanding
    // one has been answered (`job_engine`'s awaiting-report gate), so without this the resumed
    // driving would correctly decline to type and this assertion would fail for the wrong
    // reason. Backdated so the mid-write grace treats the marker as settled.
    {
        let paths = ProjectPaths::for_session(dir.path(), "bot");
        std::fs::write(
            paths.needs_you(),
            r#"{"seq":101,"state":"working","status":"still going"}"#,
        )
        .unwrap();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(paths.needs_you())
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(30))
            .unwrap();
    }
    flip_tier(dir.path(), "bot", Tier::Autopilot);
    for t in [6000, 6010] {
        clock.set(t);
        daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    }
    assert!(
        driver.sent_keys().len() > nudges_while_on,
        "flipping autopilot back on must resume nudging with no restart: {:?}",
        driver.sent_keys()
    );
}

#[test]
fn an_unreadable_config_stops_the_typing_and_says_so_once() {
    // The polarity, wired end to end. A config the gate cannot read is treated as
    // autopilot OFF: pmd stops typing, does NOT kill the live session, and does not
    // poison the row — but it must not go SILENT either, because silence is how a
    // half-created or corrupted session becomes invisible.
    //
    // This asserts the opposite of the test it replaced. That one kept the row driven so
    // a malformed config would surface via the poison path; the trouble is that the same
    // polarity also kept pmd typing into a session the human had deliberately switched
    // off, and `cycle_tier` refuses to write a flip it cannot read — so there was no way
    // out from the dashboard. Stopping and naming it beats driving and escalating.
    let dir = tempfile::tempdir().unwrap();
    let (p, sess) = loop_session_at(dir.path(), "bot", Tier::Autopilot);
    let driver = FakeDriver::new();
    let clock = FakeClock::new(1000);
    let notif = CaptureNotifier::new();
    let mut daemon = Daemon::new();
    daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    assert!(driver.is_alive(&sess).unwrap(), "precondition: it launched");
    let sent_while_readable = driver.sent_keys().len();

    // Corrupt the config the gate reads. Malformed JSON, not a missing field: a missing
    // field now DEFAULTS (see the test above), so this is the only shape that still
    // reaches `None`.
    std::fs::write(
        ProjectPaths::for_session(dir.path(), "bot").config(),
        "{ not json",
    )
    .unwrap();
    for _ in 0..POISON_THRESHOLD + 2 {
        clock.set(clock.now() + 10);
        daemon.sweep(&reg(vec![p.clone()]), &driver, &clock, &notif);
    }

    assert_eq!(
        driver.sent_keys().len(),
        sent_while_readable,
        "an unreadable tier must stop the typing: {:?}",
        driver.sent_keys()
    );
    assert!(
        !daemon.is_poisoned("bot"),
        "not driving is not a failure — the row must not be poisoned for it"
    );
    assert!(
        driver.is_alive(&sess).unwrap(),
        "the human keeps their agent; skipping a row never terminates anything"
    );
    assert_eq!(
        notif.count(),
        0,
        "an undriven row raises no escalation — the operator-facing note is a log line"
    );
    // WARN ONCE, not once per 500ms sweep. The `HashSet` is what makes that true, and its
    // size is the only observable proof of it from here: five sweeps, one entry.
    assert_eq!(
        daemon.warned_no_tier.len(),
        1,
        "an unreadable tier must be named exactly once, not once per sweep"
    );
}
