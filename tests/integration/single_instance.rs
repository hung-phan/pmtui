//! ACCEPTANCE: the dashboard is single-instance per `(registry, socket)`. A second
//! non-interactive `pmtui` must REFUSE, while an interactive contender may replace the
//! first only through the confirmed lock handoff. Neither route may start "a random one"
//! that races registry edits or disturb project terminals.
//!
//! Only a real second PROCESS proves it. The guard is a `flock` the kernel scopes to an
//! open fd, so an in-process test can model the exclusion (the `lease` unit tests do) but
//! not that `main` actually takes it for its whole life, nor that a rival launch prints
//! the refusal and returns cleanly. Here the first `pmtui` is the shared fixture — hosted
//! in tmux, dashboard painted, so it is demonstrably past the guard and holding its lock —
//! and the second is a plain `Command`. The second never reaches `ratatui::init` (the guard
//! returns before it), so a child with no tty is a faithful stand-in and lets us read its
//! stderr.
//!
//! `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
//! a_second_dashboard_on_the_same_registry_refuses`.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{PmdSibling, enter_fixture};
use crate::probe::{TmuxSocket, tmux_available, wait_for_pane_text_within, wait_until};
use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
use agent_manager::state::{Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, TmuxDriver, session_name};

#[test]
#[ignore]
fn a_second_dashboard_on_the_same_registry_refuses() {
    if !tmux_available() {
        eprintln!("skipping single-instance test: tmux not available");
        return;
    }
    // pmtui #1: the fixture. `Missing` keeps it minimal — no pmd is spawned, so nothing but
    // the dashboard's own lock is under test. Once "Quit" paints (`fx.up`), pmtui is long past
    // the guard at the top of `main`, so it is holding `pmtui_lock_path(reg, agent_socket)`.
    let fx = enter_fixture("solo", PmdSibling::Missing);

    // pmtui #2: the SAME `(registry, socket)`, exactly as a human double-launch would be —
    // byte-identical `--registry`/`--socket`, so it derives the identical lock path. It exits
    // before the TUI, so no tty and no tmux host are needed; capture its stderr.
    let second = Command::new(env!("CARGO_BIN_EXE_pmtui"))
        .args([
            "--socket",
            &fx.agent_socket,
            "--registry",
            &fx.reg_path.display().to_string(),
        ])
        .env("ECC_GATEGUARD", "off")
        .output();

    let pmd_left = fx.teardown();

    // ---- assertions ----
    assert!(
        fx.up,
        "pmtui #1 should paint its dashboard, and thus be holding its single-instance lock"
    );
    let out = second.expect("pmtui #2 should at least launch");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already running"),
        "a second dashboard on the same (registry, socket) must refuse rather than start a rival; \
         stderr was:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "the refusal is a clean early return (Ok(())), not a crash; status was {:?}, stderr:\n{stderr}",
        out.status
    );
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}

#[test]
#[ignore]
fn confirmed_takeover_replaces_only_the_dashboard() {
    if !tmux_available() {
        eprintln!("skipping confirmed takeover test: tmux not available");
        return;
    }
    let fx = enter_fixture("takeover", PmdSibling::Missing);
    assert!(fx.up, "first dashboard did not start");

    let sentinel = format!("takeover-sentinel-{}", std::process::id());
    fx.agent
        .launch_interactive(
            &sentinel,
            &fx.proj,
            &["sh".to_string()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("start unrelated agent terminal");

    let challenger_socket = TmuxSocket::new("pmtui-takeover-host");
    let challenger_session = format!("challenger-{}", std::process::id());
    let command = format!(
        "ECC_GATEGUARD=off '{}' --socket '{}' --registry '{}'",
        env!("CARGO_BIN_EXE_pmtui"),
        fx.agent_socket,
        fx.reg_path.display()
    );
    let launched = Command::new("tmux")
        .args([
            "-L",
            challenger_socket.name(),
            "new-session",
            "-d",
            "-s",
            &challenger_session,
            "-x",
            "120",
            "-y",
            "30",
            &command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("launch takeover challenger");
    assert!(launched.success());
    let challenger = TmuxDriver::with_socket(challenger_socket.name());
    assert!(wait_for_pane_text_within(
        &challenger,
        &challenger_session,
        "Stop it cleanly and take over?",
        Duration::from_secs(5)
    ));
    assert!(send_literal(
        challenger_socket.name(),
        &challenger_session,
        "y"
    ));
    assert!(send_key(
        challenger_socket.name(),
        &challenger_session,
        "Enter"
    ));
    assert!(wait_for_pane_text_within(
        &challenger,
        &challenger_session,
        "SESSIONS",
        Duration::from_secs(8)
    ));
    assert!(
        wait_until(Duration::from_secs(5), || {
            !fx.host.is_alive(&fx.host_session).unwrap_or(false)
                || fx.host.pane_dead(&fx.host_session).unwrap_or(false)
        }),
        "first dashboard did not exit after the confirmed handoff"
    );
    assert!(
        fx.agent.is_alive(&sentinel).unwrap_or(false),
        "dashboard takeover must preserve project terminals"
    );

    let third = Command::new(env!("CARGO_BIN_EXE_pmtui"))
        .args([
            "--socket",
            &fx.agent_socket,
            "--registry",
            &fx.reg_path.display().to_string(),
        ])
        .env("ECC_GATEGUARD", "off")
        .output()
        .expect("launch non-interactive third dashboard");
    assert!(third.status.success());
    assert!(
        String::from_utf8_lossy(&third.stderr).contains("already running"),
        "a non-interactive contender must still refuse: {}",
        String::from_utf8_lossy(&third.stderr)
    );
    assert!(send_key(challenger_socket.name(), &challenger_session, "q"));
    assert!(
        wait_until(Duration::from_secs(5), || {
            !challenger.is_alive(&challenger_session).unwrap_or(false)
                || challenger.pane_dead(&challenger_session).unwrap_or(false)
        }),
        "replacement dashboard did not exit cleanly"
    );
}

#[test]
#[ignore]
fn a_second_registry_cannot_own_or_reap_the_same_tmux_socket() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let reg_a = dir.path().join("a.json");
    let reg_b = dir.path().join("b.json");
    let id = "kept";
    Registry {
        projects: vec![ProjectEntry {
            id: id.into(),
            display_name: None,
            root: root.clone(),
            enabled: true,
            mode: Mode::AgentLoop,
            engine: Some(Engine::Claude),
            worker_model: None,
            initial_prompt: None,
            task_title: None,
            forked_from: None,
            spawned_by: None,
            launch: None,
            conversation_id: None,
            cadence_s: None,
        }],
    }
    .save(&reg_a)
    .unwrap();
    Registry::default().save(&reg_b).unwrap();
    let paths = ProjectPaths::for_session(&root, id);
    agent_manager::state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Standard,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();

    let socket = TmuxSocket::new("am-owner");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name(id, &root);
    driver
        .launch_interactive(
            &session,
            &root,
            &["sh".into(), "-c".into(), "sleep 600".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut first = Command::new(env!("CARGO_BIN_EXE_pmd"))
        .args([
            "--socket",
            socket.name(),
            "--registry",
            &reg_a.display().to_string(),
            "--tick-ms",
            "50",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let owner = agent_manager::lease::socket_owner_lock_path(socket.name());
    let first_up = wait_until(Duration::from_secs(10), || {
        agent_manager::lease::is_held(&owner).ok() == Some(Some(true))
    });
    let second = Command::new(env!("CARGO_BIN_EXE_pmd"))
        .args([
            "--socket",
            socket.name(),
            "--registry",
            &reg_b.display().to_string(),
            "--tick-ms",
            "50",
        ])
        .output()
        .unwrap();
    let one_shot = Command::new(env!("CARGO_BIN_EXE_pmd"))
        .args([
            "--socket",
            socket.name(),
            "--registry",
            &reg_b.display().to_string(),
            "--once",
        ])
        .output()
        .unwrap();
    let still_alive = driver.is_alive(&session).unwrap_or(false);

    let _ = first.kill();
    let _ = first.wait();
    let _ = driver.terminate(&session);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(first_up, "first pmd never acquired the socket owner lock");
    assert!(second.status.success(), "{stderr}");
    assert!(stderr.contains("owned by another registry"), "{stderr}");
    let one_shot_stderr = String::from_utf8_lossy(&one_shot.stderr);
    assert!(one_shot.status.success(), "{one_shot_stderr}");
    assert!(
        one_shot_stderr.contains("owned by another registry"),
        "--once must honor socket ownership too: {one_shot_stderr}"
    );
    assert!(
        still_alive,
        "second registry reaped the first registry's terminal"
    );
}
