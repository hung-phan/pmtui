use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::session_name;

use crate::probe::{TmuxSocket, tmux_available, wait_for_pane_text_within, wait_until};
use agent_manager::tmux::TmuxDriver;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
fn write_alternate_screen_agent(path: &Path) {
    fs::write(
        path,
        "#!/bin/sh\n\
         printf '\\033[?1049h'\n\
         while :; do\n\
           rows=$(stty size); rows=${rows%% *}\n\
           printf '\\033[H\\033[2J'\n\
           i=1\n\
           while [ \"$i\" -le \"$rows\" ]; do\n\
             printf 'ALT ROW %04d\\r\\n' \"$i\"\n\
             i=$((i + 1))\n\
           done\n\
           sleep 0.2\n\
         done\n",
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
#[ignore = "requires real tmux"]
fn autopilot_preview_scrolls_an_alternate_screen_pane() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not installed");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let host = TmuxSocket::new("pmtui-alt-preview-host");
    let inner = TmuxSocket::new("pmtui-alt-preview-inner");
    let id = "alt-preview";
    let root = dir.path();
    let session = session_name(id, root);
    let registry_path = root.join("registry.json");
    Registry {
        projects: vec![ProjectEntry {
            id: id.into(),
            display_name: None,
            root: root.into(),
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
    .save(&registry_path)
    .unwrap();
    let paths = ProjectPaths::for_session(root, id);
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1_800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1_860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();

    let agent = root.join("alternate-agent.sh");
    write_alternate_screen_agent(&agent);
    let started = tmux(
        inner.name(),
        &[
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "80",
            "-y",
            "50",
            agent.to_str().unwrap(),
        ],
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );

    let command = format!(
        "'{}' --registry '{}' --socket '{}'",
        env!("CARGO_BIN_EXE_pmtui"),
        registry_path.display(),
        inner.name()
    );
    let launched = Command::new("tmux")
        .args([
            "-L",
            host.name(),
            "new-session",
            "-d",
            "-s",
            "ui",
            "-x",
            "140",
            "-y",
            "79",
            &command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(launched.success());
    let driver = TmuxDriver::with_socket(host.name());
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        id,
        Duration::from_secs(5)
    ));

    assert!(wait_until(Duration::from_secs(5), || {
        let output = tmux(
            inner.name(),
            &[
                "display-message",
                "-p",
                "-t",
                &format!("={session}:"),
                "#{alternate_on} #{pane_height}",
            ],
        );
        let observed = String::from_utf8_lossy(&output.stdout);
        let mut fields = observed.split_whitespace();
        fields.next() == Some("1")
            && fields
                .next()
                .and_then(|height| height.parse::<u16>().ok())
                .is_some_and(|height| height > 79)
    }));

    assert!(
        wait_until(Duration::from_secs(5), || {
            let capture = tmux(
                inner.name(),
                &[
                    "capture-pane",
                    "-p",
                    "-t",
                    &format!("={session}:"),
                    "-S",
                    "-2000",
                ],
            );
            capture.status.success()
                && String::from_utf8_lossy(&capture.stdout)
                    .lines()
                    .filter(|line| line.contains("ALT ROW"))
                    .count()
                    > 79
        }),
        "the alternate screen must expose more rows than the dashboard viewport"
    );
}
