use std::process::Command;
use std::time::Duration;

use agent_manager::registry::{Engine, Registry};
use agent_manager::tmux::{Driver, TmuxDriver};

use crate::keystrokes::{send_key, send_literal, send_mouse_click};
use crate::probe::{
    TmuxSocket, kill_server_and_socket, tmux_available, wait_for_pane_text,
    wait_for_pane_text_within, wait_until,
};

#[test]
#[ignore]
fn initial_message_and_preview_click_use_one_interactive_terminal() {
    if !tmux_available() {
        eprintln!("skipping direct-dispatch test: tmux not available");
        return;
    }
    for engine in [Engine::Claude, Engine::Codex] {
        run_engine_case(engine);
    }
}

fn run_engine_case(engine: Engine) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("scratch directory");
    let root = std::fs::canonicalize({
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).expect("project root");
        root
    })
    .expect("canonical root");
    let registry = dir.path().join("registry.json");
    // ONE ARGUMENT PER LINE. The echo used to join every arg on a single line, which the pane then
    // wrapped mid-token — so a `contains` for the initial message failed the moment the injected
    // `-c notify=…` config grew long enough to push it across a line boundary. The product was
    // fine; the probe was measuring its own formatting.
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("stub bin");
    for name in ["claude", "codex"] {
        let stub = bin.join(name);
        std::fs::write(
            &stub,
            r#"#!/bin/sh
if [ "$1" = "debug" ] && [ "$2" = "models" ]; then exit 0; fi
printf 'DIRECT_READY\n'
for arg in "$@"; do printf '<%s>\n' "$arg"; done
case "$(basename "$0")" in
  claude) prompt='❯  ' ;;
  *) prompt='› Ready' ;;
esac
printf '%s\n' "$prompt"
while IFS= read -r line; do
  printf 'RECEIVED<%s>\n' "$line"
  printf '%s\n' "$prompt"
done
"#,
        )
        .expect("agent stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("agent stub executable");
    }

    let socket = TmuxSocket::new(match engine {
        Engine::Claude => "am-direct-claude",
        Engine::Codex => "am-direct-codex",
    });
    let host_socket = TmuxSocket::new(match engine {
        Engine::Claude => "am-direct-host-claude",
        Engine::Codex => "am-direct-host-codex",
    });
    let host = format!("direct-host-{}-{}", engine.label(), std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let host_driver = TmuxDriver::with_socket(host_socket.name());
    let command = format!(
        "PATH='{}':\"$PATH\" exec '{}' --socket '{}' --registry '{}'",
        bin.display(),
        env!("CARGO_BIN_EXE_pmtui"),
        socket.name(),
        registry.display(),
    );
    let launched = Command::new("tmux")
        .args([
            "-L",
            host_socket.name(),
            "new-session",
            "-d",
            "-s",
            &host,
            "-c",
            &root.display().to_string(),
            "-x",
            "160",
            "-y",
            "45",
            &command,
        ])
        .status()
        .is_ok_and(|status| status.success());
    let up = launched && wait_for_pane_text(&host_driver, &host, "Quit");
    let form = up
        && send_literal(host_socket.name(), &host, "n")
        && wait_for_pane_text(&host_driver, &host, "Message");
    let selected = form
        && match engine {
            Engine::Claude => true,
            Engine::Codex => {
                send_key(host_socket.name(), &host, "Tab")
                    && send_key(host_socket.name(), &host, "Space")
                    && wait_for_pane_text(&host_driver, &host, "< codex >")
                    && send_key(host_socket.name(), &host, "BTab")
            }
        };
    let message = format!("DIRECT_{}_OK", engine.label().to_ascii_uppercase());
    let submitted = selected
        && send_literal(host_socket.name(), &host, &message)
        && send_key(host_socket.name(), &host, "Enter");
    let entry = wait_until(Duration::from_secs(10), || {
        Registry::load(&registry).is_ok_and(|registry| !registry.projects.is_empty())
    })
    .then(|| {
        Registry::load(&registry)
            .expect("registry")
            .projects
            .into_iter()
            .next()
            .expect("session")
    });
    let preview = submitted
        && wait_for_pane_text(&host_driver, &host, "· open")
        && wait_for_pane_text(&host_driver, &host, &message);
    let follow_up = format!("FOLLOWUP_{}_OK", engine.label().to_ascii_uppercase());
    let continued = preview
        && send_key(host_socket.name(), &host, "s")
        && wait_for_pane_text(&host_driver, &host, "Message")
        && send_literal(host_socket.name(), &host, &follow_up)
        && send_key(host_socket.name(), &host, "Enter")
        && wait_for_pane_text(&host_driver, &host, &format!("RECEIVED<{follow_up}>"));
    // At 160 columns the preview starts right of the 50-column sessions pane; row 1 is its title.
    let clicked = continued && send_mouse_click(host_socket.name(), &host, 60, 1);
    let project_session = entry
        .as_ref()
        .map(|entry| agent_manager::tmux::session_name(&entry.id, &entry.root));
    let attached = clicked
        && project_session.as_ref().is_some_and(|session| {
            wait_until(Duration::from_secs(5), || {
                driver.has_clients(session).unwrap_or(false)
            })
        });
    let attached_message = format!("ATTACHED_{}_OK", engine.label().to_ascii_uppercase());
    let typed_while_attached = attached
        && send_literal(host_socket.name(), &host, &attached_message)
        && send_key(host_socket.name(), &host, "Enter")
        && project_session.as_ref().is_some_and(|session| {
            wait_for_pane_text(&driver, session, &format!("RECEIVED<{attached_message}>"))
        });
    let detached = typed_while_attached
        && send_key(host_socket.name(), &host, "C-q")
        && wait_for_pane_text_within(
            &host_driver,
            &host,
            "detached from",
            Duration::from_secs(10),
        );
    let sessions = driver.list_sessions();
    let pane = host_driver.capture_tail(&host, 160).unwrap_or_default();

    let _ = send_literal(host_socket.name(), &host, "q");
    wait_until(Duration::from_secs(5), || {
        !host_driver.is_alive(&host).unwrap_or(false)
    });
    kill_server_and_socket(socket.name());
    kill_server_and_socket(host_socket.name());

    assert!(
        launched && up && form,
        "pmtui/form failed for {engine:?}:\n{pane}"
    );
    assert!(
        selected && submitted && preview,
        "dispatch failed for {engine:?}:\n{pane}"
    );
    let entry = entry.expect("session entry checked above");
    assert_eq!(entry.engine, Some(engine));
    assert_eq!(entry.initial_prompt.as_deref(), Some(message.as_str()));
    assert!(
        detached && continued,
        "preview click, attached typing, or inline follow-up failed for {engine:?} (continued={continued}, clicked={clicked}, attached={attached}, typed={typed_while_attached}, detached={detached}):\n{pane}"
    );
    assert_eq!(
        sessions
            .iter()
            .filter(|name| name.starts_with("pm-"))
            .count(),
        1,
        "direct dispatch must own one project terminal: {sessions:?}"
    );
}
