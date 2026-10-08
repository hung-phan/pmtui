use crate::job::{self, AgentLoopState, JobRun, LedgerSituation, WakeState};
use crate::registry::Engine;
use crate::state::{self, Config, ProjectPaths, Tier};

use super::ProjectView;

fn autopilot_config() -> Config {
    Config {
        autonomy: Tier::Autopilot,
        step_timeout_s: 1_800,
        max_failures: 3,
        stuck_threshold: 3,
        coordinator_lease_s: 1_860,
        decider_engine: Engine::Claude,
        decider_model: None,
    }
}

#[test]
fn accepted_monitoring_report_overrides_a_missed_codex_turn_signal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    state::write_json_atomic(&paths.config(), &autopilot_config()).unwrap();

    let mut ledger = AgentLoopState::fresh(Engine::Codex, Some(300), 1_000);
    ledger.run = JobRun::Monitoring { until: 1_300 };
    ledger.turn_count_at_nudge = Some(4);
    ledger.nudged_at_seq = Some(10);
    ledger.last_marker_seq = 11;
    ledger.situation = Some(LedgerSituation {
        state: WakeState::Monitoring,
        status: Some("build is running".into()),
        open_stops: Vec::new(),
        seq: 11,
        at: 1_000,
    });
    job::save(&paths, &ledger).unwrap();
    std::fs::write(paths.turn_signal(), "....").unwrap();

    let waiting = ProjectView::read_agent_loop("bot", &paths, true, 1_000);
    assert_eq!(
        waiting.agent_working,
        Some(false),
        "an accepted monitoring report ends the wake even when Codex misses its notify hook"
    );
    assert_eq!(waiting.next_action, "waiting · check in 00:05:00");
    assert_eq!(
        ProjectView::read_agent_loop("bot", &paths, true, 1_001).next_action,
        "waiting · check in 00:04:59",
        "the restored countdown must advance with wall time"
    );

    ledger.nudged_at_seq = Some(11);
    job::save(&paths, &ledger).unwrap();
    let nudged = ProjectView::read_agent_loop("bot", &paths, true, 1_000);
    assert_eq!(
        nudged.agent_working,
        Some(true),
        "a newer outstanding nudge restores the turn-signal working gate"
    );
    assert_eq!(nudged.next_action, "working");
}
