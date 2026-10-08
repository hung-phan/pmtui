//! Shared fixtures for the daemon sweep's tests: the registry entries the sweep
//! reconciles and the per-session seed a `Mode::AgentLoop` row needs before it can
//! be driven. The area files below reach them through `use super::*`.

use super::*;
use crate::clock::test_support::FakeClock;
use crate::escalation::test_support::CaptureNotifier;
use crate::job::{AgentLoopState, JobRun};
use crate::pmstate::{OpenStop, StopKind, StopStatus};
use crate::registry::Mode;
use crate::state::{self, Config, ProjectPaths, Tier};
use crate::tmux::fake::FakeDriver;
use tempfile::TempDir;

mod agent_loop;
mod autopilot_gate;
mod reap;
mod stops;
mod sweep;

/// An agent-loop project: a fresh tempdir with a per-session `config.json` at `tier`
/// and a fresh Idle ledger, returned with its registry entry so the sweep can drive it.
fn agent_loop_project(id: &str, tier: Tier) -> (TempDir, ProjectEntry) {
    let dir = tempfile::tempdir().unwrap();
    seed_agent_loop_config(dir.path(), id, tier);
    crate::job::save(
        &ProjectPaths::for_session(dir.path(), id),
        &AgentLoopState::fresh(Engine::Claude, Some(300), 1000),
    )
    .unwrap();
    let entry = agent_loop_entry(id, dir.path());
    (dir, entry)
}

fn reg(entries: Vec<ProjectEntry>) -> Registry {
    Registry { projects: entries }
}

/// A `driver.lock` pmd has RELEASED is acquirable — checked with a bounded RETRY, not a single
/// shot. In a multi-threaded test process other tests fork+exec (git in `verify`, the escalation
/// notifier); a forked child inherits every open fd, so a lease fd a releasing sweep just dropped
/// stays briefly held until that child `exec`s (CLOEXEC only fires at exec). A single `try_acquire`
/// right after the sweep can land in that fork→exec window and read a false "still held". A REAL
/// owner holds for life and loses every retry, so this still fails loudly if the sweep did NOT free
/// the lock — the invariant these callers actually assert is "the sweep releases it", not "on the
/// very first syscall". Same false positive `acquire_with_retry` filters for pmd.
fn lock_is_free(lock: &std::path::Path) -> bool {
    crate::lease::acquire_with_retry(lock, 50, std::time::Duration::from_millis(10))
        .unwrap()
        .is_some()
}

/// Write a per-session config for an agent-loop session `id` under `root` at
/// `tier`.
///
/// The tier is an explicit parameter, not a default, because on a `Mode::AgentLoop`
/// row it now decides whether pmd DRIVES the session at all ([`pmd_drives_row`]).
/// A `Tier::Standard` fixture here would make every "it launches / it nudges /
/// it escalates" assertion below fail, and — worse — would make every "it does NOT
/// spawn" assertion pass VACUOUSLY (nothing is driven, so of course nothing spawns).
/// So each caller names the tier its behaviour belongs to.
fn seed_agent_loop_config(root: &std::path::Path, id: &str, tier: Tier) {
    let paths = ProjectPaths::for_session(root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: tier,
            step_timeout_s: 100,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
}

fn agent_loop_entry(id: &str, root: &std::path::Path) -> ProjectEntry {
    ProjectEntry {
        id: id.into(),
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
        cadence_s: Some(300),
    }
}
