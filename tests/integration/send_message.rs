//! ACCEPTANCE for the `s` key: say ONE thing to a running agent without taking over
//! the terminal. The positive and the negative are one unit, and the negative is the
//! important half — `send_keys` always finishes with a separate `Enter`, so typing
//! into a pane that is showing a numbered permission dialog would CONFIRM it.

use std::time::Duration;

use agent_manager::tmux::Driver;

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{PmdSibling, detach_to_dashboard, enter_fixture};
use crate::probe::{tmux_available, wait_for_pane_text_within, wait_until};
use crate::stubs::{DIALOG_STUB, ECHOING_COMPOSER_STUB, NUDGE_THEN_DIALOG_STUB, write_stub};

/// ACCEPTANCE: `s` types one message into the live agent's own pane.
///
/// The whole point of the key: say one thing to a running agent without taking over the
/// terminal. Only a real pane can prove it — `FakeDriver` cannot show that the bytes
/// arrive, that they arrive at the RIGHT pane, or that the trailing `Enter` submits them.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored s_delivers_a_typed_message`.
#[test]
#[ignore]
fn s_delivers_a_typed_message_to_the_live_agent_pane() {
    if !tmux_available() {
        eprintln!("skipping o-send test: tmux not available");
        return;
    }
    let fx = enter_fixture("osnd", PmdSibling::DelayedReal);
    write_stub(&fx, ECHOING_COMPOSER_STUB);
    let submitted = fx.create_then_enter(true);
    let created = submitted.then(|| fx.created()).flatten();

    let mut loop_up = false;
    let mut back = false;
    let mut field_open = false;
    let mut landed = false;
    let mut receipt = false;
    let mut agent_pane = String::new();
    if let Some((_, _, loop_s, _)) = &created {
        loop_up = wait_until(Duration::from_secs(25), || {
            fx.agent.is_alive(loop_s).unwrap_or(false)
        });
        // Autopilot's Enter auto-attaches; `s` refuses an ATTACHED pane (we cannot see a
        // half-typed draft in it), so detach first — which is also how a human gets here.
        back = loop_up && detach_to_dashboard(&fx, loop_s);
        field_open = back
            && send_literal(&fx.host_socket, &fx.host_session, "s")
            && wait_for_pane_text_within(
                &fx.host,
                &fx.host_session,
                "Send",
                Duration::from_secs(10),
            );
        if field_open {
            let typed = send_literal(&fx.host_socket, &fx.host_session, "ping from pmtui")
                && send_key(&fx.host_socket, &fx.host_session, "Enter");
            // The stub echoes what it received, so this is proof the bytes were SUBMITTED,
            // not merely typed into the composer.
            landed = typed
                && wait_until(Duration::from_secs(20), || {
                    fx.agent
                        .capture_tail(loop_s, 400)
                        .unwrap_or_default()
                        .contains("ECHO ping from pmtui")
                });
            receipt = wait_for_pane_text_within(
                &fx.host,
                &fx.host_session,
                "sent",
                Duration::from_secs(10),
            );
        }
        agent_pane = fx.agent.capture_tail(loop_s, 400).unwrap_or_default();
    }
    let host_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    assert!(fx.up, "pmtui should paint its dashboard:\n{host_pane}");
    assert!(
        submitted,
        "the create form should accept Autopilot + a goal"
    );
    assert!(loop_up, "pmd should launch the persistent agent");
    assert!(back, "Ctrl+q should return to the dashboard:\n{host_pane}");
    assert!(field_open, "`s` should open the send field:\n{host_pane}");
    assert!(
        landed,
        "the typed message never reached the agent's pane; pane was:\n{agent_pane}"
    );
    assert!(receipt, "the send reported nothing:\n{host_pane}");
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}

/// ACCEPTANCE (NEGATIVE): `s` writes NOTHING to a pane showing a permission dialog.
///
/// This is the one failure mode that would have been worse than the bug it fixes.
/// `Driver::send_keys` always finishes with a separate `send-keys Enter`, so on a numbered
/// select widget the typed characters become keyboard accelerators and that Enter CONFIRMS
/// the pre-highlighted option — granting a write the human never approved, destroying
/// their message, and returning `Ok`. No `FakeDriver` test can see it; only a real pane
/// with a real dialog can.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored s_refuses_a_real_pane_showing_a_dialog`.
#[test]
#[ignore]
fn s_refuses_a_real_pane_showing_a_dialog() {
    if !tmux_available() {
        eprintln!("skipping o-dialog test: tmux not available");
        return;
    }
    // STANDARD, deliberately. On Autopilot pmd now notices the same dialog within
    // `DIALOG_WATCH_S` and parks a stop, which flips `message_route` to `Answer` — so `s` opens
    // the Answer surface and this test can never reach the composer whose refusal is its whole
    // subject. It used to pass only because pmd could not see a mid-cadence dialog at all.
    //
    // Standard is also where this gate MATTERS most, which is why the move strengthens the test
    // rather than dodging the failure: `send_decision`'s own note says the pane-state gate "is the
    // only gate that protects an autopilot-OFF row, where `pmd_drives_row` returns before the tick
    // so nothing ever parks a stop and `v.stops` is EMPTY BY DESIGN — the ledger cannot warn us
    // there." No ledger stop can mask a missing pane check here, and the route is deterministic
    // instead of racing pmd. The Autopilot half is covered by
    // `pmd_parks_a_dialog_without_waiting_out_the_cadence` below.
    let fx = enter_fixture("odlg", PmdSibling::DelayedReal);
    write_stub(&fx, DIALOG_STUB);
    let submitted = fx.create_then_enter(false);
    let created = submitted.then(|| fx.created()).flatten();

    let mut loop_up = false;
    let mut back = false;
    let mut field_open = false;
    let mut agent_pane = String::new();
    if let Some((_, _, loop_s, _)) = &created {
        loop_up = wait_until(Duration::from_secs(25), || {
            fx.agent.is_alive(loop_s).unwrap_or(false)
        });
        // The dialog has to be ON SCREEN before the press, or this test proves nothing.
        let dialog_up = loop_up
            && wait_until(Duration::from_secs(15), || {
                fx.agent
                    .capture_tail(loop_s, 100)
                    .unwrap_or_default()
                    .contains("Do you want to create hello.txt?")
            });
        back = dialog_up && detach_to_dashboard(&fx, loop_s);
        field_open = back
            && send_literal(&fx.host_socket, &fx.host_session, "s")
            && wait_for_pane_text_within(
                &fx.host,
                &fx.host_session,
                "Send",
                Duration::from_secs(10),
            );
        if field_open {
            let _ = send_literal(&fx.host_socket, &fx.host_session, "yes do it")
                && send_key(&fx.host_socket, &fx.host_session, "Enter");
            // Give a send that WOULD have happened time to show up before concluding it
            // did not — a negative assertion needs a real window, not an instant.
            std::thread::sleep(Duration::from_secs(3));
        }
        agent_pane = fx.agent.capture_tail(loop_s, 200).unwrap_or_default();
    }
    let host_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    assert!(
        submitted,
        "the create form should accept a Standard session"
    );
    assert!(
        loop_up,
        "Enter should create and launch the agent's terminal"
    );
    assert!(back, "Ctrl+q should return to the dashboard:\n{host_pane}");
    assert!(field_open, "`s` should open the send field:\n{host_pane}");
    // THE INVARIANT: not one byte reached the dialog.
    assert!(
        !agent_pane.contains("LEAKED"),
        "`s` typed into a pane showing a permission dialog; pane was:\n{agent_pane}"
    );
    // …and it said why. ONE reason now, not two: nothing drives a Standard row, so no ledger stop
    // can exist and the pane-state gate is the only thing that can refuse. The old
    // `|| "stop open"` tolerance admitted a pass in which the pane was never checked at all.
    assert!(
        host_pane.contains("dialog up"),
        "the refusal did not name the dialog:\n{host_pane}"
    );
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}

/// ACCEPTANCE for the fix: pmd must notice a dialog it was NOT waiting for.
///
/// A dialog is the one blocking state the agent cannot report — it is stopped inside its own turn,
/// so it writes no marker. `tick`'s `Monitoring` arm therefore saw no marker revision, was not yet
/// due, and returned the parked timer without reading the pane; nothing looked again until the
/// cadence expired. Measured against live rows that was 20 minutes to SIX HOURS of a row still
/// showing the agent's last self-reported "working", and the human's only workaround was `m`
/// off/on — which merely bumps `wake_generation` so the window becomes due.
///
/// Why this cannot be a unit test: the unit suite proves the scheduler's decision against a
/// `FakeDriver` tail, which is a string the test chose. It cannot prove that a REAL claude-shaped
/// dialog on a REAL pane is recognised, that a real pmd's sweep reaches the watch, or that the
/// dashboard then stops claiming the agent is working. This asserts the thing the human sees.
///
/// The cadence is the default 5m and the assertion window is far inside it, so a pass cannot come
/// from the cadence simply expiring.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored pmd_parks_a_dialog_without_waiting_out_the_cadence`.
#[test]
#[ignore]
fn pmd_parks_a_dialog_without_waiting_out_the_cadence() {
    if !tmux_available() {
        eprintln!("skipping o-watch test: tmux not available");
        return;
    }
    let fx = enter_fixture("owch", PmdSibling::DelayedReal);
    write_stub(&fx, NUDGE_THEN_DIALOG_STUB);
    let submitted = fx.create_then_enter(true);
    let created = submitted.then(|| fx.created()).flatten();

    let mut loop_up = false;
    let mut back = false;
    let mut dialog_up = false;
    let mut parked = false;
    let mut agent_pane = String::new();
    let mut recognised = false;
    let mut ledger_run = String::new();
    if let Some((_, paths, loop_s, _)) = &created {
        loop_up = wait_until(Duration::from_secs(25), || {
            fx.agent.is_alive(loop_s).unwrap_or(false)
        });
        // Detach FIRST: the watch defers to a human at the pane on purpose, and pmd will not nudge
        // one either, so nothing at all happens until the dashboard has the terminal back.
        back = loop_up && detach_to_dashboard(&fx, loop_s);
        // The dialog cannot appear until the stub is nudged, and pmd cannot nudge until it has
        // confirmed the pane idle twice and parked `Monitoring{now + 5m}`. So reaching this means
        // the dialog is provably INSIDE a parked window — the state that used to be invisible.
        dialog_up = back
            && wait_until(Duration::from_secs(90), || {
                fx.agent
                    .capture_tail(loop_s, 100)
                    .unwrap_or_default()
                    .contains("Do you want to create hello.txt?")
            });
        // 60s is a fifth of the 5m cadence: comfortably longer than DIALOG_WATCH_S plus a real pmd
        // sweep, and far short of the window whose expiry used to be the only thing that worked.
        // A pass here cannot be the cadence coming due.
        parked = dialog_up
            && wait_for_pane_text_within(
                &fx.host,
                &fx.host_session,
                "needs you",
                Duration::from_secs(60),
            );
        // A negative assertion has to say what it SAW, or the next person debugging it starts from
        // zero: the pane the recogniser was given, and the ledger state that chose the branch.
        agent_pane = fx.agent.capture_tail(loop_s, 60).unwrap_or_default();
        recognised = agent_manager::tmux::classify_dialog(&agent_pane).is_some();
        ledger_run = std::fs::read_to_string(paths.pmstate()).unwrap_or_default();
    }
    let host_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    assert!(
        submitted,
        "the create form should accept Autopilot + a goal"
    );
    assert!(loop_up, "pmd should launch the persistent agent");
    assert!(back, "Ctrl+q should return to the dashboard:\n{host_pane}");
    assert!(
        dialog_up,
        "the stub was never nudged, so no dialog was ever raised INSIDE a parked window — this \
         test would then prove nothing about mid-cadence detection"
    );
    assert!(
        parked,
        "pmd never noticed a dialog that was on screen the whole time — the row still claims the \
         agent is working, which is the bug.\n\
         classify_dialog recognised the pane: {recognised}\n\
         agent pane:\n{agent_pane}\n\
         ledger:\n{ledger_run}\n\
         dashboard:\n{host_pane}"
    );
    // The human must be told WHAT is being waited on, not merely that something is.
    assert!(
        host_pane.contains("Do you want to create hello.txt?"),
        "the parked stop must carry the agent's real question:\n{host_pane}"
    );
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}
