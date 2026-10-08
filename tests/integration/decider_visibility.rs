use std::process::{Command, Stdio};
use std::time::Duration;

use agent_manager::clock::{Clock, SystemClock};
use agent_manager::job::{
    self, DeciderOutcome, DeciderPolicy, DeciderRun, DeciderTarget, ParkedAdvice, QueuedAdvice,
};
use agent_manager::pmstate::StopKind;
use agent_manager::registry::Engine;
use agent_manager::state::{self, Config, ProjectPaths, RiskClass, Tier};
use agent_manager::tmux::{Driver, supervisor_session_name};
use agent_manager::worker::{StopDraft, StopEffect};

use crate::pmtui_fixture::{PmdSibling, enter_fixture};
use crate::probe::{tmux_available, wait_for_pane_text_within};
use crate::seed::seed_standard_loop_session;

fn draft(id: &str) -> QueuedAdvice {
    QueuedAdvice {
        stop_id: id.into(),
        report_seq: 4,
        draft: StopDraft {
            kind: StopKind::Ambiguity,
            risk_class: RiskClass::Low,
            question: format!("resolve {id}?"),
            options: vec!["Use the simple path".into(), "Keep the current path".into()],
            context_ref: None,
            effect: StopEffect::default(),
        },
    }
}

#[test]
#[ignore = "acceptance: real pmtui primary decider activity over tmux"]
fn primary_view_distinguishes_pending_debt_from_a_live_decider() {
    if !tmux_available() {
        eprintln!("skipping decider-visibility acceptance: tmux not available");
        return;
    }

    let fixture = enter_fixture("decvis", PmdSibling::Missing);
    if !fixture.up {
        eprintln!("skipping decider-visibility acceptance: dashboard did not start");
        return;
    }
    seed_standard_loop_session(&fixture.reg_path, &fixture.proj, "alpha");
    let paths = ProjectPaths::for_session(&fixture.proj, "alpha");
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
    let now = SystemClock.now();
    let mut ledger = job::load(&paths).unwrap().expect("seeded ledger");
    ledger.advice_inflight = Some(ParkedAdvice {
        seq: 9,
        stop_ids: vec!["decision-1".into()],
        pane_dialog: false,
    });
    ledger.advice_queue = vec![draft("decision-1"), draft("decision-2")];
    ledger.decider_runs.push(DeciderRun {
        seq: 9,
        started_at: now - 65,
        finished_at: None,
        engine: Engine::Claude,
        model: None,
        target: DeciderTarget::Marker,
        question: "Which parser should the worker use?".into(),
        options: vec!["Use serde".into(), "Keep the custom parser".into()],
        reported_kind: Some(StopKind::Ambiguity),
        effect: None,
        policy: DeciderPolicy {
            kind: StopKind::Ambiguity,
            labelled_risk: RiskClass::Low,
            effective_risk: RiskClass::Low,
        },
        outcome: DeciderOutcome::Consulting,
    });
    job::save(&paths, &ledger).unwrap();

    assert!(wait_for_pane_text_within(
        &fixture.host,
        &fixture.host_session,
        "review pending",
        Duration::from_secs(5)
    ));
    let supervisor = supervisor_session_name("alpha", &fixture.proj, 9);
    let status = Command::new("tmux")
        .args([
            "-L",
            &fixture.agent_socket,
            "new-session",
            "-d",
            "-s",
            &supervisor,
            "exec sleep 120",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("start scratch decider terminal");
    assert!(status.success());
    assert!(wait_for_pane_text_within(
        &fixture.host,
        &fixture.host_session,
        "reviewing",
        Duration::from_secs(5)
    ));
    let pane = fixture
        .host
        .capture_tail(&fixture.host_session, 80)
        .expect("capture primary view");
    assert!(pane.contains("decision #9"), "{pane}");
    assert!(pane.contains("1 queued"), "{pane}");
    assert!(
        !pane.contains("[Decisions]"),
        "audit should still be closed: {pane}"
    );
}
