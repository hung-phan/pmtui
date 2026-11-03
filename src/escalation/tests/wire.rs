//! The program-based wires: the argv each one builds, and the fact that adding one
//! changes nothing about delivery. Pure — no notification server, no relay.

use super::*;
use crate::escalation::transport::{DEAD_AFTER, Wire, argv};

fn escalation(severity: Severity) -> Escalation {
    Escalation {
        project: "agent-manager".into(),
        title: "1 decision needs you".into(),
        body: "Ship in the current window, or hold for the morning review?".into(),
        severity,
    }
}

/// `notify-send` has two text arguments, so title and body fold into one.
#[test]
fn the_notify_send_argv_folds_title_and_body() {
    assert_eq!(
        argv(Wire::NotifySend, &escalation(Severity::Urgent)),
        [
            "-u",
            "critical",
            "pmd · agent-manager",
            "1 decision needs you\nShip in the current window, or hold for the morning review?",
        ]
    );
}

/// cmux has three, so they stay apart. The folding difference is the whole reason
/// `argv` is a function rather than a hardcoded argv, so the two are pinned together.
#[test]
fn the_cmux_argv_keeps_the_three_pieces_apart() {
    assert_eq!(
        argv(Wire::Cmux, &escalation(Severity::Warn)),
        [
            "notify",
            "--title",
            "pmtui · agent-manager",
            "--subtitle",
            "1 decision needs you",
            "--body",
            "Ship in the current window, or hold for the morning review?",
        ]
    );
}

/// Only `notify-send` has an urgency argument. cmux carries severity nowhere, so it
/// must not appear to — severity survives as the min-severity gate instead.
#[test]
fn severity_reaches_the_wire_only_where_the_wire_has_a_slot_for_it() {
    let send_urgent = argv(Wire::NotifySend, &escalation(Severity::Urgent));
    let send_info = argv(Wire::NotifySend, &escalation(Severity::Info));
    assert_ne!(send_urgent, send_info);

    let cmux_urgent = argv(Wire::Cmux, &escalation(Severity::Urgent));
    let cmux_info = argv(Wire::Cmux, &escalation(Severity::Info));
    assert_eq!(cmux_urgent, cmux_info);
}

#[test]
fn each_wire_names_the_program_it_speaks_to() {
    assert_eq!(Wire::NotifySend.program(), "notify-send");
    assert_eq!(Wire::Cmux.program(), "cmux");
}

/// The default wire is the one that shipped, so no existing caller changed behavior
/// when the enum arrived.
#[test]
fn the_default_wire_is_notify_send() {
    assert_eq!(Wire::default(), Wire::NotifySend);
    assert_eq!(ProgramNotifier::default().wire, Wire::NotifySend);
    assert_eq!(ProgramNotifier::default().bin, "notify-send");
    assert_eq!(
        ProgramNotifier::with_bin("elsewhere", Severity::Warn, Wire::default()).wire,
        Wire::NotifySend
    );
}

#[test]
fn a_wired_notifier_takes_its_binary_from_its_wire() {
    let n = ProgramNotifier::wired(Wire::Cmux, Severity::Warn);
    assert_eq!(n.bin, "cmux");
    assert_eq!(n.wire, Wire::Cmux);
    assert_eq!(n.min_severity, Severity::Warn);
}

/// Every wire shares the gate, the latch and the streak rule, which is the reason
/// they are one struct. Asserted per wire so a future arm cannot quietly opt out.
#[test]
fn every_wire_shares_the_severity_gate() {
    for wire in [Wire::NotifySend, Wire::Cmux] {
        let n = ProgramNotifier::wired(wire, Severity::Warn);
        assert!(!n.will_send(&escalation(Severity::Info)), "{wire:?}");
        assert!(n.will_send(&escalation(Severity::Warn)), "{wire:?}");
        assert!(n.will_send(&escalation(Severity::Urgent)), "{wire:?}");
        assert!(
            n.spawn_delivery(&escalation(Severity::Info)).is_none(),
            "{wire:?}"
        );
    }
}

/// A missing program is a spawn error, which is not transient — so the transport
/// latches off after ONE attempt rather than shelling out on every sweep. This is
/// what lets `pmd` register a wire whose program the machine does not have.
#[test]
fn every_wire_latches_off_after_one_unspawnable_attempt() {
    for wire in [Wire::NotifySend, Wire::Cmux] {
        let n = ProgramNotifier::wired(wire, Severity::Warn);
        assert!(!n.is_unavailable(), "{wire:?}");
        record(&n, unspawnable());
        assert!(n.is_unavailable(), "{wire:?}");
        assert_eq!(n.warning_count(), 1, "{wire:?}");
    }
}

/// A program that runs and refuses is retried, because that failure can clear: the
/// cmux CLI fails this way while the SSH attach is down, and the attach comes back.
#[test]
fn a_refusing_program_only_latches_after_the_streak() {
    let n = ProgramNotifier::wired(Wire::Cmux, Severity::Warn);
    for _ in 0..DEAD_AFTER - 1 {
        record(
            &n,
            exited(1, "failed to read relay auth challenge: i/o timeout"),
        );
        assert!(!n.is_unavailable());
    }
    record(
        &n,
        exited(1, "failed to read relay auth challenge: i/o timeout"),
    );
    assert!(n.is_unavailable());
}
