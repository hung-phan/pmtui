//! The working-vs-idle CONFIRMATION GATE, which is pure and so can be tested without tmux.
//!
//! Moved out of `refresh.rs` but kept beside it: the four things it exercises are private to that
//! module, and this path leaves them reachable without widening what production exposes.

use agent_manager::registry::Engine;

use super::{ACTIVITY_IDLE_CONFIRMATIONS, ActivityObservation, idle_gate_step, merge_activity};

#[test]
fn the_idle_gate_confirms_only_a_byte_stable_transcript() {
    // m76. This is the guard that keeps an idle-at-prompt claude reading idle WITHOUT flipping a
    // WORKING one idle mid-stream (m70 streaming-blindness). The rule: a pane that classifies Idle
    // is only trusted as idle after ACTIVITY_IDLE_CONFIRMATIONS byte-stable observations.

    // First Idle observation ARMS the gate (count 1) but does not yet confirm idle.
    let (fp, c) = idle_gate_step(None, 42);
    assert_eq!((fp, c), (42, 1));
    assert!(
        c < ACTIVITY_IDLE_CONFIRMATIONS,
        "one observation is not idle yet ⇒ still running"
    );

    // A SECOND observation with the SAME fingerprint confirms idle.
    let (fp2, c2) = idle_gate_step(Some((42, 1)), 42);
    assert_eq!((fp2, c2), (42, 2));
    assert!(
        c2 >= ACTIVITY_IDLE_CONFIRMATIONS,
        "two stable observations ⇒ confirmed idle"
    );

    // A CHANGED fingerprint — a streaming transcript growing between captures — RESETS the count,
    // so a mid-stream agent never reaches the threshold and never falsely reads idle.
    let (fp3, c3) = idle_gate_step(Some((42, 5)), 99);
    assert_eq!((fp3, c3), (99, 1));
    assert!(
        c3 < ACTIVITY_IDLE_CONFIRMATIONS,
        "a changed transcript re-arms ⇒ stays running"
    );
}

#[test]
fn live_activity_combines_with_the_hook_in_the_fail_safe_direction() {
    for hook in [None, Some(false), Some(true)] {
        assert_eq!(
            merge_activity(Some(Engine::Claude), hook, ActivityObservation::Missing),
            Some(false),
            "no live pane is idle regardless of stale hook state"
        );
        for active in [
            ActivityObservation::Unreadable,
            ActivityObservation::Busy,
            ActivityObservation::IdleUnconfirmed,
        ] {
            assert_eq!(
                merge_activity(Some(Engine::Claude), hook, active),
                Some(true),
                "a live pane without confirmed idleness stays working: {hook:?} {active:?}"
            );
        }
    }
    assert_eq!(
        merge_activity(
            Some(Engine::Claude),
            Some(true),
            ActivityObservation::IdleConfirmed
        ),
        Some(true),
        "an outstanding hook-backed turn cannot be softened by pixels"
    );
    assert_eq!(
        merge_activity(
            Some(Engine::Claude),
            Some(false),
            ActivityObservation::IdleConfirmed
        ),
        Some(false),
        "a completed turn plus confirmed idle pane is idle"
    );
    assert_eq!(
        merge_activity(
            Some(Engine::Claude),
            None,
            ActivityObservation::IdleConfirmed
        ),
        Some(false),
        "without a hook, stable pane content confirms idle"
    );
    assert_eq!(
        merge_activity(
            Some(Engine::Codex),
            Some(true),
            ActivityObservation::IdleConfirmed
        ),
        Some(false),
        "Codex's stable idle composer overrides a stale optional notify byte"
    );
}
