use std::process::Command;

use agent_manager::registry::Registry;
use agent_manager::state::ProjectPaths;
use agent_manager::tmux::{Driver, TmuxDriver, session_name};

use crate::keystrokes::{send_key, send_literal, send_mouse_click, send_mouse_wheel};
use crate::probe::{TmuxSocket, kill_server_and_socket, tmux_available, wait_for_pane_text};
use crate::seed::seed_standard_loop_session;

#[test]
#[ignore]
fn quick_switch_and_rename_preserve_project_terminals_and_stable_identity() {
    if !tmux_available() {
        eprintln!("skipping session-switcher test: tmux not available");
        return;
    }

    let dir = tempfile::tempdir().expect("scratch directory");
    let alpha_root = dir.path().join("alpha-project");
    let beta_root = dir.path().join("beta-project");
    std::fs::create_dir_all(&alpha_root).expect("alpha root");
    std::fs::create_dir_all(&beta_root).expect("beta root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, &alpha_root, "alpha");
    seed_standard_loop_session(&registry, &beta_root, "beta");
    let registry_before = std::fs::read(&registry).expect("registry bytes");

    let socket = TmuxSocket::new("am-switcher");
    let driver = TmuxDriver::with_socket(socket.name());
    let alpha_session = session_name("alpha", &alpha_root);
    let beta_session = session_name("beta", &beta_root);
    for (session, root) in [
        (alpha_session.as_str(), alpha_root.as_path()),
        (beta_session.as_str(), beta_root.as_path()),
    ] {
        driver
            .launch_interactive(
                session,
                root,
                &["sh".into(), "-c".into(), "while :; do sleep 1; done".into()],
                &agent_manager::tmux::ManagedEnv::default(),
            )
            .expect("launch project terminal");
    }

    let host = format!("switcher-host-{}", std::process::id());
    let command = format!(
        "exec '{}' --socket '{}' --registry '{}'",
        env!("CARGO_BIN_EXE_pmtui"),
        socket.name(),
        registry.display(),
    );
    let launched = Command::new("tmux")
        .args([
            "-L",
            socket.name(),
            "new-session",
            "-d",
            "-s",
            &host,
            "-x",
            "120",
            "-y",
            "35",
            &command,
        ])
        .status()
        .is_ok_and(|status| status.success());
    let ready = launched
        && wait_for_pane_text(&driver, &host, "alpha")
        && wait_for_pane_text(&driver, &host, "beta");
    let keyboard = ready
        && send_literal(socket.name(), &host, "/")
        && wait_for_pane_text(&driver, &host, "Switch session")
        && send_literal(socket.name(), &host, "beta")
        && wait_for_pane_text(&driver, &host, "1 match")
        && send_key(socket.name(), &host, "Enter")
        && wait_for_pane_text(&driver, &host, "selected beta");

    // 120x35 with two results gives an 84x9 centered overlay. Its first result row is y=16.
    let pointer = keyboard
        && send_literal(socket.name(), &host, "/")
        && wait_for_pane_text(&driver, &host, "2 matches")
        && send_mouse_click(socket.name(), &host, 24, 16)
        && wait_for_pane_text(&driver, &host, "selected alpha");
    let registry_after_switch = std::fs::read(&registry).expect("registry bytes after switching");
    let state_dir = ProjectPaths::for_session(&alpha_root, "alpha").state_dir();
    let rename = pointer
        && send_literal(socket.name(), &host, "R")
        && wait_for_pane_text(&driver, &host, "Rename")
        && send_literal(socket.name(), &host, "Alpha release")
        && send_key(socket.name(), &host, "Enter")
        && wait_for_pane_text(&driver, &host, "renamed alpha to Alpha release")
        && wait_for_pane_text(&driver, &host, "Alpha release");
    // The open composer is titled with the renamed label and stays bound to its session: a
    // wheel notch over SESSIONS (column 10, row 4 is beta's row under alpha) must not move the
    // selection under the field. After Esc the dormant shelf belongs to the selected row, so it
    // still showing alpha's parked draft proves the selection never moved.
    let composing = rename
        && send_literal(socket.name(), &host, "s")
        && wait_for_pane_text(&driver, &host, "Keep draft")
        && wait_for_pane_text(&driver, &host, "Message \u{b7} Alpha release")
        && send_literal(socket.name(), &host, "hold for alpha")
        && wait_for_pane_text(&driver, &host, "hold for alpha");
    let composer_pinned = composing
        && send_mouse_wheel(socket.name(), &host, 10, 4, true)
        && send_key(socket.name(), &host, "Escape")
        && wait_for_pane_text(&driver, &host, "draft saved for alpha")
        && wait_for_pane_text(&driver, &host, "draft \u{b7} hold for alpha");
    let sessions_after = driver.list_sessions();
    let projects_alive = driver.is_alive(&alpha_session).unwrap_or(false)
        && driver.is_alive(&beta_session).unwrap_or(false)
        && !driver.has_clients(&alpha_session).unwrap_or(true)
        && !driver.has_clients(&beta_session).unwrap_or(true);
    let pane = driver.capture_tail(&host, 120).unwrap_or_default();
    let renamed = Registry::load(&registry).expect("registry after rename");

    let _ = send_literal(socket.name(), &host, "q");
    kill_server_and_socket(socket.name());

    assert!(ready, "pmtui did not show both sessions:\n{pane}");
    assert!(keyboard, "keyboard quick switch failed:\n{pane}");
    assert!(pointer, "pointer quick switch failed:\n{pane}");
    assert_eq!(
        registry_after_switch, registry_before,
        "quick switching must not rewrite registry state"
    );
    assert!(rename, "session rename failed:\n{pane}");
    assert!(
        composing,
        "composer did not open under the renamed label:\n{pane}"
    );
    assert!(
        composer_pinned,
        "a sessions wheel moved the selection under the open composer:\n{pane}"
    );
    let alpha = renamed
        .projects
        .iter()
        .find(|entry| entry.id == "alpha")
        .expect("stable alpha entry");
    assert_eq!(alpha.display_name.as_deref(), Some("Alpha release"));
    assert!(state_dir.exists(), "rename moved or removed stable state");
    assert!(
        projects_alive,
        "switching touched a project terminal: {sessions_after:?}"
    );
    assert_eq!(
        sessions_after
            .iter()
            .filter(|name| name.starts_with("pm-"))
            .count(),
        2,
        "expected exactly the two original project terminals: {sessions_after:?}"
    );
}
