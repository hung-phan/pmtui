//! THE GATE: the foreground attach must not outlive the pmtui that spawned it.
//!
//! `TmuxDriver::attach_interactive` runs `tmux attach-session` as a CHILD and waits on it, so
//! pmtui cannot exit normally while the attach lives. An ABNORMAL death is the hole: on `SIGKILL`
//! the child is reparented to init and KEEPS HOLDING the tmux client, and every consumer then
//! reads that client as a present human. `JobScheduler::human_present` defers on
//! `Driver::has_clients`, so a driven row re-parks `Monitoring` every tick and is never nudged
//! again; `send_decision` refuses `s` with "attached there — type it in that pane". Both symptoms,
//! one orphan, and nothing says so.
//!
//! Measured live on 2026-10-10: a `start-up` row sat 47h past a 6h cadence
//! (`nudged_at` 1791488951) behind `tmux -L pmd attach-session` pid 2296786 whose PPID was 1, on
//! a tty whose own shell was still alive — so the pty never closed and never hung the orphan up.
//!
//! Only a real kernel settles this. `PR_SET_PDEATHSIG` is delivered ON PARENT DEATH, which no
//! unit test and no `FakeDriver` can observe: the whole mechanism is "what the OS does to a
//! process once another process is gone". A green unit suite is exactly what shipped the orphan.
//!
//! THE CONTROL IS LOAD-BEARING. The parent runs inside a tmux pane whose shell OUTLIVES it
//! (`…; sleep`), because if the pty closed with the parent then `SIGHUP` would reap the attach on
//! its own and this test would pass without the fix. That is the arrangement the live failure had.
//!
//! Run: `cargo test --test integration pdeathsig -- --ignored --nocapture`

use std::process::Command;
use std::time::Duration;

use agent_manager::tmux::{Driver, TmuxDriver};

use crate::probe::{TmuxSocket, kill_server_and_socket, list_clients, pid_alive, wait_until};

/// Set on the re-exec so the same `#[test]` acts as the doomed PARENT instead of the harness.
/// Carries `socket\nsession`.
const ROLE: &str = "PM_PDEATHSIG_ATTACH";
/// The module-qualified test path, so the re-exec runs THIS test and not the whole ignored suite.
const TEST_PATH: &str = "pdeathsig::the_attach_client_dies_with_the_pmtui_that_spawned_it";
/// Env var naming where the parent publishes its own pid for the harness to kill.
const PID_FILE: &str = "PM_PDEATHSIG_PIDFILE";

#[test]
#[ignore]
fn the_attach_client_dies_with_the_pmtui_that_spawned_it() {
    if let Ok(role) = std::env::var(ROLE) {
        return run_as_doomed_parent(&role);
    }
    if !crate::probe::tmux_available() {
        eprintln!("skipping pdeathsig gate: tmux not available");
        return;
    }
    run_harness();
}

/// The pmtui stand-in. Publishes its pid, then blocks in the PRODUCTION attach path — never a
/// hand-rolled `tmux attach-session`, because the argv and its pre-exec guard are what is under
/// test. A hand-copy here would prove only that this file can set a signal.
fn run_as_doomed_parent(role: &str) {
    let (socket, session) = role.split_once('\n').expect("socket\\nsession");
    let pid_file = std::env::var(PID_FILE).expect("pid file path");
    std::fs::write(&pid_file, std::process::id().to_string()).expect("publish pid");
    let driver = TmuxDriver::with_socket(socket);
    let _ = driver.attach_interactive(session);
}

fn run_harness() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let pid_file = dir.path().join("parent.pid");
    let inner = TmuxSocket::new("am-pdeath-inner");
    let outer = TmuxSocket::new("am-pdeath-outer");
    const TARGET: &str = "pm-pdeath-target";
    const HOST: &str = "pdeath-host";

    // The session a client will attach to. Stands in for a project terminal: it must still be
    // alive at the end, because reaping the orphan must never touch the agent.
    let up = Command::new("tmux")
        .args([
            "-L",
            inner.name(),
            "new-session",
            "-d",
            "-s",
            TARGET,
            "-x",
            "80",
            "-y",
            "24",
            "while true; do sleep 1; done",
        ])
        .status()
        .is_ok_and(|s| s.success());

    // A pane whose shell outlives the parent, so the pty survives the kill (see the module note).
    // `env -u TMUX` because a tmux client started inside a pane otherwise resolves $TMUX's socket
    // rather than the `-L` we pass.
    let exe = std::env::current_exe().expect("test binary");
    let parent_cmd = format!(
        "env -u TMUX {ROLE}='{socket}\n{TARGET}' {PID_FILE}='{pid_file}' {exe} \
         --exact {TEST_PATH} --ignored --nocapture; sleep 120",
        socket = inner.name(),
        pid_file = pid_file.display(),
        exe = exe.display(),
    );
    let hosted = up
        && Command::new("tmux")
            .args([
                "-L",
                outer.name(),
                "new-session",
                "-d",
                "-s",
                HOST,
                "-x",
                "80",
                "-y",
                "24",
                &parent_cmd,
            ])
            .status()
            .is_ok_and(|s| s.success());

    // The attach has to actually land first, or killing the parent proves nothing.
    let attached = hosted
        && wait_until(Duration::from_secs(20), || {
            !list_clients(inner.name(), TARGET).trim().is_empty()
        });
    let parent_pid = std::fs::read_to_string(&pid_file)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // SIGKILL, the one death a parent cannot clean up after: no destructor, no handler, and the
    // child is never signalled.
    let killed = attached
        && !parent_pid.is_empty()
        && Command::new("kill")
            .args(["-9", &parent_pid])
            .status()
            .is_ok_and(|s| s.success());
    let released = killed
        && wait_until(Duration::from_secs(20), || {
            list_clients(inner.name(), TARGET).trim().is_empty()
        });
    let clients = list_clients(inner.name(), TARGET);
    let target_alive = TmuxDriver::with_socket(inner.name())
        .is_alive(TARGET)
        .unwrap_or(false);
    let parent_gone = parent_pid.is_empty() || !pid_alive(&parent_pid);

    // Only if it somehow survived: killing an already-dead pid makes `kill` print
    // "No such process" into the suite log, which reads like a failure in the one test whose
    // whole subject is a process that should be gone.
    if !parent_pid.is_empty() && pid_alive(&parent_pid) {
        let _ = Command::new("kill").args(["-9", &parent_pid]).status();
    }
    kill_server_and_socket(inner.name());
    kill_server_and_socket(outer.name());

    assert!(up && hosted, "could not stand up the probe (up={up})");
    assert!(
        attached,
        "the parent never attached a client, so the kill proves nothing"
    );
    assert!(killed, "could not SIGKILL the parent pid {parent_pid:?}");
    assert!(
        released,
        "ORPHANED ATTACH: the tmux client outlived the pmtui that spawned it, so every \
         `has_clients` consumer still reads a present human. clients:\n{clients}"
    );
    assert!(parent_gone, "the parent survived its own SIGKILL");
    assert!(
        target_alive,
        "reaping the orphan must never end the agent's own terminal"
    );
}
