//! The NO-PROGRESS circuit breaker (slice 1): the byte-pure evaluator, the `PM_NO_PROGRESS`
//! kill-switch parser, the git working-tree fingerprint, and the end-to-end escalation on a
//! real git `work_dir` — plus its mirror-image control (a tree that keeps changing never
//! escalates), which is what proves the escalate test isn't passing for the wrong reason.

use super::*;
use std::process::Command;

// --- the byte-pure evaluator ---------------------------------------------

#[test]
fn disabled_threshold_never_escalates() {
    // threshold 0 = off: an unchanged tree across many steps never escalates.
    let (mut streak, mut prev) = (0u32, None);
    for _ in 0..10 {
        let step = step_no_progress(prev, Some(42), streak, 0);
        assert!(!step.escalate);
        streak = step.streak;
        prev = step.fp;
    }
}

#[test]
fn an_unobservable_tree_is_inert() {
    // cur == None (not a git repo / git error) ⇒ forget the baseline, never escalate.
    let step = step_no_progress(Some(1), None, 5, 2);
    assert_eq!(
        step,
        ProgressStep {
            streak: 0,
            fp: None,
            escalate: false
        }
    );
}

#[test]
fn the_first_observation_only_baselines() {
    let step = step_no_progress(None, Some(7), 0, 2);
    assert_eq!(
        step,
        ProgressStep {
            streak: 0,
            fp: Some(7),
            escalate: false
        }
    );
}

#[test]
fn an_unchanged_tree_climbs_then_escalates_at_the_threshold() {
    let s1 = step_no_progress(None, Some(9), 0, 2); // baseline
    assert_eq!((s1.streak, s1.escalate), (0, false));
    let s2 = step_no_progress(s1.fp, Some(9), s1.streak, 2); // 1st unchanged interval
    assert_eq!((s2.streak, s2.escalate), (1, false));
    let s3 = step_no_progress(s2.fp, Some(9), s2.streak, 2); // 2nd ⇒ threshold ⇒ escalate
    assert_eq!((s3.streak, s3.escalate), (2, true));
}

#[test]
fn a_changed_tree_resets_the_run() {
    let s1 = step_no_progress(Some(9), Some(9), 1, 3); // unchanged: streak 2
    assert_eq!((s1.streak, s1.escalate), (2, false));
    let s2 = step_no_progress(s1.fp, Some(10), s1.streak, 3); // changed ⇒ reset
    assert_eq!(
        s2,
        ProgressStep {
            streak: 0,
            fp: Some(10),
            escalate: false
        }
    );
}

#[test]
fn the_env_kill_switch_parses() {
    assert_eq!(
        no_progress_threshold_from_env(None),
        DEFAULT_NO_PROGRESS_NUDGES
    );
    for off in ["off", "OFF", "0", "false", "No"] {
        assert_eq!(
            no_progress_threshold_from_env(Some(off)),
            0,
            "{off} disables"
        );
    }
    assert_eq!(no_progress_threshold_from_env(Some("5")), 5);
    assert_eq!(no_progress_threshold_from_env(Some(" 7 ")), 7, "trimmed");
    assert_eq!(
        no_progress_threshold_from_env(Some("garbage")),
        DEFAULT_NO_PROGRESS_NUDGES,
        "unrecognised text falls back to the default (feature on)"
    );
}

// --- the git working-tree fingerprint ------------------------------------

/// `git init` a repo at `dir` with one commit. Returns `false` if git is unavailable, so the
/// git-dependent tests skip cleanly on a box without it (CI has git). Global/system config is
/// neutralised so a developer's own git settings (signing, hooks, templates) can't interfere.
fn git_init_repo(dir: &Path) -> bool {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init", "-q"]) {
        return false;
    }
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(dir.join("README.md"), "hi\n").unwrap();
    git(&["add", "-A"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "init"])
}

#[test]
fn tree_fingerprint_is_none_off_a_repo_and_stable_until_the_tree_changes() {
    let dir = tempfile::tempdir().unwrap();
    // Not a git tree ⇒ None (the detector is inert — the fail-safe direction).
    assert_eq!(
        tree_fingerprint(dir.path()),
        None,
        "a non-git dir yields no fingerprint"
    );
    if !git_init_repo(dir.path()) {
        return; // git unavailable — skip
    }
    let a = tree_fingerprint(dir.path()).expect("a git repo yields a fingerprint");
    let b = tree_fingerprint(dir.path()).expect("still a repo");
    assert_eq!(a, b, "an unchanged tree fingerprints identically");

    // A new source file is real progress ⇒ the fingerprint changes.
    std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
    let c = tree_fingerprint(dir.path()).expect("still a repo");
    assert_ne!(a, c, "a new file changes the fingerprint");

    // Writing under `.project-state/` is HARNESS bookkeeping, not agent output ⇒ excluded.
    std::fs::create_dir_all(dir.path().join(".project-state/sessions/x")).unwrap();
    std::fs::write(
        dir.path().join(".project-state/sessions/x/state.json"),
        "{}",
    )
    .unwrap();
    let d = tree_fingerprint(dir.path()).expect("still a repo");
    assert_eq!(c, d, ".project-state writes must not count as progress");
}

// --- end-to-end through the scheduler ------------------------------------

#[test]
fn a_stagnant_tree_escalates_instead_of_nudging_forever() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(50));
    if !git_init_repo(&fx.sched.work_dir) {
        return; // git unavailable — skip
    }
    fx.sched.no_progress_threshold = 2;
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch → Monitoring{START+8}
    fx.driver.set_tail(&sess, IDLE_PANE);

    // Nudge 1 baselines the tree (streak 0).
    fx.clock.set(START + LAUNCH_GRACE_S);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.sent_keys().len(), 1);

    // Nudge 2: the tree is unchanged (only `.project-state` churned) ⇒ streak 1 (< 2), still
    // nudges. The agent reports so the m38 awaiting-report gate opens for a second nudge.
    report_progress(&fx, 101, 40);
    fx.clock.set(START + LAUNCH_GRACE_S + BUSY_RECHECK_S + 50);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    ));
    assert_eq!(fx.driver.sent_keys().len(), 2);

    // Third due tick: STILL unchanged ⇒ streak 2 == threshold ⇒ escalate, no third nudge.
    report_progress(&fx, 102, 30);
    fx.clock
        .set(START + LAUNCH_GRACE_S + 2 * BUSY_RECHECK_S + 100);
    let out = tick_confirmed(&mut fx);
    match out {
        JobTick::Stuck(reason) => assert!(
            reason.contains("has not changed"),
            "the Stuck reason names the no-progress cause: {reason}"
        ),
        other => panic!("a stagnant tree must escalate, got {other:?}"),
    }
    assert_eq!(
        fx.driver.sent_keys().len(),
        2,
        "the backstop stops the nudges"
    );
    assert!(matches!(fx.sched.run, JobRun::Blocked { .. }));
    assert_eq!(ledger(&fx).open_stops[0].kind, StopKind::Stuck);
}

#[test]
fn a_tree_that_keeps_changing_never_escalates() {
    // The mirror image of the test above, same timing — but the agent produces a new file
    // before each nudge, so the fingerprint changes every interval, the streak keeps resetting,
    // and the tick that escalated above now nudges. This is what proves the escalation above is
    // caused by no-progress, not by the cadence rhythm.
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(50));
    if !git_init_repo(&fx.sched.work_dir) {
        return; // git unavailable — skip
    }
    fx.sched.no_progress_threshold = 2;
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // launch
    fx.driver.set_tail(&sess, IDLE_PANE);

    fx.clock.set(START + LAUNCH_GRACE_S);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    )); // nudge 1

    // Real progress before nudge 2.
    std::fs::write(fx.sched.work_dir.join("a.rs"), "1").unwrap();
    report_progress(&fx, 101, 40);
    fx.clock.set(START + LAUNCH_GRACE_S + BUSY_RECHECK_S + 50);
    assert!(matches!(
        tick_confirmed(&mut fx),
        JobTick::Monitoring { .. }
    )); // nudge 2

    // Real progress before the tick that escalated in the stagnant case.
    std::fs::write(fx.sched.work_dir.join("b.rs"), "2").unwrap();
    report_progress(&fx, 102, 30);
    fx.clock
        .set(START + LAUNCH_GRACE_S + 2 * BUSY_RECHECK_S + 100);
    let out = tick_confirmed(&mut fx);
    assert!(
        matches!(out, JobTick::Monitoring { .. }),
        "a changing tree must not escalate, got {out:?}"
    );
    assert_eq!(
        fx.driver.sent_keys().len(),
        3,
        "all three nudges landed — no escalation"
    );
}
