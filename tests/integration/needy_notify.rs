//! The notification a session's human actually receives: OSC 777 bytes, written by the
//! DASHBOARD to its own terminal, when a row newly needs them.
//!
//! This needs a real pty and nothing else can stand in for one. The transport writes only
//! when its stderr `is_terminal()`, so redirecting pmtui's stderr to a file — which is how
//! every other test reads a process's output — turns the feature off by construction. A
//! tmux pane is a terminal, but tmux CONSUMES the sequence, so the bytes can never be
//! captured from the far side of a pane either.
//!
//! `script` gives pmtui a pty and records every byte that crosses it, which is exactly the
//! observation the human makes in their own terminal.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::probe::wait_until;
use super::seed::seed_standard_loop_session;

/// A worker's own report saying it is blocked, with no stop drafted — the autopilot-off
/// marker gate, which is how a STANDARD row reaches needs-you with nothing a daemon had to
/// fold in. The shortest path from a seeded row to the transition under test.
const BLOCKED_MARKER: &str =
    r#"{"seq":3,"state":"blocked","status":"waiting on a decision","next_step":"ask the human"}"#;

fn typescript(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
#[ignore = "real pty: runs the pmtui binary under `script`"]
fn a_session_that_newly_needs_you_writes_osc_777_to_the_dashboards_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let reg = dir.path().join("registry.json");
    let out = dir.path().join("typescript");
    // Seeded NOT needy: the first refresh happens inside `App::new`, and a row that already
    // needs the human then is recorded silently. The transition has to happen while the
    // dashboard is up, which is also the only case worth a notification.
    seed_standard_loop_session(&reg, &root, "bot");
    let paths = agent_manager::state::ProjectPaths::for_session(&root, "bot");

    // No tmux server on this socket: the dashboard draws its rows from the registry, and a
    // dead socket only costs it the preview capture.
    let mut child = Command::new("script")
        .arg("-q")
        // Unbuffered, or the bytes under test sit in `script`'s buffer until it exits.
        .arg("-f")
        .arg("-c")
        .arg(format!(
            // `script` copies the window size from ITS OWN terminal, and the test harness has
            // none — the pty then comes up 0x0 and the dashboard draws an empty frame forever.
            // The pty slave is this shell's own tty, so it can size itself before `exec`.
            "stty rows 40 cols 140; exec env -u PMTUI_SESSION -u PMTUI_STATE_DIR -u PMTUI_BIN '{}' \
             --socket 'am-osc-none-{}' --registry '{}'",
            env!("CARGO_BIN_EXE_pmtui"),
            std::process::id(),
            reg.display(),
        ))
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("`script` must be installed to run this test");

    let drew = wait_until(Duration::from_secs(20), || {
        typescript(&out).contains("SESSIONS")
    });
    let before = typescript(&out);
    if drew {
        std::fs::write(paths.needs_you(), BLOCKED_MARKER).unwrap();
    }
    // The dashboard re-reads the registry and the session's files on every idle tick, so
    // the transition is seen without a keystroke.
    let notified = drew
        && wait_until(Duration::from_secs(20), || {
            typescript(&out).contains("\u{1b}]777;notify;")
        });
    let after = typescript(&out);

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        drew,
        "the dashboard never drew its session list under a pty; it wrote:\n{before}"
    );
    assert!(
        !before.contains("\u{1b}]777;notify;"),
        "nothing may be notified before the row needs anyone"
    );
    assert!(
        notified,
        "a row that newly needs the human must write OSC 777 to the dashboard's own terminal"
    );
    let seq = after
        .split("\u{1b}]777;notify;")
        .nth(1)
        .and_then(|rest| rest.split("\u{1b}\\").next())
        .unwrap_or_default()
        .to_string();
    assert!(
        seq.contains("bot") && seq.contains("needs you"),
        "the notification must name the session and what it wants: {seq:?}"
    );
    assert_eq!(
        after.matches("\u{1b}]777;notify;").count(),
        1,
        "EDGE-TRIGGERED: the row stays needy across many refreshes, and refreshes run several \
         times a second — one notification per transition, not per frame"
    );
}
