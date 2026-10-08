//! Reading a fact back out of the real substrate, and waiting for one to become
//! true: whether `tmux` exists at all, what a pane renders, which `pmd` is alive,
//! what a stub process actually received. Every wait here POLLS instead of sleeping
//! a fixed time, because the observables belong to real processes on a loaded
//! machine.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use agent_manager::tmux::{Driver, TmuxDriver};

pub(crate) fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// An RAII holder for a private tmux server socket used by a test.
///
/// Exists because the acceptance suite was the biggest source of the socket litter on this
/// box: tests minted `format!("{prefix}-{pid}")` names, started servers, and — at best —
/// killed the server at the END of the test, which (a) is skipped when the test PANICS on a
/// failed assertion, and (b) never removes the socket FILE (tmux leaves the 0-byte file
/// behind even after `kill-server`). Thousands piled up under `/tmp/tmux-<uid>/`.
///
/// A guard held for the test's scope fixes both: `Drop` runs on the normal path AND on an
/// unwinding panic, killing the server and removing its socket file. Name it once, pass
/// `sock.name()` everywhere the old `&socket` went.
pub(crate) struct TmuxSocket(String);

impl TmuxSocket {
    /// `<prefix>-<pid>` — unique per test-binary process, matching the old naming so a
    /// migrated test keeps the same observable socket name.
    pub(crate) fn new(prefix: &str) -> Self {
        Self(format!("{prefix}-{}", std::process::id()))
    }

    pub(crate) fn name(&self) -> &str {
        &self.0
    }
}

impl Drop for TmuxSocket {
    fn drop(&mut self) {
        kill_server_and_socket(&self.0);
    }
}

/// Kill the server on `socket` (idempotent — most test servers have already self-exited)
/// and remove the lingering 0-byte socket file so runs do not accumulate. Also usable
/// directly for a socket not owned by a [`TmuxSocket`] (e.g. a fixture's second socket).
pub(crate) fn kill_server_and_socket(socket: &str) {
    let _ = Command::new("tmux")
        .args(["-L", socket, "kill-server"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    // `kill-server` leaves the socket file behind; `id -u`/`$TMUX_TMPDIR` here so the path
    // matches wherever tmux actually put it, without a getuid dependency in the test crate.
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{socket}\""
        ))
        .status();
}

/// Poll `path` until it contains `needle` (or `bound` elapses) and return whatever it
/// holds at the end. Used where the honest observable is what a stub PROCESS actually
/// received, rather than what the pane renders.
pub(crate) fn wait_for_file_contents(path: &Path, needle: &str, bound: Duration) -> String {
    let start = Instant::now();
    loop {
        let s = std::fs::read_to_string(path).unwrap_or_default();
        if s.contains(needle) || start.elapsed() >= bound {
            return s;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `capture_tail` (bounded) until `needle` shows up in the pane. Polling
/// rather than a fixed sleep keeps the test robust on a loaded machine.
pub(crate) fn wait_for_pane_text(driver: &TmuxDriver, session: &str, needle: &str) -> bool {
    wait_for_pane_text_within(driver, session, needle, Duration::from_secs(5))
}

/// Poll (bounded) until the status log's NEWEST line contains `needle` — see
/// [`newest_status_entry`]. The wait to use when the same status may already be on screen as
/// history, which a plain [`wait_for_pane_text`] would match on its first poll.
pub(crate) fn wait_for_newest_status(registry: &Path, needle: &str) -> bool {
    wait_until(Duration::from_secs(5), || {
        newest_status_entry(registry).contains(needle)
    })
}

/// The status log's newest line, timestamp stripped; empty when nothing has been logged yet.
///
/// Read from the LOG FILE beside `registry`, not from the screen. This used to scan the ` STATUS `
/// pane's newest `▸` entry, and that pane is gone: the dashboard draws only the latest status, on the
/// keybar, where it is truncated to half the bar — so a long refusal is unmatchable there. The file
/// holds the whole line, and its last line is by definition the newest.
pub(crate) fn newest_status_entry(registry: &Path) -> String {
    let log = registry
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("pmtui.log");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .next_back()
        .map(|line| {
            line.split_once(' ')
                .map_or(line, |(_timestamp, rest)| rest)
                .to_string()
        })
        .unwrap_or_default()
}

/// [`wait_for_pane_text`] with an explicit bound, for waits that include a process
/// spawn (a real `pmtui` painting its first frame) rather than just a keystroke.
pub(crate) fn wait_for_pane_text_within(
    driver: &TmuxDriver,
    session: &str,
    needle: &str,
    bound: Duration,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < bound {
        if let Ok(tail) = driver.capture_tail(session, 200)
            && tail.contains(needle)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Pids of the `pmd` started for `socket`. Matched on `/pmd --socket <socket>` so it
/// cannot pick up the `pmtui` that spawned it (same `--socket`, different exe) or any
/// daemon on another socket — `socket` carries this test's pid.
pub(crate) fn pmd_pids_for(socket: &str) -> Vec<String> {
    Command::new("pgrep")
        .arg("-f")
        .arg(format!("/pmd --socket {socket}"))
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Poll a predicate (bounded) — for observables whose timing depends on a real pmd sweep
/// or a real attach rather than on a keystroke.
pub(crate) fn wait_until(bound: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < bound {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// `list-clients` output for a session. Returns the STDOUT text, because
/// `display-message`-family targets exit 0 and print NOTHING on a bad target — so the
/// output, never the exit status, is the observable.
pub(crate) fn list_clients(socket: &str, session: &str) -> String {
    Command::new("tmux")
        .args(["-L", socket, "list-clients", "-t", &format!("={session}")])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// The SESSIONS-column half of a captured pane: everything left of the PREVIEW pane's
/// left border. In the wide two-pane layout ONE screen row carries both panes, so a
/// naive `pane.contains("chat")` would be satisfied by the preview's park line — this
/// keeps a row-chip assertion about the row.
pub(crate) fn sessions_column(pane: &str) -> String {
    pane.lines()
        .map(|l| l.split("││").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pid of a tmux session's first pane — the process a human sees running.
pub(crate) fn probe_pane_pid(socket: &str, session: &str) -> Option<String> {
    let out = Command::new("tmux")
        .args([
            "-L",
            socket,
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_pid}",
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Is that pid still alive? `/proc` rather than `kill -0`, so a pid we do not own cannot be
/// mistaken for dead by a permissions error.
pub(crate) fn pid_alive(pid: &str) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
