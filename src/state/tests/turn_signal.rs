//! Reading the turn-end signal's health — the one rule `pmd doctor` reports and the report-debt
//! backstop acts on.
//!
//! The whole value of this classifier is that it REFUSES to diagnose what it cannot prove: a fresh
//! session and a dead hook look identical from the file alone, and only the report count separates
//! them. Most of what is pinned here is therefore the absence of a finding.

use std::fs;

use tempfile::tempdir;

use crate::state::{TURN_SIGNAL_LAG_MAX, TurnSignalHealth, turn_signal_health};

/// A missing signal with few reports is a NEW session, not a broken hook. Pinned at the boundary
/// itself, because one off-by-one here turns every young session into a false alarm.
#[test]
fn no_signal_and_few_reports_is_a_fresh_session() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("turn-complete");

    assert_eq!(turn_signal_health(&path, 0), TurnSignalHealth::Fresh);
    assert_eq!(
        turn_signal_health(&path, TURN_SIGNAL_LAG_MAX),
        TurnSignalHealth::Fresh,
        "the lag allowance is inclusive"
    );
    assert!(
        !turn_signal_health(&path, TURN_SIGNAL_LAG_MAX).is_blind(),
        "and a fresh session is never reported as blinding pmd"
    );
}

/// ONE REPORT PAST THE ALLOWANCE is the first point at which a missing file means something: the
/// worker cannot have reported more times than it finished turns.
#[test]
fn no_signal_with_many_reports_is_a_hook_that_never_fired() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("turn-complete");

    let health = turn_signal_health(&path, TURN_SIGNAL_LAG_MAX + 1);
    assert_eq!(
        health,
        TurnSignalHealth::NeverFired {
            reports: TURN_SIGNAL_LAG_MAX + 1
        }
    );
    assert!(health.is_blind());
}

/// A SIGNAL IN STEP WITH THE REPORTS is healthy, and stays healthy across the allowance — a turn ends
/// before its notification lands, so a small lead is the ordinary race, not a fault.
#[test]
fn a_signal_keeping_pace_with_the_reports_is_healthy() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("turn-complete");
    fs::write(&path, vec![b'x'; 10]).unwrap();

    assert_eq!(
        turn_signal_health(&path, 10),
        TurnSignalHealth::Healthy { turns: 10 }
    );
    assert_eq!(
        turn_signal_health(&path, 10 + TURN_SIGNAL_LAG_MAX),
        TurnSignalHealth::Healthy { turns: 10 },
        "a lead within the allowance is still healthy"
    );
    assert!(!turn_signal_health(&path, 10).is_blind());
}

/// MORE REPORTS THAN TURNS BY MORE THAN THE ALLOWANCE is the intermittent hook measured in the wild:
/// `turn-complete` advanced once across roughly seven accepted reports on a live session.
#[test]
fn a_signal_far_behind_the_reports_is_an_intermittent_hook() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("turn-complete");
    fs::write(&path, b"x").unwrap();

    let health = turn_signal_health(&path, 7);
    assert_eq!(
        health,
        TurnSignalHealth::Partial {
            turns: 1,
            reports: 7
        }
    );
    assert!(health.is_blind());
}

/// A SIGNAL AHEAD OF THE REPORTS IS NOT A FAULT. The worker reports at its own decision points, not
/// once per turn, so turns legitimately outrun reports by any margin — and the subtraction must not
/// wrap into a false `Partial` when it does.
#[test]
fn more_turns_than_reports_is_healthy_rather_than_underflowing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("turn-complete");
    fs::write(&path, vec![b'x'; 500]).unwrap();

    assert_eq!(
        turn_signal_health(&path, 0),
        TurnSignalHealth::Healthy { turns: 500 }
    );
}

/// AN ODD INODE IN PLACE OF THE SIGNAL NEVER PANICS: a tick must not fail over a diagnostic.
#[test]
fn a_directory_in_place_of_the_signal_is_classified_without_panicking() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("turn-complete");
    fs::create_dir(&path).unwrap();

    let health = turn_signal_health(&path, 0);
    assert!(
        !health.is_blind(),
        "no reports means no finding, whatever the inode is: {health:?}"
    );
}
