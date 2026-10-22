//! ACCEPTANCE for `pmtui message`: one agent reaches another agent's pane.
//!
//! Only a real pane can prove this. `FakeDriver` cannot show that the bytes arrive, that they
//! arrive at the RIGHT pane, that the envelope is what the recipient actually sees, or that the
//! trailing `Enter` submits them — and the delivery gate it shares with the `s` key exists because
//! typing into the wrong pane state is the riskiest action in the system.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
use agent_manager::tmux::{self, Driver, TmuxDriver};

use crate::probe::{TmuxSocket, tmux_available, wait_until};
use crate::stubs::{DIALOG_STUB, ECHOING_COMPOSER_STUB};

/// A registry holding one row per `(id, root)`, written where the CLI will read it.
fn write_registry(path: &Path, rows: &[(&str, &Path)]) {
    let mut registry = Registry::default();
    for (id, root) in rows {
        registry.projects.push(ProjectEntry {
            id: (*id).to_string(),
            display_name: None,
            root: root.to_path_buf(),
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
        });
    }
    registry.save(path).expect("write the scratch registry");
}

/// Start `script` as a session named exactly what pmd and pmtui would call this row's pane, so
/// the CLI's own `tmux::session_name` has to agree with it for anything to be delivered.
fn start_stub_pane(socket: &str, id: &str, root: &Path, script: &Path) -> String {
    let session = tmux::session_name(id, root);
    let ok = Command::new("tmux")
        .args([
            "-L",
            socket,
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "80",
            "-y",
            "24",
        ])
        .arg(script)
        .status()
        .expect("tmux new-session")
        .success();
    assert!(ok, "could not start the stub pane {session}");
    session
}

fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// Run `pmtui message …` as the agent of `from` would: only the three variables a managed pane
/// carries, and `$TMUX` naming the scratch server. `env_clear` is the point — anything it needs
/// that is NOT here would be read from the developer's own environment, and the test would pass
/// while the real command failed inside a sandbox.
fn run_message(from: &str, state_dir: &Path, socket: &str, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_pmtui"))
        .arg("message")
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("PMTUI_SESSION", from)
        .env("PMTUI_STATE_DIR", state_dir)
        .env("TMUX", format!("/tmp/tmux-x/{socket},1,0"))
        .output()
        .expect("run pmtui message");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// ACCEPTANCE: a message from one session arrives in another session's pane, submitted, wearing
/// the envelope that tells the recipient who sent it and how to answer.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored a_message_reaches`.
#[test]
#[ignore]
fn a_message_reaches_the_other_sessions_pane_with_its_envelope() {
    if !tmux_available() {
        eprintln!("skipping message test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sender_root = dir.path().join("sender");
    let peer_root = dir.path().join("peer");
    let sender_state = sender_root.join(".project-state/sessions/sender-1");
    std::fs::create_dir_all(&sender_state).unwrap();
    std::fs::create_dir_all(&peer_root).unwrap();
    let registry = dir.path().join("registry.json");
    write_registry(&registry, &[("sender", &sender_root), ("peer", &peer_root)]);

    let socket = TmuxSocket::new("pm-msg");
    let script = write_script(dir.path(), "peer.sh", ECHOING_COMPOSER_STUB);
    let peer_session = start_stub_pane(socket.name(), "peer", &peer_root, &script);
    let agent = TmuxDriver::with_socket(socket.name());
    let up = wait_until(Duration::from_secs(10), || {
        agent.is_alive(&peer_session).unwrap_or(false)
    });

    let (code, said) = run_message(
        "sender",
        &sender_state,
        socket.name(),
        &[
            "--to",
            "peer",
            "--message",
            "ping from sender",
            "--registry",
            registry.to_str().unwrap(),
        ],
    );
    // The stub echoes each SUBMITTED line, so this is proof the bytes were submitted rather than
    // merely typed into the composer.
    let echoed = wait_until(Duration::from_secs(20), || {
        agent
            .capture_tail(&peer_session, 400)
            .unwrap_or_default()
            .contains("ECHO")
    });
    let pane = agent.capture_tail(&peer_session, 400).unwrap_or_default();

    assert!(up, "the peer's stub pane never came up");
    assert_eq!(code, 0, "exit code; said: {said}");
    assert!(said.contains("delivered"), "receipt: {said}");
    assert!(echoed, "nothing was submitted to the peer's pane:\n{pane}");
    assert!(
        pane.contains("ping from sender"),
        "the message text never arrived:\n{pane}"
    );
    // THE ENVELOPE IS WHAT THE RECIPIENT SEES. Without it a message from another agent is
    // indistinguishable from the human's own turn.
    assert!(
        pane.contains("message from session sender"),
        "the envelope did not name the sender:\n{pane}"
    );
    assert!(
        pane.contains("--to sender"),
        "the envelope did not say how to reply:\n{pane}"
    );
    // And the hop ledger recorded it, which is what bounds a reply loop.
    assert!(
        sender_state.join("message-hops/peer.json").exists(),
        "no hop was recorded for the peer"
    );
}

/// ACCEPTANCE, the half that matters: a pane showing a numbered permission dialog is REFUSED.
///
/// `Driver::send_keys` always finishes with a separate `Enter`, so a message delivered here would
/// not be read by an agent — its characters would hit a select widget's accelerators and the
/// `Enter` would CONFIRM whatever option was highlighted. An agent that can send to another agent
/// must not be able to grant that agent's permissions.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored a_message_is_refused`.
#[test]
#[ignore]
fn a_message_is_refused_by_a_pane_showing_a_dialog() {
    if !tmux_available() {
        eprintln!("skipping message dialog test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let sender_state = dir.path().join("sender/.project-state/sessions/sender-1");
    let peer_root = dir.path().join("peer");
    std::fs::create_dir_all(&sender_state).unwrap();
    std::fs::create_dir_all(&peer_root).unwrap();
    let registry = dir.path().join("registry.json");
    write_registry(&registry, &[("peer", &peer_root)]);

    let socket = TmuxSocket::new("pm-msgd");
    let script = write_script(dir.path(), "peer.sh", DIALOG_STUB);
    let peer_session = start_stub_pane(socket.name(), "peer", &peer_root, &script);
    let agent = TmuxDriver::with_socket(socket.name());
    let up = wait_until(Duration::from_secs(10), || {
        agent.is_alive(&peer_session).unwrap_or(false)
    });
    // The dialog has to be on screen before the send, or the refusal proves nothing.
    let drawn = wait_until(Duration::from_secs(10), || {
        agent
            .capture_tail(&peer_session, 400)
            .unwrap_or_default()
            .contains("1.")
    });

    let (code, said) = run_message(
        "sender",
        &sender_state,
        socket.name(),
        &[
            "--to",
            "peer",
            "--message",
            "2",
            "--registry",
            registry.to_str().unwrap(),
        ],
    );
    let pane = agent.capture_tail(&peer_session, 400).unwrap_or_default();

    assert!(
        up && drawn,
        "the dialog stub never drew its dialog:\n{pane}"
    );
    assert_eq!(code, 3, "a dialog is transient, so retry — said: {said}");
    assert!(said.contains("dialog"), "the reason must name it: {said}");
    // The stub prints `LEAKED` for anything it receives. Nothing may have reached it.
    assert!(
        !pane.contains("LEAKED"),
        "a message reached a pane showing a dialog:\n{pane}"
    );
}
