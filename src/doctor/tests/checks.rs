use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::super::{Check, CheckStatus, DoctorOptions, OverallStatus, RuntimeProbe, inspect_with};
use crate::job::{
    AgentLoopState, DeciderOutcome, DeciderPolicy, DeciderRun, DeciderTarget, ParkedAdvice,
};
use crate::registry::Engine;
use crate::state::{ProjectPaths, RiskClass};
use crate::tmux::{session_name, supervisor_session_name};

#[derive(Debug, Clone)]
struct FakeProbe {
    binaries: BTreeSet<String>,
    tmux_version: Result<String, String>,
    tmux_sessions: Result<Vec<String>, String>,
    dead_sessions: BTreeSet<String>,
    pane_error: Option<String>,
}

impl FakeProbe {
    fn healthy(sessions: Vec<String>) -> Self {
        Self {
            binaries: ["claude", "codex", "notify-send"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            tmux_version: Ok("tmux 3.5".into()),
            tmux_sessions: Ok(sessions),
            dead_sessions: BTreeSet::new(),
            pane_error: None,
        }
    }
}

impl RuntimeProbe for FakeProbe {
    fn binary_on_path(&self, binary: &str) -> bool {
        self.binaries.contains(binary)
    }

    fn tmux_version(&self) -> Result<String, String> {
        self.tmux_version.clone()
    }

    fn tmux_sessions(&self, _socket: &str) -> Result<Vec<String>, String> {
        self.tmux_sessions.clone()
    }

    fn tmux_pane_dead(&self, _socket: &str, session: &str) -> Result<bool, String> {
        if let Some(error) = &self.pane_error {
            Err(error.clone())
        } else {
            Ok(self.dead_sessions.contains(session))
        }
    }
}

fn options(dir: &TempDir) -> DoctorOptions {
    DoctorOptions {
        registry: dir.path().join("registry.json"),
        socket: "doctor-test".into(),
    }
}

fn project(id: &str, root: &Path, enabled: bool, engine: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "root": root,
        "enabled": enabled,
        "engine": engine
    })
}

fn write_registry(path: &Path, projects: Vec<serde_json::Value>) {
    fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({ "projects": projects })).unwrap(),
    )
    .unwrap();
}

fn seed_project(root: &Path, id: &str, engine: Engine) {
    let paths = ProjectPaths::for_session(root, id);
    fs::create_dir_all(paths.state_dir()).unwrap();
    fs::write(paths.config(), b"{}").unwrap();
    fs::write(paths.control(), b"{}").unwrap();
    fs::write(
        paths.pmstate(),
        serde_json::to_vec_pretty(&AgentLoopState::fresh(engine, None, 10)).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(
        paths
            .canonical_worker_skill_file()
            .parent()
            .expect("skill has a parent"),
    )
    .unwrap();
    fs::write(
        paths.canonical_worker_skill_file(),
        crate::skills::WORKER_SKILL_MD,
    )
    .unwrap();
    crate::skills::install_spawn_skill(&paths, engine).unwrap();
}

fn check<'a>(checks: &'a [Check], id: &str) -> &'a Check {
    checks
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("missing check {id}: {checks:#?}"))
}

#[test]
fn a_healthy_project_passes_every_check() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );
    let expected = session_name("alpha", &root);

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(vec![expected]));

    assert_eq!(report.status, OverallStatus::Pass, "{report:#?}");
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.status == CheckStatus::Pass)
    );
    assert_eq!(
        check(&report.checks, "project.0.state").summary,
        "alpha ledger is valid"
    );
    assert_eq!(
        check(&report.checks, "project.0.driver").summary,
        "alpha has no driver record yet"
    );
}

#[test]
fn missing_and_malformed_registries_are_distinct() {
    let dir = TempDir::new().unwrap();
    let probe = FakeProbe::healthy(Vec::new());

    let missing = inspect_with(&options(&dir), &probe);
    assert_eq!(missing.status, OverallStatus::Warn);
    assert_eq!(check(&missing.checks, "registry").status, CheckStatus::Warn);

    fs::write(&options(&dir).registry, b"{not-json").unwrap();
    let malformed = inspect_with(&options(&dir), &probe);
    assert_eq!(malformed.status, OverallStatus::Fail);
    assert_eq!(
        check(&malformed.checks, "registry").status,
        CheckStatus::Fail
    );
}

#[test]
fn unreadable_registry_is_a_failure() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(&options(&dir).registry).unwrap();

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(check(&report.checks, "registry").status, CheckStatus::Fail);
    assert!(
        check(&report.checks, "registry")
            .summary
            .contains("could not be read")
    );
}

#[test]
fn registry_entries_reject_blank_duplicate_and_relative_identity() {
    let dir = TempDir::new().unwrap();
    write_registry(
        &options(&dir).registry,
        vec![
            project("", Path::new("relative"), true, "claude"),
            project("same", Path::new("relative-a"), true, "claude"),
            project("same", Path::new("relative-b"), true, "codex"),
        ],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    for index in 0..3 {
        assert_eq!(
            check(&report.checks, &format!("registry.entry.{index}")).status,
            CheckStatus::Fail
        );
    }
}

#[test]
fn missing_roots_fail_only_for_enabled_projects() {
    let dir = TempDir::new().unwrap();
    let enabled = dir.path().join("enabled-missing");
    let disabled = dir.path().join("disabled-missing");
    write_registry(
        &options(&dir).registry,
        vec![
            project("enabled", &enabled, true, "claude"),
            project("disabled", &disabled, false, "codex"),
        ],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(
        check(&report.checks, "project.0.root").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.1.root").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "project.0.state").status,
        CheckStatus::Skip
    );
}

#[test]
fn disabled_projects_do_not_require_worker_or_decider_executables() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("disabled");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "disabled", Engine::Claude);
    write_registry(
        &options(&dir).registry,
        vec![project("disabled", &root, false, "claude")],
    );
    let probe = FakeProbe {
        binaries: ["notify-send"].into_iter().map(str::to_owned).collect(),
        tmux_version: Ok("tmux 3.5".into()),
        tmux_sessions: Ok(Vec::new()),
        dead_sessions: BTreeSet::new(),
        pane_error: None,
    };

    let report = inspect_with(&options(&dir), &probe);

    assert!(
        report
            .checks
            .iter()
            .all(|check| !check.id.starts_with("worker.") && !check.id.starts_with("decider."))
    );
}

#[test]
fn corrupt_state_stale_skill_and_engine_mismatch_are_reported() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Codex);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(
        paths.config(),
        br#"{"step_timeout_s":100,"coordinator_lease_s":20}"#,
    )
    .unwrap();
    fs::write(paths.control(), b"{broken").unwrap();
    fs::write(paths.driver(), b"{broken").unwrap();
    fs::write(paths.canonical_worker_skill_file(), b"old skill").unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(
        check(&report.checks, "project.0.config").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.0.control").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.0.state").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.0.driver").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.0.skill").status,
        CheckStatus::Warn
    );
}

#[test]
fn malformed_state_and_missing_or_unreadable_skills_are_reported() {
    let dir = TempDir::new().unwrap();
    let missing_root = dir.path().join("missing-skill");
    let unreadable_root = dir.path().join("unreadable-skill");
    for (root, id) in [
        (&missing_root, "missing-skill"),
        (&unreadable_root, "unreadable-skill"),
    ] {
        fs::create_dir(root).unwrap();
        seed_project(root, id, Engine::Claude);
    }
    let missing_paths = ProjectPaths::for_session(&missing_root, "missing-skill");
    fs::write(missing_paths.config(), b"{broken").unwrap();
    fs::write(missing_paths.pmstate(), b"{broken").unwrap();
    fs::remove_file(missing_paths.canonical_worker_skill_file()).unwrap();

    let unreadable_paths = ProjectPaths::for_session(&unreadable_root, "unreadable-skill");
    fs::remove_file(unreadable_paths.canonical_worker_skill_file()).unwrap();
    fs::create_dir(unreadable_paths.canonical_worker_skill_file()).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![
            project("missing-skill", &missing_root, true, "claude"),
            project("unreadable-skill", &unreadable_root, true, "claude"),
        ],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(
        check(&report.checks, "project.0.config").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.0.state").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "project.0.skill").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "project.1.skill").status,
        CheckStatus::Fail
    );
}

#[test]
fn doctor_reports_a_stale_spawn_skill() {
    let dir = TempDir::new().unwrap();
    let ids = ["current", "stale", "missing", "unreadable"];
    let mut projects = Vec::new();
    for id in ids {
        let root = dir.path().join(id);
        fs::create_dir(&root).unwrap();
        seed_project(&root, id, Engine::Claude);
        projects.push(project(id, &root, true, "claude"));
    }
    let spawn_skill =
        |id: &str| ProjectPaths::for_session(dir.path().join(id), id).canonical_spawn_skill_file();
    fs::write(spawn_skill("stale"), b"an older spawn skill").unwrap();
    fs::remove_file(spawn_skill("missing")).unwrap();
    fs::remove_file(spawn_skill("unreadable")).unwrap();
    fs::create_dir(spawn_skill("unreadable")).unwrap();
    projects.push(project("gone", &dir.path().join("gone"), true, "claude"));
    write_registry(&options(&dir).registry, projects);

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    let spawn = |index: usize| check(&report.checks, &format!("project.{index}.spawn_skill"));
    assert_eq!(spawn(0).status, CheckStatus::Pass);
    assert_eq!(spawn(0).summary, "current spawn skill is current");
    assert_eq!(spawn(1).status, CheckStatus::Warn);
    assert_eq!(spawn(1).summary, "stale spawn skill is stale");
    assert_eq!(spawn(2).status, CheckStatus::Warn);
    assert_eq!(spawn(2).summary, "missing spawn skill is missing");
    assert_eq!(spawn(3).status, CheckStatus::Fail);
    assert_eq!(spawn(3).summary, "unreadable spawn skill could not be read");
    assert_eq!(
        spawn(4).status,
        CheckStatus::Skip,
        "a missing root skips it"
    );
    for index in 1..3 {
        assert!(
            spawn(index)
                .remediation
                .as_deref()
                .is_some_and(|hint| hint.contains("pmtui")),
            "a stale or missing spawn skill names the relaunch that ships it: {:?}",
            spawn(index)
        );
    }
    for index in 0..4 {
        assert_eq!(
            check(&report.checks, &format!("project.{index}.skill")).status,
            CheckStatus::Pass,
            "the worker skill check is independent of the spawn skill"
        );
    }
}

#[test]
fn missing_worker_fails_but_optional_capabilities_warn() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );
    let probe = FakeProbe {
        binaries: BTreeSet::new(),
        tmux_version: Err("tmux not found".into()),
        tmux_sessions: Err("must not be called".into()),
        dead_sessions: BTreeSet::new(),
        pane_error: None,
    };

    let report = inspect_with(&options(&dir), &probe);

    assert_eq!(
        check(&report.checks, "worker.claude").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "decider.claude").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "notification.notify-send").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "tmux.version").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "tmux.sessions").status,
        CheckStatus::Skip
    );
}

#[test]
fn tmux_inventory_warns_about_missing_expected_and_leftover_sessions() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );
    let probe = FakeProbe::healthy(vec!["pm-unregistered".into(), "pmsup-stale".into()]);

    let report = inspect_with(&options(&dir), &probe);

    assert_eq!(
        check(&report.checks, "tmux.session.0").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "tmux.unregistered").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "tmux.supervisors.stale").status,
        CheckStatus::Warn
    );
}

#[test]
fn runtime_artifacts_are_checked_without_treating_absence_as_failure() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(paths.needs_you(), b"{broken").unwrap();
    fs::write(paths.checkpoint(), b"{broken").unwrap();
    fs::write(paths.answers(), b"{broken").unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    for id in ["project.0.marker", "project.0.checkpoint"] {
        let check = check(&report.checks, id);
        assert_eq!(check.status, CheckStatus::Warn, "{check:#?}");
        assert!(check.remediation.is_some(), "{check:#?}");
    }
    let answers = check(&report.checks, "project.0.answers");
    assert_eq!(answers.status, CheckStatus::Fail, "{answers:#?}");
    assert!(answers.remediation.is_some(), "{answers:#?}");
}

#[test]
fn autopilot_requires_a_goal_and_control_cadence_must_be_bounded() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(paths.config(), br#"{"autonomy":"autopilot"}"#).unwrap();
    fs::write(paths.control(), br#"{"human_cadence_s":1}"#).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let missing = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));
    assert_eq!(
        check(&missing.checks, "project.0.goal").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&missing.checks, "project.0.control").status,
        CheckStatus::Fail
    );

    fs::write(paths.brief(), "Finish the parser migration.").unwrap();
    fs::write(paths.control(), br#"{"human_cadence_s":300}"#).unwrap();
    let valid = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));
    assert_eq!(
        check(&valid.checks, "project.0.goal").status,
        CheckStatus::Pass
    );
    assert_eq!(
        check(&valid.checks, "project.0.control").status,
        CheckStatus::Pass
    );
}

#[cfg(unix)]
#[test]
fn runtime_artifact_symlinks_are_rejected_without_following_them() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    let target = dir.path().join("outside.json");
    fs::write(&target, br#"{"seq":1,"state":"working"}"#).unwrap();
    symlink(&target, paths.needs_you()).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    let marker = check(&report.checks, "project.0.marker");
    assert_eq!(marker.status, CheckStatus::Warn);
    assert!(
        marker
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("regular file")),
        "{marker:#?}"
    );
}

#[test]
fn runtime_marker_variants_answers_and_nonregular_files_are_classified() {
    let dir = TempDir::new().unwrap();
    let working_root = dir.path().join("working");
    let blocked_root = dir.path().join("blocked");
    for (root, id) in [(&working_root, "working"), (&blocked_root, "blocked")] {
        fs::create_dir(root).unwrap();
        seed_project(root, id, Engine::Claude);
    }
    let working = ProjectPaths::for_session(&working_root, "working");
    fs::write(
        working.needs_you(),
        br#"{"seq":2,"state":"working","status":"running tests"}"#,
    )
    .unwrap();
    fs::write(working.answers(), b"[]").unwrap();
    let blocked = ProjectPaths::for_session(&blocked_root, "blocked");
    fs::write(
        blocked.needs_you(),
        br#"{"seq":3,"state":"blocked","stops":[]}"#,
    )
    .unwrap();
    fs::create_dir(blocked.answers()).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![
            project("working", &working_root, true, "claude"),
            project("blocked", &blocked_root, true, "claude"),
        ],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert!(
        check(&report.checks, "project.0.marker")
            .summary
            .contains("working")
    );
    assert_eq!(
        check(&report.checks, "project.0.answers").status,
        CheckStatus::Pass
    );
    assert!(
        check(&report.checks, "project.1.marker")
            .summary
            .contains("blocked")
    );
    assert!(
        check(&report.checks, "project.1.answers")
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("regular file"))
    );
}

#[test]
fn unreadable_session_inventory_is_a_recoverable_failure() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );
    let sessions = root.join(".project-state/sessions");
    fs::remove_dir_all(&sessions).unwrap();
    fs::write(&sessions, "not a directory").unwrap();

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(
        check(&report.checks, "project-state.orphans.0").status,
        CheckStatus::Fail
    );
}

#[test]
fn pending_lower_sequence_markers_and_orphan_state_are_recovery_warnings() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(paths.config(), br#"{"autonomy":"autopilot"}"#).unwrap();
    fs::write(paths.brief(), "Resolve the parser decision.").unwrap();
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.last_marker_seq = 5;
    fs::write(paths.pmstate(), serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
    fs::write(
        paths.needs_you(),
        br#"{"seq":4,"state":"working","status":"older report"}"#,
    )
    .unwrap();
    fs::create_dir_all(root.join(".project-state/sessions/orphan-session")).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(
        check(&report.checks, "project.0.marker").status,
        CheckStatus::Warn
    );
    assert!(
        check(&report.checks, "project.0.marker")
            .summary
            .contains("pending pmd acceptance")
    );
    let orphan = check(&report.checks, "project-state.orphans.0");
    assert_eq!(orphan.status, CheckStatus::Warn);
    assert!(orphan.summary.contains("orphan"), "{orphan:#?}");
}

#[test]
fn standard_row_decider_debt_is_inconsistent_and_its_terminal_is_stale() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.advice_inflight = Some(ParkedAdvice {
        seq: 5,
        stop_ids: Vec::new(),
        pane_dialog: true,
    });
    fs::write(paths.pmstate(), serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );
    let supervisor = supervisor_session_name("alpha", &root, 5);

    let report = inspect_with(
        &options(&dir),
        &FakeProbe::healthy(vec![session_name("alpha", &root), supervisor.clone()]),
    );

    assert_eq!(
        check(&report.checks, "project.0.decider").status,
        CheckStatus::Warn
    );
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.id != "tmux.supervisor.0")
    );
    assert!(
        check(&report.checks, "tmux.supervisors.stale")
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&supervisor))
    );
}

#[test]
fn active_and_stale_decider_terminals_are_distinguished() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(paths.config(), br#"{"autonomy":"autopilot"}"#).unwrap();
    fs::write(paths.brief(), "Resolve the parser decision.").unwrap();
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.advice_inflight = Some(ParkedAdvice {
        seq: 7,
        stop_ids: vec!["decision-1".into()],
        pane_dialog: false,
    });
    ledger.decider_runs.push(DeciderRun {
        seq: 7,
        started_at: 10,
        finished_at: None,
        engine: Engine::Claude,
        model: None,
        target: DeciderTarget::Marker,
        question: "which parser?".into(),
        options: vec!["serde".into(), "custom".into()],
        reported_kind: None,
        effect: None,
        policy: DeciderPolicy {
            kind: crate::pmstate::StopKind::Ambiguity,
            labelled_risk: RiskClass::Low,
            effective_risk: RiskClass::Low,
        },
        outcome: DeciderOutcome::Consulting,
    });
    fs::write(paths.pmstate(), serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );
    let worker = session_name("alpha", &root);
    let active = supervisor_session_name("alpha", &root, 7);
    let stale = supervisor_session_name("alpha", &root, 6);

    let probe = FakeProbe::healthy(vec![worker, active.clone(), stale.clone()]);
    let report = inspect_with(&options(&dir), &probe);

    assert_eq!(
        check(&report.checks, "tmux.supervisor.0").status,
        CheckStatus::Pass
    );
    let leftovers = check(&report.checks, "tmux.supervisors.stale");
    assert_eq!(leftovers.status, CheckStatus::Warn);
    assert!(
        leftovers
            .detail
            .as_deref()
            .is_some_and(|d| d.contains(&stale))
    );
    assert!(
        leftovers
            .detail
            .as_deref()
            .is_some_and(|d| !d.contains(&active))
    );

    let mut dead = probe.clone();
    dead.dead_sessions.insert(active.clone());
    let dead_report = inspect_with(&options(&dir), &dead);
    assert!(
        check(&dead_report.checks, "tmux.supervisor.0")
            .summary
            .contains("pane has exited")
    );

    let mut unknown = probe;
    unknown.pane_error = Some("tmux probe failed".into());
    let unknown_report = inspect_with(&options(&dir), &unknown);
    assert!(
        check(&unknown_report.checks, "tmux.supervisor.0")
            .summary
            .contains("could not be verified")
    );
}

#[test]
fn valid_optional_runtime_artifacts_and_bounds_are_reported() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(
        paths.needs_you(),
        br#"{"seq":6,"state":"monitoring","next_check_s":300}"#,
    )
    .unwrap();
    fs::write(paths.checkpoint(), br#"{"version":1,"seq":3}"#).unwrap();
    let answers: Vec<_> = (0..=crate::state::ANSWERS_MAX)
        .map(|index| {
            serde_json::json!({
                "stop_id": format!("stop-{index}"),
                "answer": "continue",
                "answered_by": "user",
                "answered_at": index as i64
            })
        })
        .collect();
    fs::write(paths.answers(), serde_json::to_vec(&answers).unwrap()).unwrap();
    fs::write(
        paths.driver(),
        br#"{"step_id":1,"pane":"pm-alpha","spawned_at":1,"deadline":2,"exit_reason":"running"}"#,
    )
    .unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert!(
        check(&report.checks, "project.0.marker")
            .summary
            .contains("monitoring")
    );
    assert_eq!(
        check(&report.checks, "project.0.marker").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "project.0.checkpoint").status,
        CheckStatus::Pass
    );
    assert_eq!(
        check(&report.checks, "project.0.answers").status,
        CheckStatus::Warn
    );
    assert_eq!(
        check(&report.checks, "project.0.driver").status,
        CheckStatus::Pass
    );
}

#[test]
fn blank_unreadable_and_not_yet_seeded_state_have_distinct_recovery_results() {
    let dir = TempDir::new().unwrap();
    let blank_root = dir.path().join("blank");
    let unreadable_root = dir.path().join("unreadable");
    let empty_root = dir.path().join("empty");
    for root in [&blank_root, &unreadable_root] {
        fs::create_dir(root).unwrap();
        seed_project(
            root,
            root.file_name().unwrap().to_str().unwrap(),
            Engine::Claude,
        );
        let id = root.file_name().unwrap().to_str().unwrap();
        let paths = ProjectPaths::for_session(root, id);
        fs::write(paths.config(), br#"{"autonomy":"autopilot"}"#).unwrap();
    }
    let blank_paths = ProjectPaths::for_session(&blank_root, "blank");
    fs::write(blank_paths.brief(), "   ").unwrap();
    fs::remove_file(ProjectPaths::for_session(&unreadable_root, "unreadable").brief()).ok();
    fs::create_dir(ProjectPaths::for_session(&unreadable_root, "unreadable").brief()).unwrap();
    fs::create_dir(&empty_root).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![
            project("blank", &blank_root, true, "claude"),
            project("unreadable", &unreadable_root, true, "claude"),
            project("empty", &empty_root, true, "claude"),
        ],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert!(
        check(&report.checks, "project.0.goal")
            .summary
            .contains("blank")
    );
    assert!(
        check(&report.checks, "project.1.goal")
            .summary
            .contains("could not be read")
    );
    assert_eq!(
        check(&report.checks, "project.2.config").status,
        CheckStatus::Warn
    );
    assert!(
        check(&report.checks, "project.2.state")
            .summary
            .contains("no ledger")
    );
    assert!(
        check(&report.checks, "project-state.orphans.2")
            .summary
            .contains("no orphan")
    );
}

#[test]
fn oversized_runtime_json_and_missing_decider_terminal_are_recovery_warnings() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("alpha");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "alpha", Engine::Claude);
    let paths = ProjectPaths::for_session(&root, "alpha");
    fs::write(paths.config(), br#"{"autonomy":"autopilot"}"#).unwrap();
    fs::write(paths.brief(), "Resolve the pending decision.").unwrap();
    fs::write(paths.needs_you(), vec![b' '; 1024 * 1024 + 1]).unwrap();
    let mut ledger = AgentLoopState::fresh(Engine::Claude, None, 10);
    ledger.advice_inflight = Some(ParkedAdvice {
        seq: 11,
        stop_ids: Vec::new(),
        pane_dialog: true,
    });
    fs::write(paths.pmstate(), serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
    write_registry(
        &options(&dir).registry,
        vec![project("alpha", &root, true, "claude")],
    );

    let report = inspect_with(
        &options(&dir),
        &FakeProbe::healthy(vec![session_name("alpha", &root)]),
    );

    assert!(
        check(&report.checks, "project.0.marker")
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("maximum diagnostic size"))
    );
    assert_eq!(
        check(&report.checks, "tmux.supervisor.0").status,
        CheckStatus::Warn
    );
}

#[test]
fn tmux_list_failure_is_a_failure_without_session_guesses() {
    let dir = TempDir::new().unwrap();
    let probe = FakeProbe {
        binaries: ["notify-send"].into_iter().map(str::to_owned).collect(),
        tmux_version: Ok("tmux 3.5".into()),
        tmux_sessions: Err("permission denied".into()),
        dead_sessions: BTreeSet::new(),
        pane_error: None,
    };

    let report = inspect_with(&options(&dir), &probe);

    assert_eq!(
        check(&report.checks, "tmux.sessions").status,
        CheckStatus::Fail
    );
    assert!(
        report
            .checks
            .iter()
            .all(|check| !check.id.starts_with("tmux.session."))
    );
}

#[test]
fn empty_tmux_socket_fails_without_running_tmux() {
    let dir = TempDir::new().unwrap();
    let probe = FakeProbe {
        binaries: ["notify-send"].into_iter().map(str::to_owned).collect(),
        tmux_version: Err("must not be called".into()),
        tmux_sessions: Err("must not be called".into()),
        dead_sessions: BTreeSet::new(),
        pane_error: None,
    };
    let mut doctor_options = options(&dir);
    doctor_options.socket.clear();

    let report = inspect_with(&doctor_options, &probe);

    assert_eq!(
        check(&report.checks, "tmux.socket").status,
        CheckStatus::Fail
    );
    assert_eq!(
        check(&report.checks, "tmux.sessions").status,
        CheckStatus::Skip
    );
    assert!(report.checks.iter().all(|check| check.id != "tmux.version"));
}

/// Wait out the window in which a sibling test's fork still holds a just-written script's write
/// fd (CLOEXEC only takes effect at exec), which makes exec fail with ETXTBSY. The stubs here have
/// no side effects, so probing one by running it is harmless.
#[cfg(unix)]
fn wait_until_runnable(script: &std::path::Path) {
    const ETXTBSY: i32 = 26;
    for _ in 0..400 {
        match std::process::Command::new(script).arg("-V").output() {
            Err(error) if error.raw_os_error() == Some(ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => return,
        }
    }
    panic!("{} stayed busy", script.display());
}

#[cfg(unix)]
#[test]
fn native_probe_handles_version_sessions_no_server_and_path_permissions() {
    use std::os::unix::fs::PermissionsExt;

    use super::super::{NativeProbe, binary_in_path};

    let dir = TempDir::new().unwrap();
    let tmux = dir.path().join("tmux");
    fs::write(
        &tmux,
        "#!/bin/sh\nif [ \"$1\" = \"-V\" ]; then echo 'tmux fake'; exit 0; fi\nif [ \"$3\" = \"list-sessions\" ]; then printf 'one\\ntwo\\n'; exit 0; fi\nif [ \"$3\" = \"display-message\" ]; then echo 1; exit 0; fi\nexit 9\n",
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).unwrap();
    wait_until_runnable(&tmux);
    let probe = NativeProbe::with_tmux(tmux.clone());
    assert_eq!(probe.tmux_version().unwrap(), "tmux fake");
    assert_eq!(probe.tmux_sessions("doctor-test").unwrap(), ["one", "two"]);
    assert!(probe.tmux_pane_dead("doctor-test", "one").unwrap());

    let path = std::env::join_paths([dir.path()]).unwrap();
    assert!(binary_in_path("tmux", Some(&path)));
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!binary_in_path("tmux", Some(&path)));
    assert!(!binary_in_path("missing", None));

    let no_server = dir.path().join("no-server");
    fs::write(
        &no_server,
        "#!/bin/sh\nprintf 'no server running on test\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&no_server, fs::Permissions::from_mode(0o755)).unwrap();
    wait_until_runnable(&no_server);
    assert!(
        NativeProbe::with_tmux(no_server)
            .tmux_sessions("doctor-test")
            .unwrap()
            .is_empty()
    );

    let broken = dir.path().join("broken");
    fs::write(&broken, "#!/bin/sh\nprintf 'boom\\n' >&2\nexit 7\n").unwrap();
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
    wait_until_runnable(&broken);
    assert!(
        NativeProbe::with_tmux(broken)
            .tmux_sessions("doctor-test")
            .unwrap_err()
            .contains("boom")
    );
    assert!(
        NativeProbe::with_tmux(dir.path().join("broken"))
            .tmux_pane_dead("doctor-test", "one")
            .unwrap_err()
            .contains("boom")
    );

    let missing = NativeProbe::with_tmux(dir.path().join("missing-tmux"));
    assert!(
        missing
            .tmux_version()
            .unwrap_err()
            .contains("could not run")
    );
    assert!(
        missing
            .tmux_sessions("doctor-test")
            .unwrap_err()
            .contains("could not list")
    );
    assert!(
        missing
            .tmux_pane_dead("doctor-test", "one")
            .unwrap_err()
            .contains("could not inspect")
    );

    let silent = dir.path().join("silent");
    fs::write(&silent, "#!/bin/sh\nexit 8\n").unwrap();
    fs::set_permissions(&silent, fs::Permissions::from_mode(0o755)).unwrap();
    wait_until_runnable(&silent);
    assert!(
        NativeProbe::with_tmux(silent)
            .tmux_version()
            .unwrap_err()
            .contains("exited with")
    );
}

#[test]
fn public_run_is_read_only_when_the_registry_is_absent() {
    let dir = TempDir::new().unwrap();
    let doctor_options = DoctorOptions {
        registry: dir.path().join("missing.json"),
        socket: format!("doctor-native-{}", std::process::id()),
    };

    let report = super::super::run(&doctor_options);

    assert_eq!(report.registry, doctor_options.registry);
    assert!(!doctor_options.registry.exists());
}

#[test]
fn managed_launches_need_tmux_3_0_for_their_session_environment() {
    let dir = TempDir::new().unwrap();
    for (version, status) in [
        ("tmux 3.5", CheckStatus::Pass),
        ("tmux 3.0a", CheckStatus::Pass),
        ("tmux next-3.6", CheckStatus::Pass),
        ("tmux 10.1", CheckStatus::Pass),
        ("tmux 2.9a", CheckStatus::Fail),
        ("tmux 1.8", CheckStatus::Fail),
        ("tmux master", CheckStatus::Warn),
        ("tmux 3", CheckStatus::Warn),
    ] {
        let probe = FakeProbe {
            tmux_version: Ok(version.into()),
            ..FakeProbe::healthy(Vec::new())
        };
        let report = inspect_with(&options(&dir), &probe);
        let managed = check(&report.checks, "tmux.managed_env");
        assert_eq!(managed.status, status, "{version}: {managed:#?}");
        if status != CheckStatus::Pass {
            assert!(
                managed
                    .remediation
                    .as_deref()
                    .is_some_and(|fix| fix.contains("3.0")),
                "{version}: {managed:#?}"
            );
        }
        assert_eq!(
            check(&report.checks, "tmux.version").status,
            CheckStatus::Pass,
            "{version}"
        );
    }

    let probe = FakeProbe {
        tmux_version: Err("tmux not found".into()),
        ..FakeProbe::healthy(Vec::new())
    };
    let report = inspect_with(&options(&dir), &probe);
    assert!(
        report
            .checks
            .iter()
            .all(|check| check.id != "tmux.managed_env"),
        "no tmux, nothing to check"
    );
}

/// A DEAD OR INTERMITTENT TURN-END HOOK IS REPORTED, because for claude that signal is the only proof
/// a turn ended and a session without it holds until the report-debt ceiling. Diagnosing this by hand
/// on a live session took reading a ledger, a file stat and an argv; it should take one command.
#[test]
fn a_turn_end_hook_that_misses_turns_is_a_warning() {
    let dir = TempDir::new().unwrap();
    // healthy: the signal keeps up with the reports. lagging: many reports, one signal — the live
    // shape (661 signals against 668 reports). silent: reports with the hook never firing at all.
    let cases = [
        ("healthy", 20u64, Some(18usize)),
        ("lagging", 40, Some(1)),
        ("silent", 40, None),
    ];
    let mut rows = Vec::new();
    for (id, reports, turns) in cases {
        let root = dir.path().join(id);
        fs::create_dir(&root).unwrap();
        seed_project(&root, id, Engine::Claude);
        let paths = ProjectPaths::for_session(&root, id);
        let mut ledger: AgentLoopState =
            serde_json::from_slice(&fs::read(paths.pmstate()).unwrap()).unwrap();
        ledger.report_generation = reports;
        fs::write(paths.pmstate(), serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();
        if let Some(turns) = turns {
            fs::create_dir_all(paths.daemon_dir()).unwrap();
            fs::write(paths.turn_signal(), vec![b'.'; turns]).unwrap();
        }
        rows.push(project(id, &root, true, "claude"));
    }
    write_registry(&options(&dir).registry, rows);

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    assert_eq!(
        check(&report.checks, "project.0.turn_signal").status,
        CheckStatus::Pass,
        "a hook keeping up with the reports is healthy"
    );
    let lagging = check(&report.checks, "project.1.turn_signal");
    assert_eq!(lagging.status, CheckStatus::Warn);
    assert!(
        lagging.summary.contains("only some turns"),
        "{}",
        lagging.summary
    );
    assert!(
        lagging
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("1 turn-end signals against 40 accepted reports")),
        "the numbers a human needs are shown: {:?}",
        lagging.detail
    );
    let silent = check(&report.checks, "project.2.turn_signal");
    assert_eq!(silent.status, CheckStatus::Warn);
    assert!(silent.summary.contains("never fired"), "{}", silent.summary);
    // NEVER a failure: the hook is optional, and a fresh session has neither file nor reports.
    assert!(
        report
            .checks
            .iter()
            .filter(|c| c.id.ends_with("turn_signal"))
            .all(|c| c.status != CheckStatus::Fail)
    );
}

/// A FRESH SESSION IS NOT ACCUSED. No signal file and no reports yet is the ordinary starting state,
/// and so is a report or two landing before the first notification does.
#[test]
fn a_session_with_no_turns_yet_is_not_warned_about_its_hook() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("fresh");
    fs::create_dir(&root).unwrap();
    seed_project(&root, "fresh", Engine::Claude);
    write_registry(
        &options(&dir).registry,
        vec![project("fresh", &root, true, "claude")],
    );

    let report = inspect_with(&options(&dir), &FakeProbe::healthy(Vec::new()));

    let fresh = check(&report.checks, "project.0.turn_signal");
    assert_eq!(fresh.status, CheckStatus::Pass, "{}", fresh.summary);
    assert!(fresh.summary.contains("no turn-end signal yet"));
}
