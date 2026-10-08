//! A REAL `pmtui`, hosted on its own tmux server so a test can type at it, pointed
//! at a second socket it shares with the `pmd` it spawns — plus the keystroke
//! recipes for driving its dashboard (create a session, flip autopilot on, detach
//! back to the list). Shared by every acceptance test that presses a key a human
//! presses.

use std::process::Command;
use std::time::Duration;

use agent_manager::registry::Registry;
use agent_manager::state::ProjectPaths;
use agent_manager::tmux::{Driver, TmuxDriver, session_name};

use crate::keystrokes::{send_key, send_keys_atomic, send_literal};
use crate::probe::{
    kill_server_and_socket, list_clients, pmd_pids_for, probe_pane_pid, wait_for_pane_text,
    wait_for_pane_text_within, wait_until,
};

/// The shared fixture for the three Enter-routing acceptance tests: a scratch registry, a
/// `claude` stub on `PATH` (a `cat` that keeps its pane alive and spends nothing), a
/// per-pid HOST tmux server running a real `pmtui` in a WIDE pane, and a second per-pid
/// AGENT socket that `pmtui` shares with the `pmd` it spawns.
pub(crate) struct EnterFixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) proj: std::path::PathBuf,
    pub(crate) reg_path: std::path::PathBuf,
    /// Where `pmtui` itself is hosted (the server this test types at).
    pub(crate) host_socket: String,
    pub(crate) host_session: String,
    pub(crate) host: TmuxDriver,
    /// The server `pmtui` + `pmd` + every `pmloop-`/`pmchat-` share.
    pub(crate) agent_socket: String,
    pub(crate) agent: TmuxDriver,
    /// Whether the host `new-session` succeeded and the dashboard painted.
    pub(crate) up: bool,
    /// The exact dashboard launch, kept so [`EnterFixture::relaunch_dashboard`] starts the same
    /// `pmtui` (same env, registry and sockets) again.
    host_launch: HostLaunch,
}

/// How the fixture starts its dashboard in the host session.
struct HostLaunch {
    cwd: std::path::PathBuf,
    width: u16,
    height: u16,
    command: String,
}

impl HostLaunch {
    /// Start the dashboard in `session` on `socket`; true when `new-session` succeeded.
    fn start(&self, socket: &str, session: &str) -> bool {
        Command::new("tmux")
            .args([
                "-L",
                socket,
                "new-session",
                "-d",
                "-s",
                session,
                "-c",
                &self.cwd.display().to_string(),
                "-x",
                &self.width.to_string(),
                "-y",
                &self.height.to_string(),
                &self.command,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Drop for EnterFixture {
    fn drop(&mut self) {
        self.stop_dashboard();
        // The standalone `pmd` that `pmtui` spawned lives OUTSIDE any tmux pane, so the
        // kill-server below does not reach it — it is the exact leak observed on this box
        // (a `pmd --socket am-…` still running an hour after its test). SIGTERM it by pid
        // (its own reaper then clears the pmloop-/pmchat- sessions it owns), and pkill by
        // the UNIQUE agent socket name as a backstop for a `pmd` still inside its
        // boot-delay shim, before its pid is readable anywhere.
        for pid in pmd_pids_for(&self.agent_socket) {
            let _ = Command::new("kill").args(["-TERM", &pid]).status();
        }
        let _ = Command::new("pkill")
            .args(["-TERM", "-f", &self.agent_socket])
            .status();
        // Both private servers — the host server running this test's `pmtui`, and the agent
        // server holding its pmloop-/pmchat- sessions — plus their lingering socket files.
        kill_server_and_socket(&self.host_socket);
        kill_server_and_socket(&self.agent_socket);
    }
}

/// Which `pmd` the fixture puts NEXT TO its `pmtui` copy — the only place
/// `spawn_daemon` looks (`current_exe().parent()/pmd`).
pub(crate) enum PmdSibling {
    /// The real `pmd`, behind a shim that sleeps [`PMD_BOOT_DELAY_S`] first.
    ///
    /// This delay is what makes these tests DISCRIMINATE instead of merely pass. The
    /// defect lives in the window where a `pmd` has been spawned but has not yet taken the
    /// per-session `driver.lock`, and on a warm machine that window is a few
    /// MILLISECONDS — narrower than `submit_create`'s own atomic file writes, so an
    /// un-delayed fixture lands on the ARM path and looks green on the buggy code too
    /// (verified: it did). A slow-booting daemon is not an artificial scenario either —
    /// it is precisely what a loaded machine produces.
    DelayedReal,
    /// No `pmd` at all: the honest-refusal case.
    Missing,
}

/// How long [`PmdSibling::DelayedReal`] waits before exec'ing the real daemon. Long
/// enough to swallow `submit_create`'s writes plus a TUI redraw with orders of magnitude
/// to spare; short enough to keep the tests a few seconds each.
const PMD_BOOT_DELAY_S: u32 = 3;

/// Build [`EnterFixture`] and wait for the dashboard. `pmtui` runs from a COPY in a
/// staging dir so the test controls what `pmd` (if any) sits beside it.
pub(crate) fn enter_fixture(tag: &str, pmd: PmdSibling) -> EnterFixture {
    enter_fixture_at(tag, pmd, 300, 50)
}

/// [`enter_fixture`] at a caller-selected terminal size. UI gallery tests use
/// laptop-sized panes while behavioral routing tests retain the roomy default.
pub(crate) fn enter_fixture_at(
    tag: &str,
    pmd: PmdSibling,
    width: u16,
    height: u16,
) -> EnterFixture {
    enter_fixture_with_env(tag, pmd, width, height, &[])
}

/// [`enter_fixture_at`] with extra environment for the dashboard process and every terminal
/// its agent server starts, for a test that must point pmtui at scratch engine state (for
/// example `CLAUDE_CONFIG_DIR`) instead of the host's own.
pub(crate) fn enter_fixture_with_env(
    tag: &str,
    pmd: PmdSibling,
    width: u16,
    height: u16,
    env: &[(&str, &std::path::Path)],
) -> EnterFixture {
    let dir = tempfile::tempdir().unwrap();
    let reg_path = dir.path().join("registry.json");
    let staging = dir.path().join("exe");
    std::fs::create_dir_all(&staging).unwrap();
    let pmtui_exe = staging.join("pmtui");
    std::fs::copy(env!("CARGO_BIN_EXE_pmtui"), &pmtui_exe).unwrap();
    if let PmdSibling::DelayedReal = pmd {
        let shim = staging.join("pmd");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nsleep {PMD_BOOT_DELAY_S}\nexec '{}' \"$@\"\n",
                env!("CARGO_BIN_EXE_pmd")
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(
        staging.join("pmd").exists(),
        matches!(pmd, PmdSibling::DelayedReal),
        "the fixture must control whether a pmd sits beside pmtui"
    );
    // Canonicalized because `submit_create` canonicalizes what the form carries, and the
    // form is seeded from pmtui's cwd (`new-session -c`).
    let proj = {
        let p = dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::canonicalize(&p).unwrap()
    };
    let host_socket = format!("am-{tag}h-{}", std::process::id());
    let agent_socket = format!("am-{tag}a-{}", std::process::id());
    let host_session = format!("am{tag}-{}", std::process::id());

    let stub_bin = dir.path().join("bin");
    std::fs::create_dir_all(&stub_bin).unwrap();
    let stub = stub_bin.join("claude");
    std::fs::write(&stub, "#!/bin/sh\nexec cat\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Wide pane: `keybar_line` reserves at most a THIRD of the bar for a transient status
    // and truncates the TAIL, so a narrow pane would clip a correct status and fail these
    // tests for the wrong reason.
    let host_launch = HostLaunch {
        cwd: proj.clone(),
        width,
        height,
        // `env -u PMTUI_*` because these tests are usually run BY an agent inside a managed `pm-`
        // terminal: without it the fixture's dashboard, and everything its tmux server starts,
        // inherits the developer's own `PMTUI_SESSION`/`PMTUI_STATE_DIR`, and a stub that runs
        // `pmtui spawn` publishes into the developer's live session instead of the fixture's. That
        // happened: a job child created real rows in the developer's own dashboard.
        command: format!(
            "PATH='{}':\"$PATH\" {}ECC_GATEGUARD=off exec env -u PMTUI_SESSION -u PMTUI_STATE_DIR \
             -u PMTUI_BIN '{}' --socket '{}' --registry '{}'",
            stub_bin.display(),
            env.iter()
                .map(|(name, value)| format!("{name}='{}' ", value.display()))
                .collect::<String>(),
            pmtui_exe.display(),
            agent_socket,
            reg_path.display(),
        ),
    };
    let launched = host_launch.start(&host_socket, &host_session);

    let host = TmuxDriver::with_socket(&host_socket);
    let up = launched
        && wait_for_pane_text_within(&host, &host_session, "pmtui", Duration::from_secs(15));
    EnterFixture {
        dir,
        proj,
        reg_path,
        host_socket,
        host_session,
        host,
        agent: TmuxDriver::with_socket(&agent_socket),
        agent_socket,
        up,
        host_launch,
    }
}

impl EnterFixture {
    /// Quit the dashboard through its normal key path and report whether it is gone. Its
    /// project terminals keep running on the agent server.
    pub(crate) fn quit_dashboard(&mut self) -> bool {
        self.stop_dashboard();
        let gone = !self.host.is_alive(&self.host_session).unwrap_or(false);
        self.up = !gone;
        gone
    }

    /// The running dashboard's pid: the host pane's process, since the launch `exec`s pmtui.
    pub(crate) fn dashboard_pid(&self) -> Option<String> {
        probe_pane_pid(&self.host_socket, &self.host_session)
    }

    /// SIGKILL the dashboard, as a crash would: no shutdown path runs. Returns whether its host
    /// session is gone afterwards.
    pub(crate) fn kill_dashboard(&mut self) -> bool {
        if let Some(pid) = self.dashboard_pid() {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
        }
        let gone = wait_until(Duration::from_secs(10), || {
            !self.host.is_alive(&self.host_session).unwrap_or(false)
        });
        self.up = !gone;
        gone
    }

    /// Start the same dashboard again (same env, registry and sockets) in the host session, after
    /// [`Self::quit_dashboard`] or [`Self::kill_dashboard`], and wait for it to paint.
    pub(crate) fn relaunch_dashboard(&mut self) -> bool {
        self.up = self
            .host_launch
            .start(&self.host_socket, &self.host_session)
            && wait_for_pane_text_within(
                &self.host,
                &self.host_session,
                "pmtui",
                Duration::from_secs(15),
            );
        self.up
    }

    /// Leave an attached project terminal if necessary, then quit the dashboard through its
    /// normal key path. Besides exercising the real shutdown path, this lets instrumented test
    /// binaries flush their coverage profile before the host tmux server is removed.
    fn stop_dashboard(&self) {
        if !self.up {
            return;
        }
        let _ = send_key(&self.host_socket, &self.host_session, "C-q");
        // On the dashboard itself `C-q` reads as `q` and quits at once, and a narrow keybar
        // never labels the `Quit` chip, so stop waiting for it as soon as the dashboard is gone.
        let gone = || !self.host.is_alive(&self.host_session).unwrap_or(false);
        let quit_shown = wait_until(Duration::from_secs(3), || {
            gone()
                || self
                    .host
                    .capture_tail(&self.host_session, 200)
                    .is_ok_and(|pane| pane.contains("Quit"))
        });
        if quit_shown && !gone() {
            let _ = send_literal(&self.host_socket, &self.host_session, "q");
        }
        wait_until(Duration::from_secs(3), gone);
    }

    /// Drive the create form (`n`, Tab through Name, [Space ⇒ Autopilot], Tab, goal) and then
    /// submit + press Enter on the new row in ONE atomic `send-keys` (see
    /// [`send_keys_atomic`]). Returns whether every step reported success.
    pub(crate) fn create_then_enter(&self, autopilot: bool) -> bool {
        let (sock, sess) = (self.host_socket.as_str(), self.host_session.as_str());
        // Message is focused first. Three Tabs reach Directory; Autopilot crosses the optional
        // Name row before reaching its Autonomy toggle.
        let mut ok = self.up
            && send_literal(sock, sess, "n")
            && wait_for_pane_text(&self.host, sess, "New session")
            && send_key(sock, sess, "Tab")
            && send_key(sock, sess, "Tab")
            && send_key(sock, sess, "Tab");
        if autopilot {
            // Proves the dial landed on Autopilot BEFORE submitting, so a create that
            // silently stayed on Standard cannot masquerade as a pass. On Autopilot the Goal field
            // exists (m39), so Tab reaches it and the goal is typed.
            ok = ok
                && send_key(sock, sess, "Tab")
                && send_key(sock, sess, "Tab")
                && send_key(sock, sess, "Space")
                && wait_for_pane_text(&self.host, sess, "autopilot")
                && send_key(sock, sess, "Tab")
                && send_literal(sock, sess, "keep the fixture green");
        }
        // On STANDARD there is no Goal field to fill — it is neither shown nor navigable — so the
        // form may be submitted from any row. A Standard session legitimately has no
        // brief; the tests that then turn autopilot on use `turn_autopilot_on`, which supplies one.
        ok && send_keys_atomic(sock, sess, &["Enter", "Enter"])
    }

    /// The created session's registry id (bounded — the create is a real keystroke round
    /// trip), plus its per-session paths and the two tmux session names Enter chooses
    /// between.
    pub(crate) fn created(&self) -> Option<(String, ProjectPaths, String, String)> {
        let find = || {
            Registry::load(&self.reg_path)
                .ok()?
                .projects
                .iter()
                .find(|p| p.root == self.proj)
                .map(|p| p.id.clone())
        };
        wait_until(Duration::from_secs(10), || find().is_some());
        let id = find()?;
        let paths = ProjectPaths::for_session(&self.proj, &id);
        let loop_s = session_name(&id, &self.proj);
        let chat_s = session_name(&id, &self.proj);
        Some((id, paths, loop_s, chat_s))
    }

    /// Kill the spawned `pmd` by pid, then BOTH tmux servers — before any assertion runs,
    /// so a failing assertion can never leak a daemon, a server or an engine session.
    /// Returns any `pmd` still alive afterwards.
    ///
    /// Swept TWICE with a settle in between because [`PmdSibling::DelayedReal`] can be
    /// mid-`sleep` (its shim already matches `pgrep`, but the real daemon it execs is a new
    /// process that would otherwise appear right after a single sweep).
    pub(crate) fn teardown(&self) -> Vec<String> {
        self.stop_dashboard();
        for _ in 0..2 {
            for pid in pmd_pids_for(&self.agent_socket) {
                let _ = Command::new("kill").arg("-TERM").arg(&pid).status();
            }
            std::thread::sleep(Duration::from_millis(
                u64::from(PMD_BOOT_DELAY_S) * 1000 + 300,
            ));
        }
        for sock in [&self.agent_socket, &self.host_socket] {
            // `output()` (not `status()`) so tmux's "error connecting to …" on a socket
            // that never got a server — the `PmdSibling::Missing` case — stays out of the
            // test log.
            let _ = Command::new("tmux")
                .arg("-L")
                .arg(sock)
                .arg("kill-server")
                .output();
        }
        pmd_pids_for(&self.agent_socket)
    }
}

/// Turn autopilot ON from the dashboard: `m`, then TYPE A GOAL in the prompt it opens, then Enter.
///
/// `m` ASKS FOR THE GOAL on its way into Autopilot (m37), and since m38 an EMPTY save is refused
/// when there is no goal on disk — which is the common case here, because a Standard session has no
/// Goal field in the create form at all (m39). So the helper types one; on a session that already
/// had a brief this appends to it, which is harmless (no test asserts the exact brief after a flip)
/// and still lands a non-empty goal.
///
/// Waiting for the overlay before typing is not politeness, it is the difference between a test and
/// a flake: keys that arrive before the prompt has opened land on the DASHBOARD, where `m` would
/// re-open the prompt and the goal text would scatter across the row — and every later `capture`
/// would read the wrong screen.
pub(crate) fn turn_autopilot_on(fx: &EnterFixture, sock: &str, sess: &str) -> bool {
    let goal = send_literal(sock, sess, "m")
        && wait_for_pane_text_within(&fx.host, sess, "goal for", Duration::from_secs(15))
        && send_literal(sock, sess, "fixture goal")
        && send_key(sock, sess, "Enter");
    if !goal {
        return false;
    }
    // A SECOND PROMPT may follow: since m40 `m` also asks for the cadence when the session has
    // none, which is every session created on Standard (the form does not ask there). CONDITIONAL,
    // because a session seeded on disk with a cadence skips straight to the flip — so this waits
    // briefly for the heartbeat prompt and only answers it if it appears. Enter takes the default.
    // "check-in for", the cadence field's title. It read "heartbeat for" until the copy pass
    // unified the UI on ONE word for the interval — and this helper is the reason that rename had
    // teeth: it waited 3s for text that no longer existed, returned without dismissing the field,
    // and the flip silently never happened.
    if wait_for_pane_text_within(&fx.host, sess, "check-in for", Duration::from_secs(3)) {
        return send_key(sock, sess, "Enter");
    }
    true
}

/// Get from "Enter auto-attached us to the daemon's agent" back to the dashboard, with
/// no client left attached — the state `s` is pressed from.
///
/// WAITING FOR THE ATTACH FIRST IS LOAD-BEARING, and getting it wrong cost a debugging
/// round: Autopilot's Enter attaches on the ~500ms idle drain, so a `C-q` sent before then
/// lands on the DASHBOARD — where `handle_key` matches on the key code alone, reads it as
/// plain `q`, and quits pmtui. The host session then ends and every later capture is empty.
pub(crate) fn detach_to_dashboard(fx: &EnterFixture, loop_s: &str) -> bool {
    wait_until(Duration::from_secs(20), || {
        !list_clients(&fx.agent_socket, loop_s).trim().is_empty()
    }) && send_key(&fx.host_socket, &fx.host_session, "C-q")
        && wait_for_pane_text_within(
            &fx.host,
            &fx.host_session,
            "still running",
            Duration::from_secs(15),
        )
        && wait_until(Duration::from_secs(10), || {
            list_clients(&fx.agent_socket, loop_s).trim().is_empty()
        })
}
