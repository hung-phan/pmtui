//! ACCEPTANCE for the two things the dashboard has to SAY OUT LOUD: whether a real
//! `pmd` is alive, and whether a live chat REPL is holding the wheel. Both are facts
//! about a flock held by a real process and a real tmux session's liveness, so both
//! need real substrate — and both are checked for HONESTY, not just for appearing.

use std::process::Command;
use std::time::Duration;

use agent_manager::tmux::Driver;

use crate::keystrokes::send_key;
use crate::pmtui_fixture::{EnterFixture, PmdSibling, enter_fixture, turn_autopilot_on};
use crate::probe::{
    list_clients, pmd_pids_for, probe_pane_pid, tmux_available, wait_for_pane_text,
    wait_for_pane_text_within, wait_until,
};
use crate::seed::seed_standard_loop_session;

// ---------------------------------------------------------------------------
// "I can't tell whether anything is running" (m14)
//
// The complaint these two answer was reported three times in the same words: a session
// is created, Enter goes in, and NOTHING on screen says whether a daemon exists or why
// the loop is not advancing. Two bugs behind it are fixed; what is left is that the
// dashboard did not SAY what was true — so this slice adds a `pmd up`/`pmd DOWN`
// indicator and a `chat` chip, and these tests are the only kind that can check them:
// both facts are composed of a real flock held by a real process and a real tmux
// session's liveness, which a `FakeDriver` unit test cannot see.
// ---------------------------------------------------------------------------

/// ACCEPTANCE: the status bar's `pmd up` / `pmd DOWN` indicator tracks the REAL daemon,
/// through a start and a kill, without restarting `pmtui` — and the render-time probe
/// must not CREATE the daemon singleton lock file.
///
/// That last part is not a detail: `lease::try_acquire` opens the lock `create(true)`,
/// and several pmtui unit tests use "the lock file exists" as the observable proof that
/// an `ensure_daemon` ran. A probe on the render path that created it would silently
/// void them, and no unit test would fail. So this asserts on a FRESH registry directory
/// that the file is still absent after the dashboard has painted the indicator.
///
/// Also the regression guard for the copy fix: `A toggles autopilot` named a key that
/// has not existed since tier and autopilot were consolidated onto `m`, and the line
/// renders on rows whose tier chip reads `[A]`. Every pane captured over the whole run
/// is grepped for it.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored pmd_liveness_indicator_tracks_the_real_daemon`.
#[test]
#[ignore]
fn pmd_liveness_indicator_tracks_the_real_daemon() {
    if !tmux_available() {
        eprintln!("skipping pmd-liveness indicator test: tmux not available");
        return;
    }
    let fx = enter_fixture("liv", PmdSibling::DelayedReal);
    let (sock, sess) = (fx.host_socket.clone(), fx.host_session.clone());
    let lock = agent_manager::lease::daemon_lock_path(&fx.reg_path, &fx.agent_socket);
    // Every pane we look at, concatenated, for the `A toggles` sweep at the end.
    let mut seen = String::new();
    let snap = |fx: &EnterFixture, seen: &mut String| {
        let p = fx
            .host
            .capture_tail(&fx.host_session, 200)
            .unwrap_or_default();
        seen.push_str(&p);
        seen.push('\n');
        p
    };

    // (1) Empty registry, no daemon: the bar must SAY so. `ensure_daemon_for_enabled_
    //     autopilot` is a no-op with nothing registered, so nothing has ensured anything.
    let saw_down = fx.up && wait_for_pane_text(&fx.host, &sess, "pmd DOWN");
    let down_pane = snap(&fx, &mut seen);
    // THE INVARIANT: the render-time probe is non-creating.
    let lock_absent_after_render = !lock.exists();

    // (2) A Standard row appears underneath the running dashboard. Its preview must name
    //     the key that actually switches autopilot (`m`) — and still no lock file.
    seed_standard_loop_session(&fx.reg_path, &fx.proj, "livebot");
    let saw_m_hint = saw_down
        && wait_for_pane_text_within(
            &fx.host,
            &sess,
            "m switches autopilot",
            Duration::from_secs(15),
        );
    let hint_pane = snap(&fx, &mut seen);
    let lock_absent_with_a_row = !lock.exists();

    // (3) `m` = Standard -> Autopilot = "let pmd drive this", which ensures a daemon. The
    //     fixture's pmd sleeps PMD_BOOT_DELAY_S first, so the chip must flip only once the
    //     daemon really holds the flock — not when pmtui merely spawned it.
    let pressed_m = saw_m_hint && turn_autopilot_on(&fx, &sock, &sess);
    let saw_up =
        pressed_m && wait_for_pane_text_within(&fx.host, &sess, "pmd up", Duration::from_secs(25));
    let up_pane = snap(&fx, &mut seen);

    // (4) Kill the daemon out from under a RUNNING pmtui: the chip must fall back to DOWN
    //     on its own, within the probe cache's TTL, with no restart and no keystroke.
    let pids = pmd_pids_for(&fx.agent_socket);
    for pid in &pids {
        let _ = Command::new("kill").arg("-KILL").arg(pid).status();
    }
    let back_down = saw_up
        && !pids.is_empty()
        && wait_for_pane_text_within(&fx.host, &sess, "pmd DOWN", Duration::from_secs(20));
    let final_pane = snap(&fx, &mut seen);

    let pmd_left = fx.teardown();

    // ---- assertions (both servers and the daemon are already gone) ----
    assert!(
        fx.up,
        "pmtui should paint its dashboard; pane was:\n{down_pane}"
    );
    assert!(
        saw_down,
        "with no daemon the status bar must say `pmd DOWN` — nothing on this screen used \
         to answer 'is a daemon running?'. Pane was:\n{down_pane}"
    );
    assert!(
        lock_absent_after_render,
        "the render-time liveness probe MUST NOT create {} — pmtui tests use that file's \
         existence as proof an ensure_daemon ran",
        lock.display()
    );
    assert!(
        saw_m_hint,
        "a fresh loop row's preview must name the BOUND dial key `m` (never `m`, never `A` — \
         both retired); pane was:\n{hint_pane}"
    );
    assert!(
        lock_absent_with_a_row,
        "rendering a row must not create the daemon lock file either"
    );
    assert!(pressed_m, "the `m` keystroke should be delivered");
    assert!(
        saw_up,
        "flipping to Autopilot starts a daemon, and the bar must flip to `pmd up` once it \
         holds the flock; pane was:\n{up_pane}"
    );
    assert!(
        !pids.is_empty(),
        "a real pmd must exist after the Autopilot flip"
    );
    assert!(
        back_down,
        "killing pmd must return the indicator to `pmd DOWN` within the cache TTL, with no \
         pmtui restart; pane was:\n{final_pane}"
    );
    assert!(
        !seen.contains("A toggles"),
        "the unbound `A` key is back in user-facing copy; panes were:\n{seen}"
    );
    assert!(
        pmd_left.is_empty(),
        "teardown must leave no pmd behind, still running: {pmd_left:?}"
    );
    drop(fx); // whole fixture: reaps pmd + both tmux servers, then the tempdir
}

/// ACCEPTANCE: Standard -> Autopilot preserves the exact tmux terminal and process.
#[test]
#[ignore]
fn tier_change_preserves_the_one_terminal() {
    if !tmux_available() {
        eprintln!("skipping unified-terminal indicator test: tmux not available");
        return;
    }
    let fx = enter_fixture("one", PmdSibling::DelayedReal);
    let (sock, host) = (fx.host_socket.clone(), fx.host_session.clone());
    let submitted = fx.create_then_enter(false);
    let created = submitted.then(|| fx.created()).flatten();

    let mut terminal_up = false;
    let mut attached = false;
    let mut detached = false;
    let mut marker_cleared = false;
    let mut turned_on = false;
    let mut same_process = false;
    if let Some((_, paths, session, duplicate)) = &created {
        assert_eq!(session, duplicate, "there is only one terminal name");
        terminal_up = wait_until(Duration::from_secs(20), || {
            fx.agent.is_alive(session).unwrap_or(false)
        });
        attached = terminal_up
            && wait_until(Duration::from_secs(20), || {
                !list_clients(&fx.agent_socket, session).trim().is_empty()
            });
        let before = probe_pane_pid(&fx.agent_socket, session);
        detached = attached
            && send_key(&sock, &host, "C-q")
            && wait_until(Duration::from_secs(15), || {
                list_clients(&fx.agent_socket, session).trim().is_empty()
            });
        marker_cleared =
            detached && wait_until(Duration::from_secs(10), || !paths.chat_lock().exists());
        turned_on = marker_cleared
            && turn_autopilot_on(&fx, &sock, &host)
            && wait_for_pane_text_within(&fx.host, &host, "pmd up", Duration::from_secs(25));
        let after = probe_pane_pid(&fx.agent_socket, session);
        same_process = before.is_some() && before == after;
    }
    let pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    assert!(fx.up && submitted, "fixture/create failed:\n{pane}");
    assert!(
        terminal_up && attached && detached,
        "attach/detach failed:\n{pane}"
    );
    assert!(marker_cleared, "attach intent survived detach:\n{pane}");
    assert!(turned_on, "Autopilot did not start:\n{pane}");
    assert!(same_process, "tier change restarted the terminal:\n{pane}");
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}
