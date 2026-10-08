//! ACCEPTANCE for the create form itself: a session created with autopilot ALREADY
//! selected must come out of the form with a real `pmd` behind it. It builds its own
//! fixture rather than using [`crate::pmtui_fixture`], because the point is what
//! `submit_create` does on a virgin registry with the real daemon sitting beside the
//! real `pmtui`.

use std::process::Command;
use std::time::Duration;

use agent_manager::registry::{Mode, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, TmuxDriver};

use crate::keystrokes::{send_key, send_literal};
use crate::probe::TmuxSocket;
use crate::probe::{
    pmd_pids_for, tmux_available, wait_for_pane_text, wait_for_pane_text_within, wait_until,
};

/// ACCEPTANCE: creating a session **with autopilot already selected** must start the
/// daemon, exactly as flipping autopilot on later with `m` does.
///
/// This is the one bug class a `FakeDriver` unit test cannot see. `pmd` is the ONLY
/// thing that drives an agent-loop session, and `submit_create` never ensured one —
/// so "autopilot at create" silently did nothing until pmtui was restarted, while
/// "autopilot via `m`" worked. Every layer in between (registry write, seeded config,
/// ledger) looked perfect on disk. So this drives the REAL create FORM with real
/// keystrokes into a real `pmtui` on a real tmux server — the way the human hits it —
/// and checks the two observables that actually matter: what the dashboard says, and
/// whether a `pmd` process exists holding the daemon singleton lock.
///
/// Hygiene, mirroring `send_keys_against_real_tmux_reaches_the_pane`: a private
/// per-pid socket and a scratch registry in a tempdir, every result recorded into a
/// local, and the spawned `pmd` killed by pid + the tmux server killed BEFORE the
/// first assertion — so a failing assertion can never leak a daemon or a tmux server.
///
/// `claude` is stubbed on `PATH` (as the worker tests stub `claude -p` with `sh`):
/// `pmd` really does sweep and really does launch the engine, but the engine is a
/// `cat` that spends no tokens. `ECC_GATEGUARD=off` for the same reason the live
/// autopilot runs need it — a gated first tool call stalls the driven agent.
///
/// `#[ignore]` like the other real-substrate tests here; run with
/// `cargo test --test integration -- --ignored create_with_autopilot_starts_the_daemon`.
#[test]
#[ignore]
fn create_with_autopilot_starts_the_daemon() {
    if !tmux_available() {
        eprintln!("skipping live create-form test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    // Canonicalized because `submit_create` canonicalizes what the form carries, and
    // the form is seeded from pmtui's cwd (`new-session -c`) — so the registry root and
    // the path we assert on have to be the same string.
    let proj = {
        let p = dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::canonicalize(&p).unwrap()
    };
    // Private socket + session name that cannot collide with the daemon's real sockets
    // or any `pmloop-`/`pmchat-` session; both are created and destroyed here.
    let socket = TmuxSocket::new("am-create");
    let session = format!("amcr-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());

    // The stubbed engine. `cat` keeps its pane alive (a process that exits would tear
    // the session down and make pmd's launch look like a failure) and spends nothing.
    let stub_bin = dir.path().join("bin");
    std::fs::create_dir_all(&stub_bin).unwrap();
    let stub = stub_bin.join("claude");
    std::fs::write(&stub, "#!/bin/sh\nexec cat\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Wide pane: the status trails the keybar chips and is TRUNCATED into whatever
    // columns are left, so a narrow pane could clip the daemon fragment off the end of
    // an otherwise-correct status and fail this test for the wrong reason.
    let launched = Command::new("tmux")
        .args([
            "-L",
            socket.name(),
            "new-session",
            "-d",
            "-s",
            &session,
            "-c",
            &proj.display().to_string(),
            "-x",
            "300",
            "-y",
            "50",
            &format!(
                "PATH='{}':\"$PATH\" ECC_GATEGUARD=off exec '{}' --socket '{}' --registry '{}'",
                stub_bin.display(),
                env!("CARGO_BIN_EXE_pmtui"),
                socket.name(),
                reg_path.display(),
            ),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    // Everything below records into locals; the daemon and the server are killed
    // before any assertion runs.
    //
    // The dashboard is up once the keybar paints. `q Quit` is on it at every width.
    let dash_up =
        launched && wait_for_pane_text_within(&driver, &session, "Quit", Duration::from_secs(15));

    // `n` opens the create form. Field order includes optional Name between Directory and Autonomy,
    // and `-c` above already seeded the Directory field with the project root, so the human's
    // Message is focused first; five Tabs walk Message→engine→model→dir→name→autonomy.
    let form_up = dash_up
        && send_literal(socket.name(), &session, "n")
        && wait_for_pane_text(&driver, &session, "New session");
    let autopilot_selected = form_up
        && send_key(socket.name(), &session, "Tab")
        && send_key(socket.name(), &session, "Tab")
        && send_key(socket.name(), &session, "Tab")
        && send_key(socket.name(), &session, "Tab")
        && send_key(socket.name(), &session, "Tab")
        && send_key(socket.name(), &session, "Space")
        // Proves the dial actually landed on Autopilot BEFORE submitting, so a create
        // that silently stayed on Standard can't masquerade as a pass.
        && wait_for_pane_text(&driver, &session, "autopilot");
    let submitted = autopilot_selected
        && send_key(socket.name(), &session, "Tab")
        && send_literal(socket.name(), &session, "keep the fixture green")
        && send_key(socket.name(), &session, "Enter");

    // The two halves of the fix, in the status line: the create happened, AND the
    // daemon ensure ran and reported. Waited for separately so a failure says which.
    let saw_created = submitted && wait_for_pane_text(&driver, &session, "created");
    let saw_daemon = saw_created && wait_for_pane_text(&driver, &session, "daemon");
    let pane = driver.capture_tail(&session, 200).unwrap_or_default();

    // pmd is the point: a status claiming "daemon started" is worthless if no process
    // exists. Two independent observables — a live pid, and the singleton flock being
    // UNAVAILABLE to us (which is exactly how pmtui itself probes liveness).
    let pmd_pids = pmd_pids_for(socket.name());
    let lock_held = matches!(
        agent_manager::lease::try_acquire(&agent_manager::lease::daemon_lock_path(
            &reg_path,
            socket.name()
        )),
        Ok(None)
    );

    // On-disk proof the form really submitted (independent of any pane text).
    let registry = Registry::load(&reg_path);

    // ---- teardown: quit pmtui normally so instrumented runs flush coverage, then stop the
    // daemon and remove the server. ----
    let dashboard_quit = send_key(socket.name(), &session, "q")
        && wait_until(Duration::from_secs(5), || {
            !driver.is_alive(&session).unwrap_or(false)
        });
    for pid in &pmd_pids {
        let _ = Command::new("kill").arg("-TERM").arg(pid).status();
    }
    let pmd_stopped = wait_until(Duration::from_secs(10), || {
        pmd_pids_for(socket.name()).is_empty()
    });
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();
    let pmd_left = pmd_pids_for(socket.name());

    // ---- assertions (daemon and server are already gone) ----
    assert!(launched, "tmux new-session should start pmtui");
    assert!(
        dash_up,
        "pmtui should paint its dashboard; pane was:\n{pane}"
    );
    assert!(
        form_up,
        "`n` should open the create form; pane was:\n{pane}"
    );
    assert!(
        autopilot_selected,
        "space on the Autonomy field should select autopilot; pane was:\n{pane}"
    );
    assert!(submitted, "the form should accept a goal and Enter");
    assert!(
        saw_created,
        "the dashboard should report the create; pane was:\n{pane}"
    );
    assert!(
        saw_daemon,
        "the create must ALSO report the daemon ensure — this is the bug: creating with \
         autopilot on left nothing driving the session. Pane was:\n{pane}"
    );
    assert!(
        !pmd_pids.is_empty(),
        "a real pmd process must exist after creating an autopilot session"
    );
    assert!(
        lock_held,
        "the daemon singleton flock must be held by that pmd (this is how pmtui probes it)"
    );
    let reg = registry.expect("registry should be readable");
    let entry = reg
        .projects
        .iter()
        .find(|p| p.root == proj)
        .expect("the created session should be registered");
    assert_eq!(entry.mode, Mode::AgentLoop);
    let cfg: Config = state::read_json(&ProjectPaths::for_session(&proj, &entry.id).config())
        .expect("per-session config should be seeded");
    assert_eq!(
        cfg.autonomy,
        Tier::Autopilot,
        "the form's Autonomy dial is what got seeded"
    );
    assert!(
        pmd_stopped && pmd_left.is_empty(),
        "teardown must leave no pmd behind, still running: {pmd_left:?}"
    );
    assert!(dashboard_quit, "q should stop the real dashboard cleanly");
}
