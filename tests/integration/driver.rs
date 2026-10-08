//! The real `TmuxDriver`'s own primitives against a real server on a private
//! socket: spawn a step and reap it, launch an interactive session (twice, to prove
//! idempotence), the bare `Ctrl+q` detach binding, and `capture_tail`'s pane target.
//! Nothing above the driver takes part, so a failure here is the shim itself.

use std::process::Command;
use std::time::{Duration, Instant};

use agent_manager::tmux::{
    Driver, LAUNCH_COMMAND_MAX_BYTES, Observation, TmuxDriver, launch_command, observe,
    session_name,
};

use crate::probe::TmuxSocket;
use crate::probe::tmux_available;

/// Focused coverage of the *real* `TmuxDriver`: spawn a trivial step on a private
/// socket, poll `observe` to `Completed`, and assert the done-signal contents and
/// that the session is gone. Without this, the real driver's spawn/observe/
/// terminate shim is exercised only by the two full-loop tests above.
#[test]
fn tmux_driver_spawns_observes_and_reaps_a_trivial_step() {
    if !tmux_available() {
        eprintln!("skipping tmux_driver focused test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let done = dir.path().join("steps/0.done");
    let log = dir.path().join("steps/0.log");
    let socket = TmuxSocket::new("pm-drv");
    let driver = TmuxDriver::with_socket(socket.name());

    let session = "pmd-drv-0";
    let handle = driver
        .spawn_step(
            session,
            dir.path(),
            &["sh".into(), "-c".into(), "exit 7".into()],
            &done,
            &log,
        )
        .expect("spawn");

    // Poll until the wrapper writes the exit code (bounded).
    let mut obs = Observation::Running;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        obs = observe(&driver, &handle).expect("observe");
        if obs != Observation::Running {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = driver.terminate(session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    assert_eq!(
        obs,
        Observation::Completed { exit_code: 7 },
        "real driver should observe the exit code the wrapper recorded"
    );
    let recorded = std::fs::read_to_string(&done).unwrap();
    assert_eq!(
        recorded.trim(),
        "7",
        "done-signal holds the child's exit code"
    );
    assert!(
        !driver.is_alive(session).unwrap(),
        "session should be gone after completion + terminate"
    );
}

/// The interactive path pmtui uses: launch a persistent session (a long-lived
/// `sh` stands in for claude/codex), confirm it's alive and that launching again
/// is idempotent, then reap it. Attach itself needs a real TTY, so this covers
/// everything up to the attach hand-off.
#[test]
fn tmux_driver_launches_and_reaps_an_interactive_session() {
    if !tmux_available() {
        eprintln!("skipping interactive launch test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-int");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("demo", dir.path());

    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");
    assert!(
        driver.is_alive(&session).unwrap(),
        "interactive session should be alive after launch"
    );
    // Idempotent: a second launch on a live session is a no-op, not an error.
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("relaunch is idempotent");

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();
    assert!(
        !driver.is_alive(&session).unwrap_or(true),
        "session should be gone after terminate"
    );
}

/// ACCEPTANCE: the launch budget really launches. A command exactly at
/// `LAUNCH_COMMAND_MAX_BYTES`, run from a working directory about 3 KiB deep under a
/// production-shaped session name, starts under real tmux; one byte more is refused with its
/// size instead of tmux's silent exit 1.
#[test]
#[ignore]
fn a_launch_at_the_command_budget_starts_and_one_byte_more_is_refused() {
    if !tmux_available() {
        eprintln!("skipping launch budget test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let deep = (0..12).fold(dir.path().to_path_buf(), |path, i| {
        path.join(format!("{i:02}{}", "d".repeat(248)))
    });
    std::fs::create_dir_all(&deep).expect("deep working directory");
    assert!(deep.as_os_str().len() > 3_000);
    let socket = TmuxSocket::new("pm-budget");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("launch-budget-probe", &deep);
    let argv = |filler: usize| -> Vec<String> {
        vec![
            "sh".into(),
            "-c".into(),
            "sleep 30".into(),
            "x".repeat(filler),
        ]
    };
    let filler = LAUNCH_COMMAND_MAX_BYTES - launch_command(&argv(0)).len();
    assert_eq!(
        launch_command(&argv(filler)).len(),
        LAUNCH_COMMAND_MAX_BYTES
    );

    let at_budget = driver.launch_interactive(
        &session,
        &deep,
        &argv(filler),
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let alive = driver.is_alive(&session).unwrap_or(false);
    let _ = driver.terminate(&session);
    let over = driver.launch_interactive(
        &session,
        &deep,
        &argv(filler + 1),
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let _ = Command::new("tmux")
        .args(["-L", socket.name(), "kill-server"])
        .status();

    at_budget.expect("a launch command at the budget must start under real tmux");
    assert!(alive, "the at-budget session did not stay up");
    let error = over.expect_err("one byte over the budget must be refused");
    assert!(error.to_string().contains("after shell quoting"), "{error}");
}

/// Launching an interactive session binds bare `Ctrl+q` to `detach-client` on the
/// private server, so a human pops back to pmtui with one keystroke. Asserts the
/// root-table binding is registered (proving the mechanism; the actual keystroke
/// needs a TTY, which `attach` handles).
#[test]
fn interactive_launch_binds_ctrl_q_to_detach() {
    if !tmux_available() {
        eprintln!("skipping ctrl-q bind test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-cq");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("cq", dir.path());
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");

    let keys = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .args(["list-keys", "-T", "root"])
        .output()
        .expect("list-keys");
    let out = String::from_utf8_lossy(&keys.stdout);

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    let bound = out
        .lines()
        .any(|l| l.contains("C-q") && l.contains("detach-client"));
    assert!(
        bound,
        "launch_interactive should bind C-q -> detach-client in the root table; got:\n{out}"
    );
}

/// A DETACHED agent pane is created wider than tmux's 80x24 default, so the dashboard
/// preview fills its panel instead of showing a narrow column (user: *"why isn't it
/// fullscreen?"* → *"Can we do it simple??"*). Proven against a real server because a
/// detached pane's size — the thing `capture-pane` returns — only exists in tmux, never in
/// FakeDriver. Also asserts `window-size` stays `latest`: that is the property that keeps
/// `Enter` (attach) resizing the pane to the human's real terminal, so a fixed create size
/// does not letterbox the session when it is actually opened.
#[test]
fn interactive_launch_sizes_the_pane_wider_than_the_80x24_default() {
    if !tmux_available() {
        eprintln!("skipping pane-size test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-size");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("size", dir.path());
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");

    let geom = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .args([
            "display-message",
            "-p",
            "-t",
            &session,
            "#{pane_width} #{pane_height} #{window-size}",
        ])
        .output()
        .expect("display-message");
    let out = String::from_utf8_lossy(&geom.stdout).trim().to_string();

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    let mut fields = out.split_whitespace();
    let width: u16 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let _height = fields.next();
    let win_size = fields.next().unwrap_or("");
    // The exact inverse of the bug: the pane used to be tmux's 80-wide default, which under-fills
    // a wide preview. `> 80` proves the `-x` flag took effect without hard-coding the tunable
    // default (`PM_AGENT_COLS` can move it).
    assert!(
        width > 80,
        "the agent pane should be created wider than the 80-col default; geometry was: {out:?}"
    );
    assert_eq!(
        win_size, "latest",
        "window-size must stay `latest` so attaching (Enter) still resizes to the real terminal; \
         geometry was: {out:?}"
    );
}

/// `resize_window` reflows a DETACHED pane (proven against a real server — a pane's size
/// only exists in tmux, not in FakeDriver), and `set_window_size_auto` restores `latest`
/// WITHOUT shrinking it. That two-move pattern is what lets pmtui fit the pane to the
/// preview yet still hand a human's `Enter` their FULL terminal: fit sets `manual`, and the
/// attach path flips `latest` back so the attaching client governs.
#[test]
fn resize_window_reflows_the_pane_and_set_auto_restores_latest_without_shrinking() {
    if !tmux_available() {
        eprintln!("skipping resize test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-rz");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("rz", dir.path());
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");

    // The preview-fit move: reflow the detached pane to a specific size.
    driver.resize_window(&session, 132, 40).expect("resize");
    let after_resize = pane_geom(socket.name(), &session);
    // The pre-attach move: re-enable auto without changing the current size.
    driver.set_window_size_auto(&session).expect("set auto");
    let after_auto = pane_geom(socket.name(), &session);

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    assert_eq!(
        after_resize,
        ("132".to_string(), "40".to_string(), "manual".to_string()),
        "resize_window should set the pane to 132x40 and (tmux's side effect) flip window-size \
         to manual; got {after_resize:?}"
    );
    assert_eq!(
        after_auto,
        ("132".to_string(), "40".to_string(), "latest".to_string()),
        "set_window_size_auto must re-enable `latest` but KEEP the size, so a later attach \
         resizes to the real terminal rather than this preview size; got {after_auto:?}"
    );
}

/// `clear_history` drops a pane's scrollback while preserving its live frame. A five-row pane
/// is filled with 30 marker lines so a deep capture sees both history and the live rows, then
/// the real driver clears history and the same capture must see only the five live rows.
/// FakeDriver cannot exhibit either side of this contract.
#[test]
fn clear_history_collapses_deep_scrollback_to_one_live_frame() {
    if !tmux_available() {
        eprintln!("skipping clear-history test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-ch");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("ch", dir.path());
    // Start this server without the user's tmux config and pin a nonzero history limit before
    // creating the pane. A private socket still loads ~/.tmux.conf by default, so an operator's
    // `history-limit 0` would otherwise make the premise impossible.
    let bootstrap = "pm-ch-bootstrap";
    assert!(
        Command::new("tmux")
            .args([
                "-L",
                socket.name(),
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                bootstrap,
            ])
            .status()
            .expect("start isolated tmux server")
            .success(),
        "isolated tmux server should start"
    );
    for (option, value) in [("exit-empty", "off"), ("history-limit", "1000")] {
        assert!(
            Command::new("tmux")
                .args(["-L", socket.name(), "set-option", "-g", option, value])
                .status()
                .expect("configure isolated tmux server")
                .success(),
            "tmux should accept {option}={value}"
        );
    }
    assert!(
        Command::new("tmux")
            .args(["-L", socket.name(), "kill-session", "-t", bootstrap])
            .status()
            .expect("remove bootstrap session")
            .success(),
        "bootstrap session should be removable"
    );
    // Wait for the test to establish the five-row pane before printing. The last marker has
    // no newline so all five live rows contain markers after the preceding 25 enter history.
    let script = "while [ ! -f emit ]; do sleep 0.05; done; \
                  n=1; while [ \"$n\" -lt 30 ]; do echo \"CLAUDE_FRAME $n\"; n=$((n + 1)); done; \
                  printf 'CLAUDE_FRAME 30'; exec sleep 3600";
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into(), "-c".into(), script.into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");

    driver.resize_window(&session, 70, 5).expect("resize");
    std::fs::write(dir.path().join("emit"), "").expect("release emitter");
    let started = Instant::now();
    let mut stacked = 0;
    while started.elapsed() < Duration::from_secs(5) {
        stacked = frame_count(socket.name(), &session);
        if stacked == 30 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    driver.clear_history(&session).expect("clear");
    let after_clear = frame_count(socket.name(), &session);

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    assert_eq!(
        stacked, 30,
        "the emitter should finish all 30 marker lines before history is cleared"
    );
    assert_eq!(
        after_clear, 5,
        "clear_history must collapse the deep capture to exactly the one live 5-line frame, \
         got {after_clear}"
    );
}

/// Two facts the preview's scroll relies on, neither of which FakeDriver can exhibit (both are
/// about a REAL pane's scrollback):
///
///  1. DEPTH controls how far back you can see. A shallow capture cannot reach the top of a long
///     history; a deep one can. That is why the preview's capture depth FOLLOWS the scroll —
///     user: *"user should be able to scroll all the way to the top"*.
///  2. `resize_window` PRESERVES that scrollback. That is why fitting the pane to the preview must
///     NOT `clear_history` on every fit — user: *"i cannot scroll in main panel or after enter the
///     session"*. (The contrast — `clear_history` DROPS it — is
///     `clear_history_collapses_deep_scrollback_to_one_live_frame`.)
#[test]
fn scrollback_depth_reaches_the_top_and_survives_a_resize() {
    if !tmux_available() {
        eprintln!("skipping scrollback test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-sb");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("sb", dir.path());
    // Print 250 uniquely-marked lines into the normal buffer (they scroll into history), then idle.
    let script = "i=1; while [ $i -le 250 ]; do echo LINE_$i; i=$((i+1)); done; \
                  while :; do sleep 0.2; done";
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &["sh".into(), "-c".into(), script.into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");
    std::thread::sleep(Duration::from_millis(500));

    // Does a capture of the given DEPTH contain `needle` as a whole line?
    let has = |depth: usize, needle: &str| -> bool {
        driver
            .capture_tail(&session, depth)
            .unwrap_or_default()
            .lines()
            .any(|l| l.trim() == needle)
    };

    // (1) A shallow capture (like following the tail) cannot reach the top; a deep one reaches both
    //     the top and the tail — the whole premise of scroll-to-top.
    let shallow_reaches_top = has(30, "LINE_1");
    let deep_reaches_top = has(400, "LINE_1");
    let deep_reaches_tail = has(400, "LINE_250");
    // (2) A resize must PRESERVE the scrollback, so a deep capture STILL reaches the top afterwards.
    driver.resize_window(&session, 70, 20).expect("resize");
    std::thread::sleep(Duration::from_millis(300));
    let top_survives_resize = has(400, "LINE_1");

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    assert!(
        !shallow_reaches_top,
        "a shallow capture must NOT reach the top of a 250-line history"
    );
    assert!(
        deep_reaches_top && deep_reaches_tail,
        "a deep capture must reach BOTH the top (LINE_1) and the tail (LINE_250) — the premise of \
         scrolling all the way to the top"
    );
    assert!(
        top_survives_resize,
        "resize_window must PRESERVE scrollback (LINE_1 still reachable); otherwise fitting the \
         pane to the preview would silently kill the scroll"
    );
}

/// How many `CLAUDE_FRAME` marker lines a DEEP capture (`-S -600`, as the preview does) pulls
/// from the session's pane — the count that is > one frame when stale frames have stacked.
fn frame_count(socket: &str, session: &str) -> usize {
    let out = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(["capture-pane", "-p", "-S", "-600", "-t"])
        .arg(format!("={session}:"))
        .output()
        .expect("capture-pane");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("CLAUDE_FRAME"))
        .count()
}

/// The `(pane_width, pane_height, window-size)` of a session, as strings — the geometry
/// the resize tests assert on.
fn pane_geom(socket: &str, session: &str) -> (String, String, String) {
    let out = Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args([
            "display-message",
            "-p",
            "-t",
            session,
            "#{pane_width} #{pane_height} #{window-size}",
        ])
        .output()
        .expect("display-message");
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut f = s.split_whitespace();
    (
        f.next().unwrap_or("").to_string(),
        f.next().unwrap_or("").to_string(),
        f.next().unwrap_or("").to_string(),
    )
}

/// `capture_tail` against a real pane — exercises the `=name:` exact pane target
/// (a bare `=name` is rejected by capture-pane) and confirms it reads pane text.
#[test]
fn tmux_driver_capture_tail_reads_pane_text() {
    if !tmux_available() {
        eprintln!("skipping capture_tail test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-cap");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = "pmi-cap";
    driver
        .launch_interactive(
            session,
            dir.path(),
            &[
                "sh".into(),
                "-c".into(),
                "echo CAP_MARKER_42; sleep 300".into(),
            ],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");

    let mut tail = String::new();
    for _ in 0..50 {
        tail = driver.capture_tail(session, 20).unwrap_or_default();
        if tail.contains("CAP_MARKER_42") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = driver.terminate(session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();
    assert!(
        tail.contains("CAP_MARKER_42"),
        "capture_tail should read the pane, got {tail:?}"
    );
}

#[test]
fn reap_owned_sessions_kills_only_transient_consults_and_preserves_the_project_terminal() {
    if !tmux_available() {
        eprintln!("skipping reap test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-reap");
    let driver = TmuxDriver::with_socket(socket.name());

    for name in ["pm-bot-aaaa1111", "pmsup-bot-1", "some-unrelated"] {
        driver
            .launch_interactive(
                name,
                dir.path(),
                &["sh".into(), "-c".into(), "sleep 600".into()],
                &agent_manager::tmux::ManagedEnv::default(),
            )
            .expect("launch");
    }
    assert!(driver.is_alive("pm-bot-aaaa1111").unwrap());

    let reaped = agent_manager::daemon::reap_owned_sessions(&driver);
    assert_eq!(
        reaped, 1,
        "exactly the transient supervisor session was reaped"
    );
    assert!(
        !driver.is_alive("pmsup-bot-1").unwrap(),
        "the transient consult is gone"
    );
    assert!(
        driver.is_alive("pm-bot-aaaa1111").unwrap(),
        "the project terminal survives because pmd is only its driver"
    );
    assert!(
        driver.is_alive("some-unrelated").unwrap(),
        "unrelated sessions survive"
    );
}
