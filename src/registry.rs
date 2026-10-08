//! The set of projects the daemon manages, persisted as a registry JSON file.
//! Per-project autonomy lives in each project's own `config.json`, not here.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::state;

fn default_true() -> bool {
    true
}

pub const DISPLAY_NAME_MAX_CHARS: usize = 64;

pub fn normalize_display_name(raw: &str) -> std::result::Result<Option<String>, &'static str> {
    if raw.chars().any(char::is_control) {
        return Err("name cannot contain control characters");
    }
    let name = raw.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err("name must be 64 characters or fewer");
    }
    Ok(Some(name.to_string()))
}

/// How a project is driven. The product has exactly one run mode: an "agent-loop"
/// session where `pmd` re-invokes a RESUMABLE agent on a per-session heartbeat to keep
/// working its goal until it is blocked or a human closes it. Driven by `JobScheduler`;
/// the agent self-manages its own channels via its MCP and the harness only re-invokes +
/// observes. Multiple agent-loop sessions may share one folder (state is per-session, see
/// [`crate::state::ProjectPaths::for_session`]).
///
/// A single-variant enum (rather than dropping the field) so the on-disk `"mode"` key and
/// its two TIERS — Standard (human drives) and Autopilot (pmd drives) — keep a stable
/// home, and a missing/`"agent_loop"` `mode` field still deserializes.
///
/// The retired legacy values `"auto"` (the old phase machine) and `"interactive"` (the old
/// raw hand-driven session) are accepted as DESERIALIZE aliases that fold into `AgentLoop`.
/// This is a fail-SAFE, not a feature: it keeps an old `registry.json` LOADABLE (the entry
/// survives as an undriven Standard row) rather than hard-erroring — which would make
/// `Registry::load(..).unwrap_or_default()` silently blank the whole project list and let the
/// next `save` overwrite every on-disk entry. Serialization always writes `"agent_loop"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    #[serde(alias = "auto", alias = "interactive")]
    AgentLoop,
}

/// The interactive CLI an interactive session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Claude,
    Codex,
}

impl Engine {
    /// Every engine, in a STABLE order — the one source the create-form toggle, the decider
    /// picker overlay, and the picker cursor all index into, so they cannot disagree about
    /// which engines exist or in what order.
    pub const ALL: [Engine; 2] = [Engine::Claude, Engine::Codex];

    /// The binary name to launch.
    pub fn bin(self) -> &'static str {
        match self {
            Engine::Claude => "claude",
            Engine::Codex => "codex",
        }
    }
    pub fn label(self) -> &'static str {
        self.bin()
    }
}

/// Most rows (spawned children plus their forks) that may name one parent in `spawned_by`.
pub const MAX_CHILDREN_PER_PARENT: usize = 5;

/// How far the spawn broker got with a row's one launch. Persisted before each side effect, so
/// a dashboard that dies mid-spawn leaves enough for its successor to finish without ever
/// sending the Message twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchState {
    /// Staged; the launch has not been tried.
    Pending,
    /// The launch was tried and its result is not yet known, or can never be proven.
    Attempted,
    /// A new terminal started with the child's argv.
    Started,
    /// Proven that tmux never ran the argv, so nothing read the Message.
    FailedBeforeStart,
}

/// The broker's final result for a spawned row; `None` on the record while unfinished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnOutcome {
    Ready,
    NeedsAttention,
    OutcomeUnknown,
}

/// What kind of agent a spawn launched into a row.
///
/// `Job` is a one-shot headless run that reports a result and EXITS, so the broker retires its row
/// when the process is gone. `Chat` is the interactive child v1 created, which no machine retires.
/// Deserialization defaults to `Chat`, so a row written before this field existed keeps its meaning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchKind {
    #[default]
    Chat,
    Job,
}

/// The launch a spawn request staged for its row. `args_hash` is fixed at staging, so later
/// renames or model changes never break replay matching on `(request_id, args_hash)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub request_id: String,
    pub args_hash: String,
    pub state: LaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SpawnOutcome>,
    /// Chat or job — see [`LaunchKind`]. Defaulted, never absent in a row this build writes.
    #[serde(default)]
    pub kind: LaunchKind,
    /// The branch a JOB's own git worktree was created on, and the commit it started from.
    ///
    /// Both absent for a chat child and for a project that is not a git repository, where a job runs in
    /// the project directory as before. The worktree's PATH is not stored because it is derived from the
    /// child's state directory — one place decides that layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
}

/// One managed project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectEntry {
    /// Stable id (also sanitized into tmux session names).
    pub id: String,
    /// Optional human-readable label. The stable id remains the runtime key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Absolute path to the project root (contains `.project-state/`).
    pub root: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The run mode. Exactly one variant ([`Mode::AgentLoop`]); a missing field defaults
    /// to it, so every current and legacy `agent_loop` entry loads.
    #[serde(default)]
    pub mode: Mode,
    /// Which CLI the session launches.
    #[serde(default)]
    pub engine: Option<Engine>,
    /// The MODEL the WORKER REPL launches with, launch-ready (claude: stable alias or
    /// provider-specific id; codex: slug).
    /// `None` (default) passes no `--model`/`-m`, so the CLI uses its own default. A launch
    /// parameter beside `engine`; `pmtui` is the only writer, applied on the next (re)launch.
    #[serde(default)]
    pub worker_model: Option<String>,
    /// Optional Standard-session Message submitted only by the fresh create path.
    /// Retained as launch provenance; resume and restart never replay it.
    #[serde(default)]
    pub initial_prompt: Option<String>,
    /// Human-owned title displayed by the Task view. The linked managed
    /// session remains the task's execution record.
    #[serde(default)]
    pub task_title: Option<String>,
    /// Managed-session lineage. A fork is still a normal session; this points to
    /// the source row so board and detail views can explain the relationship.
    #[serde(default)]
    pub forked_from: Option<String>,
    /// Spawn lineage: the session whose agent asked for this one through a spawn request.
    /// A fork copies it from its source, so a child's forks stay under the same parent's
    /// depth rule and [`MAX_CHILDREN_PER_PARENT`] cap. A row with this set cannot parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned_by: Option<String>,
    /// The spawn broker's record of the one launch it owes this row. Only the broker writes
    /// it; interactive New and fork leave it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchRecord>,
    /// Agent-loop sessions: the pinned (claude) or captured (codex) conversation
    /// id to resume each wake. `None` until the first wake mints/captures it.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Agent-loop sessions: per-session heartbeat cadence in seconds. `None` ⇒ the
    /// harness default. Two sessions in one folder may set different cadences.
    #[serde(default)]
    pub cadence_s: Option<u64>,
}

impl ProjectEntry {
    /// Disabled and still mid-spawn: `launch.state` is `Pending`, `Attempted` or
    /// `FailedBeforeStart`. Every start path refuses such a row until the broker finishes it.
    /// A JOB row: a one-shot headless child that reports a result and exits.
    ///
    /// Nothing a human or the daemon starts applies to one. It has no conversation to resume (claude
    /// runs it with a pinned id but the run is over; codex never had a caller-chosen one), no agent
    /// waiting for a Message, and no autonomy dial — the broker retires it when its process is gone.
    pub fn is_job(&self) -> bool {
        self.launch
            .as_ref()
            .is_some_and(|launch| launch.kind == LaunchKind::Job)
    }

    pub fn is_staged_spawn(&self) -> bool {
        !self.enabled
            && self.launch.as_ref().is_some_and(|launch| {
                matches!(
                    launch.state,
                    LaunchState::Pending | LaunchState::Attempted | LaunchState::FailedBeforeStart
                )
            })
    }
}

/// The daemon's registry file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

impl Registry {
    /// Load from `path`, or an empty registry when the file is absent.
    pub fn load(path: &Path) -> Result<Registry> {
        state::read_json_or(path, Registry::default())
    }

    /// Persist atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        state::write_json_atomic(path, self)
    }

    /// Read-MODIFY-write that refuses to clobber a CORRUPT registry. An ABSENT file defaults (a
    /// first write is legitimate); a PRESENT-but-unparseable one returns `Err` WITHOUT saving —
    /// so a truncated/0-byte/hand-broken `registry.json` can never be silently overwritten with
    /// `f`'s result and lose every registered project.
    ///
    /// This exists because the mutating idiom used to be `Registry::load(path).unwrap_or_default()`
    /// then push/modify + `save`, and `.unwrap_or_default()` turns the corrupt-file `Err` that
    /// [`load`](Self::load) correctly returns into an EMPTY registry — which the following `save`
    /// then makes permanent. Routing every write through here makes that data-loss impossible by
    /// construction: `load(path)?` propagates the corrupt `Err` before `f` or `save` can run.
    pub fn update(path: &Path, f: impl FnOnce(&mut Registry)) -> Result<()> {
        let mut reg = Registry::load(path)?;
        f(&mut reg);
        reg.save(path)
    }

    /// The enabled projects the daemon should drive.
    pub fn enabled(&self) -> impl Iterator<Item = &ProjectEntry> {
        self.projects.iter().filter(|p| p.enabled)
    }

    /// Rows (enabled or not) whose `spawned_by` is `parent`.
    pub fn children_of(&self, parent: &str) -> usize {
        self.projects
            .iter()
            .filter(|p| p.spawned_by.as_deref() == Some(parent))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(id: &str, enabled: bool) -> ProjectEntry {
        ProjectEntry {
            id: id.into(),
            display_name: None,
            root: PathBuf::from(format!("/tmp/{id}")),
            enabled,
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
        }
    }

    #[test]
    fn round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let reg = Registry {
            projects: vec![entry("a", true), entry("b", false)],
        };
        reg.save(&path).unwrap();
        let back = Registry::load(&path).unwrap();
        assert_eq!(back.projects, reg.projects);
        assert_eq!(Engine::Claude.label(), "claude");
        assert_eq!(Engine::Codex.label(), "codex");
    }

    #[test]
    fn missing_registry_is_empty() {
        let dir = tempdir().unwrap();
        let reg = Registry::load(&dir.path().join("nope.json")).unwrap();
        assert!(reg.projects.is_empty());
    }

    #[test]
    fn enabled_default_true_when_field_absent() {
        let json = r#"{ "projects": [ { "id": "a", "root": "/tmp/a" } ] }"#;
        let reg: Registry = serde_json::from_str(json).unwrap();
        assert!(reg.projects[0].enabled);
    }

    #[test]
    fn agent_loop_entry_round_trips_and_missing_mode_defaults() {
        // AgentLoop mode + resume/cadence fields round-trip; a row with no `mode`
        // field still loads and defaults to AgentLoop via serde.
        let json = r#"{ "projects": [
            { "id": "bot", "display_name": "Release work", "root": "/tmp/bot", "mode": "agent_loop",
              "engine": "claude", "task_title": "Ship task view", "forked_from": "parent", "conversation_id": "uuid-1", "cadence_s": 300 },
            { "id": "old", "root": "/tmp/old" } ] }"#;
        let reg: Registry = serde_json::from_str(json).unwrap();
        let bot = &reg.projects[0];
        assert_eq!(bot.mode, Mode::AgentLoop);
        assert_eq!(bot.display_name.as_deref(), Some("Release work"));
        assert_eq!(bot.engine, Some(Engine::Claude));
        assert_eq!(bot.task_title.as_deref(), Some("Ship task view"));
        assert_eq!(bot.forked_from.as_deref(), Some("parent"));
        assert_eq!(bot.conversation_id.as_deref(), Some("uuid-1"));
        assert_eq!(bot.cadence_s, Some(300));
        let old = &reg.projects[1];
        assert!(old.display_name.is_none());
        assert_eq!(old.mode, Mode::AgentLoop);
        assert!(
            old.task_title.is_none()
                && old.forked_from.is_none()
                && old.conversation_id.is_none()
                && old.cadence_s.is_none()
        );
        // Re-serialize + re-parse to confirm the `agent_loop` rename is stable.
        let round: Registry = serde_json::from_str(&serde_json::to_string(&reg).unwrap()).unwrap();
        assert_eq!(round.projects[0].mode, Mode::AgentLoop);
    }

    #[test]
    fn legacy_retired_modes_load_as_agent_loop_never_wipe_the_registry() {
        // FAIL-SAFE: a pre-purge registry.json carrying the retired `"auto"`/`"interactive"`
        // modes must still LOAD (folding to AgentLoop), not hard-error. A parse error here
        // would make `Registry::load(..).unwrap_or_default()` silently return an EMPTY list,
        // and the next `save` would overwrite every on-disk entry — data loss.
        let json = r#"{ "projects": [
            { "id": "phase", "root": "/tmp/phase", "mode": "auto" },
            { "id": "raw",   "root": "/tmp/raw",   "mode": "interactive" },
            { "id": "now",   "root": "/tmp/now",   "mode": "agent_loop" } ] }"#;
        let reg: Registry = serde_json::from_str(json).expect("legacy modes must not error");
        assert_eq!(reg.projects.len(), 3, "no entry may be dropped on load");
        for p in &reg.projects {
            assert_eq!(p.mode, Mode::AgentLoop, "{} should fold to AgentLoop", p.id);
        }
    }

    #[test]
    fn update_appends_without_wiping_and_refuses_a_corrupt_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("registry.json");

        // Absent: `update` starts from default and writes the first entry.
        Registry::update(&path, |r| r.projects.push(entry("a", true))).unwrap();
        assert_eq!(Registry::load(&path).unwrap().projects.len(), 1);

        // Present + valid: a second update APPENDS, keeping the first — no wipe.
        Registry::update(&path, |r| r.projects.push(entry("b", true))).unwrap();
        let ids: Vec<_> = Registry::load(&path)
            .unwrap()
            .projects
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);

        // CORRUPT: a garbage file must make `update` Err WITHOUT saving — the data-loss guard.
        // The old `load().unwrap_or_default()` + push + save would have wiped it to a one-entry
        // list here.
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(
            Registry::update(&path, |r| r.projects.push(entry("c", true))).is_err(),
            "update must refuse a corrupt registry, not default-then-save"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json",
            "the corrupt bytes are left untouched — nothing was overwritten"
        );
    }

    #[test]
    fn enabled_filter() {
        let reg = Registry {
            projects: vec![entry("a", true), entry("b", false), entry("c", true)],
        };
        let ids: Vec<&str> = reg.enabled().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn entry_without_worker_model_loads_as_none() {
        let json =
            r#"{"id":"x","root":"/tmp","enabled":true,"mode":"agent_loop","engine":"claude"}"#;
        let e: ProjectEntry = serde_json::from_str(json).expect("legacy entry must parse");
        assert_eq!(e.worker_model, None);
    }

    #[test]
    fn worker_model_round_trips() {
        let json = r#"{"id":"x","root":"/tmp","enabled":true,"mode":"agent_loop","engine":"codex","worker_model":"openai.gpt-5.6-sol"}"#;
        let e: ProjectEntry = serde_json::from_str(json).expect("must parse");
        assert_eq!(e.worker_model.as_deref(), Some("openai.gpt-5.6-sol"));
    }

    fn launch(state: LaunchState, outcome: Option<SpawnOutcome>) -> LaunchRecord {
        LaunchRecord {
            request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            args_hash: "ab12".into(),
            state,
            outcome,
            kind: LaunchKind::Job,
            branch: None,
            base_commit: None,
        }
    }

    #[test]
    fn legacy_entry_without_spawn_fields_loads_with_none() {
        let json =
            r#"{"id":"x","root":"/tmp","enabled":false,"mode":"agent_loop","engine":"claude"}"#;
        let e: ProjectEntry = serde_json::from_str(json).expect("legacy entry must parse");
        assert_eq!(e.spawned_by, None);
        assert_eq!(e.launch, None);
        assert!(
            !e.is_staged_spawn(),
            "a paused row with no launch record is not a staged spawn"
        );
    }

    #[test]
    fn launch_record_round_trips_snake_case() {
        let cases = [
            (LaunchState::Pending, None, r#""state":"pending""#),
            (LaunchState::Attempted, None, r#""state":"attempted""#),
            (LaunchState::Started, None, r#""state":"started""#),
            (
                LaunchState::FailedBeforeStart,
                None,
                r#""state":"failed_before_start""#,
            ),
            (
                LaunchState::Started,
                Some(SpawnOutcome::Ready),
                r#""outcome":"ready""#,
            ),
            (
                LaunchState::Started,
                Some(SpawnOutcome::NeedsAttention),
                r#""outcome":"needs_attention""#,
            ),
            (
                LaunchState::Attempted,
                Some(SpawnOutcome::OutcomeUnknown),
                r#""outcome":"outcome_unknown""#,
            ),
        ];
        for (state, outcome, needle) in cases {
            let mut e = entry("child", false);
            e.spawned_by = Some("parent".into());
            e.launch = Some(launch(state, outcome));
            let json = serde_json::to_string(&e).unwrap();
            assert!(json.contains(needle), "{needle} missing from {json}");
            assert!(json.contains(r#""spawned_by":"parent""#), "{json}");
            assert!(json.contains(r#""args_hash":"ab12""#), "{json}");
            let back: ProjectEntry = serde_json::from_str(&json).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn absent_spawn_fields_are_not_serialized() {
        let json = serde_json::to_string(&entry("a", true)).unwrap();
        assert!(!json.contains("spawned_by"), "{json}");
        assert!(!json.contains("launch"), "{json}");
        let json = serde_json::to_string(&launch(LaunchState::Pending, None)).unwrap();
        assert!(!json.contains("outcome"), "{json}");
    }

    #[test]
    fn children_of_counts_enabled_and_disabled_rows() {
        let row = |id: &str, enabled: bool, parent: Option<&str>| {
            let mut e = entry(id, enabled);
            e.spawned_by = parent.map(str::to_string);
            e
        };
        let reg = Registry {
            projects: vec![
                row("p", true, None),
                row("c1", true, Some("p")),
                row("c2", false, Some("p")),
                row("c2-fork", true, Some("p")),
                row("other", true, Some("q")),
            ],
        };
        assert_eq!(reg.children_of("p"), 3);
        assert_eq!(reg.children_of("q"), 1);
        assert_eq!(reg.children_of("c1"), 0);
        assert_eq!(MAX_CHILDREN_PER_PARENT, 5);
    }

    #[test]
    fn only_a_disabled_row_mid_spawn_is_staged() {
        for (state, enabled, staged) in [
            (LaunchState::Pending, false, true),
            (LaunchState::Attempted, false, true),
            (LaunchState::FailedBeforeStart, false, true),
            (LaunchState::Started, false, false),
            (LaunchState::Pending, true, false),
            (LaunchState::Attempted, true, false),
        ] {
            let mut e = entry("child", enabled);
            e.launch = Some(launch(state, None));
            assert_eq!(e.is_staged_spawn(), staged, "{state:?} enabled={enabled}");
        }
    }
}
