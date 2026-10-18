use tempfile::tempdir;

use super::super::*;

#[test]
fn config_round_trips_and_applies_defaults() {
    let json = r#"{ "autonomy": "autopilot" }"#;
    let config: Config = serde_json::from_str(json).unwrap();
    assert_eq!(config.autonomy, Tier::Autopilot);
    assert_eq!(config.step_timeout_s, 1800);
    assert_eq!(config.max_failures, 3);
}

#[test]
fn control_defaults_and_round_trips_human_requests() {
    let empty: Control = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.human_cadence_s, None);
    assert_eq!(empty.wake_generation, 0);

    let control = Control {
        human_cadence_s: Some(60),
        wake_generation: 7,
    };
    let round_trip: Control =
        serde_json::from_str(&serde_json::to_string(&control).unwrap()).unwrap();
    assert_eq!(round_trip, control);
}

#[test]
fn legacy_guardian_config_deserializes_to_standard() {
    let config: Config = serde_json::from_str(r#"{ "autonomy": "guardian" }"#).unwrap();
    assert_eq!(config.autonomy, Tier::Standard);
    assert_eq!(
        serde_json::from_str::<Tier>("\"standard\"").unwrap(),
        Tier::Standard
    );
    assert_eq!(
        serde_json::from_str::<Tier>("\"autopilot\"").unwrap(),
        Tier::Autopilot
    );
    assert_eq!(
        serde_json::to_string(&Tier::Standard).unwrap(),
        "\"standard\""
    );
}

#[test]
fn driver_round_trips() {
    let driver = DriverState {
        step_id: 42,
        pane: "pmd-proj-42".into(),
        spawned_at: 1000,
        deadline: 1300,
        ended_at: None,
        exit_code: None,
        exit_reason: ExitReason::Running,
        consecutive_failures: 0,
        observed_at: 1000,
    };

    let json = serde_json::to_string(&driver).unwrap();
    assert!(json.contains("\"running\""), "{json}");
    assert_eq!(serde_json::from_str::<DriverState>(&json).unwrap(), driver);
}

#[test]
fn atomic_write_then_read_round_trips_no_temp_leftover() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    let driver = DriverState {
        step_id: 1,
        pane: "pmd-proj-1".into(),
        spawned_at: 1,
        deadline: 10,
        ended_at: None,
        exit_code: None,
        exit_reason: ExitReason::Running,
        consecutive_failures: 0,
        observed_at: 1,
    };

    write_driver(&paths, &driver).unwrap();

    let round_trip: DriverState = read_json(&paths.driver()).unwrap();
    assert_eq!(round_trip, driver);
    let leftovers: Vec<_> = std::fs::read_dir(paths.state_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

#[test]
fn missing_arrays_and_driver_default() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    let stops: Vec<Stop> = read_json_or(&paths.stops(), Vec::new()).unwrap();
    assert!(stops.is_empty());
    let driver: Option<DriverState> = read_json_opt(&paths.driver()).unwrap();
    assert!(driver.is_none());
}

#[test]
fn legacy_stop_and_answer_records_apply_defaults() {
    let stop: Stop = serde_json::from_str(
        r#"{"id":"s","kind":"publish","risk_class":"low","question":"ship?"}"#,
    )
    .unwrap();
    assert_eq!(stop.status, "awaiting_reply");

    let answer: Answer =
        serde_json::from_str(r#"{"stop_id":"s","answer":"yes","answered_at":42}"#).unwrap();
    assert_eq!(answer.answered_by, "user");
}

#[test]
fn append_answer_accumulates() {
    let dir = tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    for index in 0..3 {
        append_answer(
            &paths,
            &Answer {
                stop_id: format!("stop-{index}"),
                answer: "B".into(),
                note: None,
                answered_by: "user".into(),
                answered_at: 100 + index,
            },
        )
        .unwrap();
    }

    let answers: Vec<Answer> = read_json(&paths.answers()).unwrap();
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[2].stop_id, "stop-2");
}

#[test]
fn config_defaults_new_harness_fields() {
    let config: Config = serde_json::from_str(r#"{ "autonomy": "standard" }"#).unwrap();
    assert_eq!(config.stuck_threshold, 3);
    assert_eq!(config.coordinator_lease_s, 1860);
    assert!(config.validate().is_ok());
}

#[test]
fn config_validate_requires_lease_over_timeout() {
    let config = Config {
        autonomy: Tier::Standard,
        step_timeout_s: 1800,
        max_failures: 3,
        stuck_threshold: 3,
        coordinator_lease_s: 1800,
        decider_engine: crate::registry::Engine::Claude,
        decider_model: None,
    };

    assert!(config.validate().is_err(), "lease must be >= timeout + 60");
}

#[test]
fn config_validate_rejects_timeout_margin_overflow() {
    let config = Config {
        autonomy: Tier::Standard,
        step_timeout_s: u64::MAX,
        max_failures: 3,
        stuck_threshold: 3,
        coordinator_lease_s: u64::MAX,
        decider_engine: crate::registry::Engine::Claude,
        decider_model: None,
    };

    assert_eq!(
        config.validate(),
        Err("step_timeout_s is too large to add the 60s lease margin".to_string())
    );
}

use super::super::records::{ANSWERS_RETENTION_S, prune_answers};
use crate::clock::Epoch;

// Moved out of `state/records.rs`: a production file does not carry its own test module when
// the module already has a mirrored test file here. `use super::super::*` is what this file
// already uses for `crate::state::*`, which is what `use super::*` meant inside `records.rs`.

fn ans(stop: &str, at: Epoch) -> Answer {
    Answer {
        stop_id: stop.to_string(),
        answer: "ok".to_string(),
        note: None,
        answered_by: "user".to_string(),
        answered_at: at,
    }
}

#[test]
fn prune_drops_only_answers_older_than_the_retention_window() {
    let now: Epoch = 1_700_000_000;
    // Appended in time order (oldest first), like the real file.
    let mut v = vec![
        ans("old", now - ANSWERS_RETENTION_S - 1), // just past the window → dropped
        ans("edge", now - ANSWERS_RETENTION_S),    // exactly at the window → kept
        ans("recent", now - 10),                   // kept
        ans("newest", now),                        // the reference → kept
    ];
    prune_answers(&mut v);
    let ids: Vec<&str> = v.iter().map(|a| a.stop_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["edge", "recent", "newest"],
        "only the answer older than the retention window is pruned"
    );
}

#[test]
fn prune_caps_the_count_dropping_the_oldest() {
    let now: Epoch = 1_700_000_000;
    // ANSWERS_MAX + 50 answers, all within the window (1s apart), oldest first.
    let n = ANSWERS_MAX + 50;
    let mut v: Vec<Answer> = (0..n)
        .map(|i| ans(&format!("s{i}"), now - (n as Epoch) + i as Epoch))
        .collect();
    prune_answers(&mut v);
    assert_eq!(v.len(), ANSWERS_MAX, "count is capped to ANSWERS_MAX");
    assert_eq!(
        v.last().unwrap().stop_id,
        format!("s{}", n - 1),
        "the newest answer is retained"
    );
    assert_eq!(
        v.first().unwrap().stop_id,
        format!("s{}", n - ANSWERS_MAX),
        "the oldest beyond the cap were dropped"
    );
}

#[test]
fn prune_is_a_noop_below_both_bounds() {
    let now: Epoch = 1_700_000_000;
    let mut v = vec![ans("a", now - 100), ans("b", now)];
    let before = v.clone();
    prune_answers(&mut v);
    assert_eq!(v, before, "a small, recent set is left untouched");
}

use crate::registry::Engine;

#[test]
fn a_config_without_decider_engine_defaults_to_claude() {
    // A config written before this field existed (or by another tool) must still load,
    // and must read as Claude — the safe/validated default. This is a safety property:
    // the decider auto-approves, so an unknown/missing engine must NOT silently become codex.
    let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860}"#;
    let cfg: Config = serde_json::from_str(json).expect("legacy config must still parse");
    assert_eq!(cfg.decider_engine, Engine::Claude);
}

#[test]
fn decider_engine_round_trips_codex() {
    let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"codex"}"#;
    let cfg: Config = serde_json::from_str(json).expect("config with decider_engine must parse");
    assert_eq!(cfg.decider_engine, Engine::Codex);
    let back = serde_json::to_string(&cfg).unwrap();
    assert!(back.contains("\"decider_engine\":\"codex\""), "got {back}");
}

#[test]
fn config_without_decider_model_loads_as_none() {
    let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"codex"}"#;
    let cfg: Config = serde_json::from_str(json).expect("legacy config must parse");
    assert_eq!(cfg.decider_model, None);
}

#[test]
fn decider_model_round_trips() {
    let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"claude","decider_model":"global.anthropic.claude-opus-5"}"#;
    let cfg: Config = serde_json::from_str(json).expect("must parse");
    assert_eq!(
        cfg.decider_model.as_deref(),
        Some("global.anthropic.claude-opus-5")
    );
    assert!(
        serde_json::to_string(&cfg)
            .unwrap()
            .contains("\"decider_model\":\"global.anthropic.claude-opus-5\"")
    );
}
