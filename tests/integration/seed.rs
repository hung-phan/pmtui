//! Writing the on-disk shape a session already has before a test starts: the
//! registry entry plus the per-session config/brief/ledger that pmtui's intake would
//! have written. Seeding directly is what lets a test begin from a state the create
//! form cannot produce — a Standard row with no daemon behind it, or a heartbeat
//! cadence below the form's 60s floor.

use std::path::Path;

use agent_manager::clock::{Clock, SystemClock};
use agent_manager::registry::{Engine, Mode, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};

/// Seed one agent-loop session's on-disk state: an Autopilot config, the goal, and a
/// fresh Idle ledger on a 1s cadence — the shape pmtui's intake writes.
pub(crate) fn seed_supervisor_session(root: &Path, id: &str, goal: &str) -> ProjectPaths {
    let paths = ProjectPaths::for_session(root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
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
    state::write_text_atomic(&paths.brief(), goal).unwrap();
    agent_manager::job::save(
        &paths,
        &agent_manager::job::AgentLoopState::fresh(Engine::Claude, Some(1), SystemClock.now()),
    )
    .unwrap();
    paths
}

/// A CONSULTABLE auto-flow decision: low-risk, so Autopilot auto-flows it, and it really
/// asks something with enumerated options, so the supervisor seam is reached.
pub(crate) const CONSULTABLE_MARKER: &str = r#"{"seq":9,"state":"blocked","status":"picking a formatter","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Which formatter for the changelog?","options":["prettier","dprint"]}]}"#;

/// Seed a STANDARD `Mode::AgentLoop` session directly on disk — registry entry plus the
/// per-session config/brief/ledger `submit_create` would write.
///
/// Deliberately NOT driven through the create form: `submit_create` ensures a daemon
/// (that is m12's fix), and the `pmd DOWN` half of the test below needs a row on screen
/// with NO pmd yet. `pmtui` re-reads the registry on every idle tick, so a session that
/// appears underneath a running dashboard simply shows up.
pub(crate) fn seed_standard_loop_session(reg_path: &Path, root: &Path, id: &str) {
    seed_standard_loop_session_at(reg_path, root, id, 300);
}

/// [`seed_standard_loop_session`] with an explicit heartbeat `cadence_s`, for a test that
/// has to WATCH the cadence fire. The create form floors cadence at 60s (`adjust_cadence`),
/// which is a sane human minimum but far longer than a test can wait for a SECOND nudge —
/// and observing a second one is the only way to prove the "no further nudge after `m`"
/// assertion is not just "there was never going to be another one".
pub(crate) fn seed_standard_loop_session_at(
    reg_path: &Path,
    root: &Path,
    id: &str,
    cadence_s: u64,
) {
    let mut reg = Registry::load(reg_path).unwrap_or_default();
    reg.projects.push(agent_manager::registry::ProjectEntry {
        id: id.to_string(),
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
        cadence_s: Some(cadence_s),
    });
    reg.save(reg_path).unwrap();
    let sp = ProjectPaths::for_session(root, id);
    state::write_json_atomic(
        &sp.config(),
        &Config {
            autonomy: Tier::Standard,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    // A goal on disk, because the `m` press below flips INTO Autopilot and that flip is
    // refused without one (`cycle_tier`'s direction gate).
    state::write_text_atomic(&sp.brief(), "keep the fixture green\n").unwrap();
    agent_manager::job::save(
        &sp,
        &agent_manager::job::AgentLoopState::fresh(
            Engine::Claude,
            Some(cadence_s),
            SystemClock.now(),
        ),
    )
    .unwrap();
}
