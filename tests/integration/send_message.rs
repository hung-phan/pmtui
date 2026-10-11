//! ACCEPTANCE for the `s` key: say ONE thing to a running agent without taking over
//! the terminal. The positive and the negative are one unit, and the negative is the
//! important half — `send_keys` always finishes with a separate `Enter`, so typing
//! into a pane that is showing a numbered permission dialog would CONFIRM it.

use std::time::Duration;

use agent_manager::tmux::Driver;

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{PmdSibling, detach_to_dashboard, enter_fixture};
use crate::probe::{tmux_available, wait_for_pane_text_within, wait_until};
use crate::stubs::{DIALOG_STUB, ECHOING_COMPOSER_STUB, write_stub};

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
    let fx = enter_fixture("odlg", PmdSibling::DelayedReal);
    write_stub(&fx, DIALOG_STUB);
    let submitted = fx.create_then_enter(true);
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
        "the create form should accept Autopilot + a goal"
    );
    assert!(loop_up, "pmd should launch the persistent agent");
    assert!(back, "Ctrl+q should return to the dashboard:\n{host_pane}");
    assert!(field_open, "`s` should open the send field:\n{host_pane}");
    // THE INVARIANT: not one byte reached the dialog.
    assert!(
        !agent_pane.contains("LEAKED"),
        "`s` typed into a pane showing a permission dialog; pane was:\n{agent_pane}"
    );
    // …and it said why. Either refusal is correct here: the pane-state gate fires on the
    // dialog itself, and pmd may independently have parked a stop for the same dialog
    // first (in which case `a` is the route, not `s`).
    assert!(
        host_pane.contains("dialog up") || host_pane.contains("stop open"),
        "the refusal did not name a reason:\n{host_pane}"
    );
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}
