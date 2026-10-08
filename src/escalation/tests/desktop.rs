use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::escalation::transport::{DESKTOP_DEAD_AFTER, first_line, urgency_for};

#[test]
fn desktop_notifier_gates_below_min_severity() {
    let n = DesktopNotifier::default(); // min = Warn
    assert!(!n.will_send(&Escalation::for_stops(
        "p",
        &[stop("s", "ambiguity", RiskClass::Low)] // Info < Warn
    )));
    assert!(n.will_send(&Escalation::stuck("p", "boom"))); // Urgent
}

#[test]
fn desktop_urgency_matches_the_notification_protocol() {
    assert_eq!(urgency_for(Severity::Info), "low");
    assert_eq!(urgency_for(Severity::Warn), "normal");
    assert_eq!(urgency_for(Severity::Urgent), "critical");
}

#[test]
fn desktop_failure_detail_uses_the_first_nonempty_line_and_caps_unicode_safely() {
    assert_eq!(first_line(b"\n  \n real cause \n ignored"), "real cause");
    assert_eq!(first_line(b""), "no stderr");

    let long = format!("  {}tail", "\u{e9}".repeat(201));
    assert_eq!(
        first_line(long.as_bytes()),
        format!("{}\u{2026}", "\u{e9}".repeat(200))
    );
}

#[test]
fn desktop_delivery_passes_urgency_and_message_to_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("record-notification");
    let args = dir.path().join("record-notification.args");
    std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"${0}.args\"\n").unwrap();
    let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bin, permissions).unwrap();

    let notifier = DesktopNotifier::new(bin.to_string_lossy(), Severity::Info);
    let escalation = Escalation {
        project: "proj".into(),
        title: "decision needed".into(),
        body: "line one\nline two".into(),
        severity: Severity::Urgent,
    };
    notifier
        .spawn_delivery(&escalation)
        .unwrap()
        .join()
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(args).unwrap(),
        "-u\ncritical\npmd \u{b7} proj\ndecision needed\nline one\nline two\n"
    );
    assert_eq!(notifier.failure_count(), 0);
    assert!(!notifier.is_unavailable());
}

#[test]
fn desktop_nonzero_exit_is_recorded_and_warns_only_once() {
    let n = desktop("false");
    let gdbus = "GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: not activatable";

    record(&n, exited(1, gdbus));
    assert_eq!(n.failure_count(), 1, "a non-zero exit is a failure, not Ok");
    assert_eq!(n.warning_count(), 1, "the first failure is reported");
    assert!(!n.is_unavailable(), "one blip must not latch it off");

    // A daemon sweeping every 500ms must not reprint this forever.
    record(&n, exited(1, gdbus));
    assert_eq!(n.failure_count(), 2, "still observed");
    assert_eq!(n.warning_count(), 1, "once-guard: no second warning");
}

#[test]
fn desktop_latches_off_after_consecutive_nonzero_exits() {
    let n = desktop("false");
    for _ in 0..DESKTOP_DEAD_AFTER {
        record(&n, exited(1, "still broken"));
    }
    assert!(n.is_unavailable(), "consistent non-zero => transport dead");
    // Latched off: subsequent escalations skip the doomed subprocess.
    assert!(
        n.spawn_delivery(&Escalation::stuck("p", "boom")).is_none(),
        "no subprocess once the transport is declared unavailable"
    );
    assert_eq!(n.warning_count(), 1);
}

#[test]
fn desktop_success_clears_the_failure_streak() {
    // A transient failure must not permanently disable a working notifier.
    let n = desktop("notify-send");
    for _ in 0..DESKTOP_DEAD_AFTER - 1 {
        record(&n, exited(1, "server restarting"));
    }
    record(&n, exited(0, ""));
    record(&n, exited(1, "another blip"));
    assert!(
        !n.is_unavailable(),
        "streak reset by the success, so no latch-off"
    );
}

#[test]
fn desktop_spawn_error_latches_off_immediately() {
    let n = desktop("pmd-no-such-notifier-binary");
    record(&n, unspawnable());
    assert_eq!(n.failure_count(), 1);
    assert_eq!(n.warning_count(), 1);
    assert!(n.is_unavailable(), "a missing binary is never transient");
}

#[test]
fn desktop_notify_returns_ok_when_the_transport_is_unavailable() {
    // The daemon must never fail a sweep because a toast failed.
    let n = desktop("pmd-no-such-notifier-binary");
    record(&n, unspawnable());
    assert!(n.is_unavailable());
    n.notify(&Escalation::stuck("p", "boom")).unwrap();
}

#[test]
fn desktop_suppressed_escalation_spawns_nothing() {
    let n = desktop("false"); // min = Warn
    let quiet = Escalation::for_stops("p", &[stop("s", "ambiguity", RiskClass::Low)]);
    assert!(!n.will_send(&quiet), "Info < Warn");
    assert!(
        n.spawn_delivery(&quiet).is_none(),
        "no subprocess for a gated escalation"
    );
    n.notify(&quiet).unwrap();
    assert_eq!(n.failure_count(), 0, "nothing ran, so nothing failed");
    assert_eq!(n.warning_count(), 0, "and nothing to warn about");
}

/// End-to-end over the real `bin` seam: a command that exits non-zero and a
/// binary that cannot be spawned, delivered on the real thread and joined so
/// the outcome is observable.
///
/// `#[ignore]` because it forks twice per notifier. A `fork` duplicates this
/// process's open file descriptions, so while the child is alive it also holds
/// every `flock` this process holds -- long enough to make a concurrent
/// `daemon::tests` lease test see "already driven by another process" and fail.
#[test]
#[ignore]
fn desktop_transport_failure_against_real_binaries() {
    let urgent = Escalation::stuck("p", "boom");

    // `false` rather than `notify-send`, so the assertions hold on a machine
    // that does have a working notification daemon.
    let n = desktop("false");
    n.notify(&urgent).unwrap(); // returns Ok even though delivery fails
    n.spawn_delivery(&urgent).unwrap().join().unwrap();
    assert!(n.failure_count() >= 1, "non-zero exit observed");
    assert_eq!(n.warning_count(), 1, "warned once");

    let missing = desktop("pmd-no-such-notifier-binary");
    missing.notify(&urgent).unwrap();
    missing.spawn_delivery(&urgent).unwrap().join().unwrap();
    assert!(missing.is_unavailable(), "spawn error latches it off");
}
