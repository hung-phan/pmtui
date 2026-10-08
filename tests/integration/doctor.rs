use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use agent_manager::job::AgentLoopState;
use agent_manager::registry::Engine;
use agent_manager::state::ProjectPaths;
use agent_manager::tmux::session_name;
use tempfile::TempDir;

use crate::probe::{TmuxSocket, tmux_available};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[derive(Debug, PartialEq, Eq)]
struct FileSnapshot {
    bytes: Option<Vec<u8>>,
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, FileSnapshot> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, FileSnapshot>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        out.insert(
            relative,
            FileSnapshot {
                bytes: metadata.is_file().then(|| fs::read(path).unwrap()),
                len: metadata.len(),
                modified: metadata.modified().ok(),
                #[cfg(unix)]
                inode: metadata.ino(),
                #[cfg(unix)]
                mode: metadata.mode(),
            },
        );
        if metadata.is_dir() {
            let mut children: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            children.sort();
            for child in children {
                visit(root, &child, out);
            }
        }
    }

    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .output()
        .unwrap()
}

fn start_session(socket: &str, name: &str) {
    let output = tmux(socket, &["new-session", "-d", "-s", name, "exec sleep 120"]);
    assert!(
        output.status.success(),
        "failed to start {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tmux_snapshot(socket: &str) -> (Vec<u8>, Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let panes = tmux(
        socket,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{pane_pid}\t#{pane_current_command}\t#{pane_dead}\t#{pane_start_command}",
        ],
    );
    assert!(panes.status.success());
    let clients = tmux(
        socket,
        &["list-clients", "-F", "#{client_pid}\t#{session_name}"],
    );
    let clients = if clients.status.success() {
        clients.stdout
    } else {
        Vec::new()
    };
    let sessions = tmux(socket, &["list-sessions", "-F", "#{session_name}"]);
    assert!(sessions.status.success());
    let mut captures = BTreeMap::new();
    for name in String::from_utf8_lossy(&sessions.stdout).lines() {
        let capture = tmux(socket, &["capture-pane", "-p", "-t", name, "-S", "-20"]);
        assert!(capture.status.success());
        captures.insert(name.to_owned(), capture.stdout);
    }
    (panes.stdout, clients, captures)
}

#[cfg(unix)]
fn install_probe(dir: &Path, name: &str, sentinel: &Path) -> PathBuf {
    let binary = dir.join(name);
    fs::write(
        &binary,
        format!("#!/bin/sh\nprintf called > '{}'\n", sentinel.display()),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    binary
}

#[test]
#[ignore = "requires real tmux"]
fn doctor_is_read_only_across_files_terminals_and_notifications() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = TempDir::new().unwrap();
    let socket = TmuxSocket::new("pmd-doctor");
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::create_dir_all(paths.state_dir()).unwrap();
    fs::write(paths.config(), b"{}").unwrap();
    fs::write(paths.control(), b"{}").unwrap();
    fs::write(
        paths.pmstate(),
        serde_json::to_vec_pretty(&AgentLoopState::fresh(Engine::Claude, None, 10)).unwrap(),
    )
    .unwrap();
    fs::write(paths.needs_you(), br#"{"seq":1,"state":"working"}"#).unwrap();
    fs::write(paths.checkpoint(), br#"{"continuity":"untrusted"}"#).unwrap();
    fs::write(paths.stops(), b"[]").unwrap();
    fs::create_dir_all(paths.daemon_dir()).unwrap();
    fs::write(paths.input_lock(), b"lock sentinel").unwrap();
    fs::write(paths.chat_lock(), b"chat sentinel").unwrap();
    let done_signal = paths.done_signal(1);
    fs::create_dir_all(done_signal.parent().expect("done signal has a parent")).unwrap();
    fs::write(done_signal, b"done sentinel").unwrap();
    fs::create_dir_all(
        paths
            .canonical_worker_skill_file()
            .parent()
            .expect("skill has a parent"),
    )
    .unwrap();
    fs::write(
        paths.canonical_worker_skill_file(),
        agent_manager::skills::WORKER_SKILL_MD,
    )
    .unwrap();

    let registry = dir.path().join("registry.json");
    fs::write(
        &registry,
        serde_json::to_vec_pretty(&serde_json::json!({
            "projects": [{
                "id": "alpha",
                "root": root,
                "enabled": false,
                "engine": "claude"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let registered = session_name("alpha", &root);
    let unregistered = format!("pm-unregistered-{}", std::process::id());
    let supervisor = format!("pmsup-stale-{}", std::process::id());
    for name in [&registered, &unregistered, &supervisor] {
        start_session(socket.name(), name);
    }

    let bin_dir = dir.path().join("bin");
    fs::create_dir(&bin_dir).unwrap();
    let sentinels = [
        ("notify-send", dir.path().join("notification-was-sent")),
        ("claude", dir.path().join("claude-was-invoked")),
        ("codex", dir.path().join("codex-was-invoked")),
    ];
    #[cfg(unix)]
    for (binary, sentinel) in &sentinels {
        install_probe(&bin_dir, binary, sentinel);
    }
    let mut path_parts = vec![bin_dir.clone()];
    path_parts.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(path_parts).unwrap();

    let files_before = snapshot_tree(dir.path());
    let tmux_before = tmux_snapshot(socket.name());

    let text = Command::new(env!("CARGO_BIN_EXE_pmd"))
        .args([
            "doctor",
            "--registry",
            registry.to_str().unwrap(),
            "--socket",
            socket.name(),
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        text.status.success(),
        "text doctor failed: {}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(String::from_utf8_lossy(&text.stdout).contains("pmd doctor: WARN"));

    let json = Command::new(env!("CARGO_BIN_EXE_pmd"))
        .args([
            "doctor",
            "--registry",
            registry.to_str().unwrap(),
            "--socket",
            socket.name(),
            "--json",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "JSON doctor failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["status"], "warn");

    assert_eq!(snapshot_tree(dir.path()), files_before);
    assert_eq!(tmux_snapshot(socket.name()), tmux_before);
    for (binary, sentinel) in &sentinels {
        assert!(
            !sentinel.exists(),
            "doctor must never invoke external binary {binary}"
        );
    }

    let sessions = tmux(socket.name(), &["list-sessions", "-F", "#{session_name}"]);
    let names = String::from_utf8_lossy(&sessions.stdout);
    for name in [registered, unregistered, supervisor] {
        assert!(names.lines().any(|line| line == name), "{name} was removed");
    }
}
