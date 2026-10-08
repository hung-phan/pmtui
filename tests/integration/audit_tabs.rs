use std::process::{Command, Stdio};
use std::time::Duration;

use agent_manager::job::{self, TurnDisposition, TurnTrigger, WakeReport, WakeState};
use agent_manager::registry::Engine;
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, TmuxDriver};

use crate::probe::{TmuxSocket, tmux_available, wait_for_pane_text_within};
use crate::seed::seed_standard_loop_session;

fn send_key(socket: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["-L", socket, "send-keys", "-t", "ui", key])
        .status()
        .expect("send scratch key");
    assert!(status.success());
}

#[test]
#[ignore = "acceptance: real pmtui audit tabs over tmux"]
fn audit_tabs_show_decisions_then_correlated_turns() {
    if !tmux_available() {
        eprintln!("skipping audit-tabs acceptance: tmux not available");
        return;
    }

    let host = TmuxSocket::new("am-audit-tabs");
    let inner = TmuxSocket::new("am-audit-tabs-inner");
    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, dir.path(), "alpha");
    let paths = ProjectPaths::for_session(dir.path(), "alpha");

    let mut ledger = job::load(&paths).unwrap().expect("seeded ledger");
    ledger.start_turn(
        100,
        TurnTrigger::Heartbeat {
            pending_context: false,
            marker_recovery: false,
        },
    );
    ledger.report_generation = 1;
    ledger.finish_turn_from_report(
        142,
        &WakeReport {
            state: WakeState::Monitoring,
            seq: 1,
            stops: Vec::new(),
            next_check_s: Some(300),
            cadence_s: None,
            status: Some(
                "monitoring the detached verification across every package until the final artifact is ready STATUS_TAIL_VISIBLE"
                    .into(),
            ),
            next_step: Some(
                "inspect the verification output and reconcile every remaining warning NEXT_TAIL_VISIBLE"
                    .into(),
            ),
            conversation_id: None,
        },
        TurnDisposition::Monitoring,
    );
    job::save(&paths, &ledger).unwrap();

    let command = format!(
        "'{}' --registry '{}' --socket '{}'",
        env!("CARGO_BIN_EXE_pmtui"),
        registry.display(),
        inner.name()
    );
    let status = Command::new("tmux")
        .args([
            "-L",
            host.name(),
            "new-session",
            "-d",
            "-s",
            "ui",
            "-x",
            "100",
            "-y",
            "24",
            &command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("launch scratch pmtui");
    assert!(status.success());
    let driver = TmuxDriver::with_socket(host.name());
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "alpha",
        Duration::from_secs(5)
    ));

    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    // No terminal runs for this seeded row, so it sits under PAUSED / OFFLINE whatever its
    // mode; its own `[A]` tag is what shows the dashboard re-read the tier.
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "[A]",
        Duration::from_secs(5)
    ));

    send_key(host.name(), "v");
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "[Decisions]",
        Duration::from_secs(5)
    ));
    send_key(host.name(), "Tab");
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "turn #1",
        Duration::from_secs(5)
    ));
    let pane = driver.capture_tail("ui", 80).expect("capture audit");
    assert!(pane.contains("[Turns]"), "{pane}");
    assert!(pane.contains("report #1"), "{pane}");
    assert!(pane.contains("STATUS_TAIL_VISIBLE"), "{pane}");
    assert!(pane.contains("NEXT_TAIL_VISIBLE"), "{pane}");

    send_key(host.name(), "q");
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "SESSIONS",
        Duration::from_secs(5)
    ));
}
