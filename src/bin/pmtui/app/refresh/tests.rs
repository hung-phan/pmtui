//! The working-vs-idle CONFIRMATION GATE, which is pure and so can be tested without tmux.
//!
//! Moved out of `refresh.rs` but kept beside it: the four things it exercises are private to that
//! module, and this path leaves them reachable without widening what production exposes.

use agent_manager::registry::Engine;

use super::{
    ACTIVITY_ANCHOR_MAX_S, ACTIVITY_IDLE_CONFIRMATIONS, ActivityObservation, idle_gate_step,
    merge_activity, turn_anchor_step, turn_in_progress,
};

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

/// THE BUG THIS EXISTS FOR. A Standard row has no pmd ledger, so `hook_working` is `None` and the
/// case asserted last above — "without a hook, stable pane content confirms idle" — is the one it
/// lands in. Claude leaves its bare composer on screen through a silent tool call, so a mid-turn
/// pane went byte-stable and the row drew the idle `○` on an agent that was working.
#[test]
fn an_anchored_turn_withholds_a_confirmed_idle_on_a_standard_row() {
    // Tick 1: the pane is busy at turn 7, which ARMS the anchor.
    let armed = turn_anchor_step(None, ActivityObservation::Busy, 7, 1_000, false);
    assert_eq!(armed, Some((7, 1_000)));

    // Tick 2: the composer is back and byte-stable, but the hook has recorded no new turn. The
    // anchor stands and the veto holds the confirmation back.
    let kept = turn_anchor_step(armed, ActivityObservation::IdleConfirmed, 7, 1_001, false);
    assert_eq!(kept, armed, "an idle frame does not disarm the anchor");
    assert!(turn_in_progress(kept, 7));

    // The turn ends: the hook appends its byte, the count passes the anchor, and the same frame now
    // reads idle. ONE byte is the whole difference between `●` and `○` here.
    assert!(!turn_in_progress(kept, 8));
}

#[test]
fn an_anchor_is_only_ever_a_veto_and_always_expires() {
    let armed = Some((7u64, 1_000i64));
    // Only BUSY arms. An unreadable or missing-count tick never invents an anchor, so a row this
    // dashboard has never seen working cannot be held at `●` by one.
    for observed in [
        ActivityObservation::Unreadable,
        ActivityObservation::IdleUnconfirmed,
        ActivityObservation::IdleConfirmed,
    ] {
        assert_eq!(
            turn_anchor_step(None, observed, 7, 1_000, false),
            None,
            "{observed:?} must not arm an anchor"
        );
    }
    // A DEAD HOOK must not pin the glyph. Past the expiry the anchor is gone, the pane's own
    // stability decides again, and the row reads whatever it actually looks like.
    assert_eq!(
        turn_anchor_step(
            armed,
            ActivityObservation::IdleConfirmed,
            7,
            1_000 + ACTIVITY_ANCHOR_MAX_S,
            false
        ),
        None,
        "an anchor older than the bound is dropped"
    );
    // A HUMAN drives the same hooked process, so the count can move for turns this anchor never
    // witnessed and comparing against it means nothing.
    assert_eq!(
        turn_anchor_step(armed, ActivityObservation::Busy, 7, 1_001, true),
        None,
        "a human attach drops the anchor, even on a busy pane"
    );
    // A pane that is gone is idle by `merge_activity`, and an anchor for a session with no terminal
    // would outlive the thing it described.
    assert_eq!(
        turn_anchor_step(armed, ActivityObservation::Missing, 7, 1_001, false),
        None
    );
    // With no anchor there is no veto, whatever the count says.
    assert!(!turn_in_progress(None, 0));
}
