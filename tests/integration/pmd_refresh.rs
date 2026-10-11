//! Acceptance: a running `pmd` really replaces itself when its binary is replaced underneath it.
//!
//! This is the half of "refresh after an update" that no dashboard can do. One `pmd` owns one
//! socket, so a machine may be running several, and a dashboard has authority over exactly one of
//! them — so each daemon has to notice for itself. The unit tests pin what counts as a new build;
//! what they cannot show is that a LIVE daemon, mid-loop, acts on it and comes back.
//!
//! Driven against a COPY of the binary inside a temp directory, replaced by `rename` exactly as
//! `install.sh` does it. Nothing outside that directory is touched.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::probe::{TmuxSocket, tmux_available};

/// Copy `from` to `to` and make it executable — the installer's `cp … && chmod +x` step.
fn copy_exe(from: &Path, to: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::copy(from, to).expect("copy the pmd binary");
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755)).expect("make it runnable");
}

/// Wait until `path` contains `needle`, or give up. Polled rather than slept: the daemon restarts on
/// its own schedule and a fixed sleep would either be flaky or slow.
fn wait_for_text(path: &Path, needle: &str, budget: Duration) -> String {
    let start = Instant::now();
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) || start.elapsed() >= budget {
            return text;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "acceptance: a real pmd replacing its own image"]
fn a_live_pmd_restarts_itself_onto_a_new_build() {
    if !tmux_available() {
        eprintln!("skipping pmd-refresh acceptance: tmux not available");
        return;
    }

    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    // An EMPTY registry: this test is about the daemon's own lifecycle, and a daemon with nothing
    // to drive still has to notice its build changed.
    std::fs::write(&registry, "{\"projects\":[]}").expect("write a scratch registry");

    // The daemon under test is a COPY, so the rename below replaces a file nothing else owns.
    let exe = dir.path().join("pmd");
    copy_exe(Path::new(env!("CARGO_BIN_EXE_pmd")), &exe);

    let socket = TmuxSocket::new("pm-refresh");
    let log = dir.path().join("pmd.log");
    let sink = std::fs::File::create(&log).expect("create the log");
    let mut child = Command::new(&exe)
        .arg("--registry")
        .arg(&registry)
        .arg("--socket")
        .arg(socket.name())
        .arg("--tick-ms")
        .arg("200")
        .env("ECC_GATEGUARD", "off")
        .stdout(Stdio::null())
        .stderr(sink)
        .spawn()
        .expect("spawn pmd");
    let pid = child.id();

    // Let it establish its baseline and sweep at least once, so the restart below is a reaction to
    // the replacement rather than to a baseline it never recorded.
    std::thread::sleep(Duration::from_millis(600));
    let alive_before = matches!(child.try_wait(), Ok(None));

    // THE UPDATE, byte for byte what `install.sh` does: stage beside the target, then rename over
    // it. The daemon's path now resolves to a different inode.
    let staged = dir.path().join("pmd.staged");
    copy_exe(Path::new(env!("CARGO_BIN_EXE_pmd")), &staged);
    std::fs::rename(&staged, &exe).expect("replace the binary");

    let said = wait_for_text(&log, "restarting", Duration::from_secs(20));

    // EXEC KEEPS THE PID, so "did it restart?" cannot be asked by watching for a new process — the
    // daemon's own line is the signal, and it is printed before the exec that replaces it.
    let still_running = matches!(child.try_wait(), Ok(None));

    // ---- teardown BEFORE the assertions, so a failure still cleans up ----
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    drop(socket);

    assert!(
        alive_before,
        "pmd died before the binary was replaced: {said}"
    );
    assert!(
        said.contains("restarting"),
        "pmd never noticed the new build: {said}"
    );
    assert!(
        !said.contains("could not restart"),
        "pmd found the new build and failed to become it: {said}"
    );
    // And it is STILL SUPERVISING: a daemon that exited instead of re-execing would leave every
    // session it was driving unattended, which is worse than running an old build.
    assert!(
        still_running,
        "pmd exited instead of replacing its image: {said}"
    );
    // Exactly once. The image that comes back records what it is now running, so a baseline that
    // was not re-read would restart on every tick.
    assert_eq!(
        said.matches("restarting").count(),
        1,
        "pmd restarted more than once, so the new image kept the old baseline: {said}"
    );
}
