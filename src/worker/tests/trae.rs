use super::*;
use std::path::PathBuf;

use crate::registry::Engine;

#[test]
fn trae_headless_command_uses_traex_exec_json_with_pinned_or_resumed_session() {
    let fresh = build_command(
        Engine::Trae,
        "work",
        PermissionMode::Auto,
        &[PathBuf::from("/repo/extra")],
        Resume::Fresh {
            session_id: Some("trae-session".into()),
        },
    );
    assert_eq!(
        fresh,
        vec![
            "traex",
            "exec",
            "--sandbox",
            "workspace-write",
            "--skip-git-repo-check",
            "--json",
            "--session-id",
            "trae-session",
            "--add-dir",
            "/repo/extra",
            "--",
            "work",
        ]
    );

    let resumed = build_command(
        Engine::Trae,
        "continue",
        PermissionMode::Auto,
        &[],
        Resume::Continue("trae-session".into()),
    );
    assert!(
        resumed
            .windows(3)
            .any(|w| w == ["exec", "resume", "trae-session"]),
        "{resumed:?}"
    );
    assert!(
        !fresh.iter().chain(resumed.iter()).any(|arg| matches!(
            arg.as_str(),
            "-p" | "--output-format" | "--permission-mode" | "-y" | "--yolo" | "bypass_permissions"
        )),
        "TRAE exec launches must use the 2.0 contract without unsafe or v1-only flags: fresh={fresh:?}, resumed={resumed:?}"
    );
}

#[test]
fn loop_command_trae_gets_auto_permission_mode_without_sandbox() {
    let sig = PathBuf::from("/proj/.project-state/sessions/s/.daemon/turn-complete");
    let argv = build_loop_command(
        Engine::Trae,
        &Resume::Continue("trae-session".into()),
        &[PathBuf::from("/proj/root")],
        Some(sig.as_path()),
        Some("seed-1"),
    );

    assert_eq!(&argv[..4], ["env", "-u", "CLAUDECODE", "traex"]);
    assert!(argv.windows(2).any(|w| w == ["-m", "seed-1"]), "{argv:?}");
    assert!(
        argv.windows(2).any(|w| w == ["--permission-mode", "auto"]),
        "{argv:?}"
    );
    assert!(
        argv.windows(2).any(|w| w == ["resume", "trae-session"]),
        "{argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "--sandbox"),
        "TraeX rejects permission-mode and sandbox overrides together: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| matches!(
            arg.as_str(),
            "-y" | "--yolo" | "bypass_permissions" | "--dangerously-bypass-hook-trust"
        )),
        "Autopilot must not bypass TraeX safety gates: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "--add-dir"),
        "the cwd is already the workspace; add-dir support is not part of the measured TraeX loop contract: {argv:?}"
    );
}

#[test]
fn loop_command_trae_injects_a_stable_stop_hook() {
    let sig = PathBuf::from("/proj/.project-state/sessions/s/.daemon/turn-complete");
    let argv = build_loop_command(
        Engine::Trae,
        &Resume::Fresh {
            session_id: Some("trae-session".into()),
        },
        &[],
        Some(sig.as_path()),
        None,
    );

    let config_at = argv.iter().position(|arg| arg == "-c").expect("-c present");
    let config = &argv[config_at + 1];
    assert!(config.contains("hooks.Stop"), "{config}");
    assert!(
        config.contains("PMTUI_BIN") && config.contains("hook turn-complete"),
        "the trusted command should be stable, path-free, and PMTUI_BIN-only: {config}"
    );
    assert!(!config.contains(":-pmtui"), "{config}");
    assert!(
        !config.contains(&*sig.to_string_lossy()),
        "TraeX hook trust is hash-based, so per-session paths must stay out of the command: {config}"
    );
}
