# Pmtui Spawn Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An agent inside a pmtui session runs `"$PMTUI_BIN" spawn --message … --json`. The command writes a request into the agent's own session state dir. The single dashboard turns that request into a Standard child session, which shows on the Task board with its title and a `from <parent>` link.

**Architecture:**
- A pure library module `agent_manager::spawn` owns the request/receipt schema and the hardened file I/O.
- The `pmtui spawn` subcommand only publishes a request and waits for its receipt.
- The dashboard broker is a non-blocking state machine stepped every frame. It validates, stages a disabled row, launches through the typed `launch_interactive`, checks readiness, and writes receipts.
- Launches carry a `ManagedEnv` (session id, state dir, pmtui path) so agents know who they are.

**Tech Stack:** Rust 2024, ratatui, tmux 3.6 (`new-session -e`), serde/serde_json, sha2, `rustix` flock (`lease`), FakeDriver/ScriptedTmux test doubles.

**Spec:** `docs/superpowers/specs/2026-09-29-pmtui-spawn-command-design.md` (approved 2026-09-29). Read it before any task. It is the contract; this plan is the route.

## Global Constraints

- The registry is written only on the dashboard's event-loop thread. The `pmtui spawn` command never reads or writes the registry, never calls tmux, and never takes the dashboard singleton.
- The request file is agent-owned: published once, never overwritten. It is created as a temp file and then `std::fs::hard_link`ed to its final name. The receipt is dashboard-owned and replaced atomically (`state::write_json_atomic`).
- Request dir `<state_dir>/spawn-requests/`, mode `0700`.
- Request files are named `<lowercase-uuid>.request.json`, receipts `<lowercase-uuid>.receipt.json`.
- Limits:
  - at most 64 KiB per request;
  - at most 16 unfinished requests processed per parent dir;
  - at most 5 existing rows per parent;
  - titles at most 120 chars;
  - `--wait` defaults to 20 s, maximum 120, `0` allowed;
  - readiness window 3 s, with two observations at least 100 ms apart;
  - receipt retention 7 days after final.
- Children are Standard only. Depth 1: a row with `spawned_by` cannot parent.
- The Message is launched exactly once, and only from a fresh stage. It is never re-sent from `attempted`. It is never echoed in receipts or output.
- Exit codes: `0` ready; `3` queued / in_progress / needs_attention; `1` failed / outcome_unknown; `2` usage_error.
- Timestamps are `Epoch` (`i64` seconds, `agent_manager::clock`), not RFC 3339 strings. This deviates from the spec's example JSON, which the spec does not bind; follow the repo convention.
- AGENTS.md rules apply:
  - tests go in behavior-named files in the module's `tests/` tree, registered in `tests/mod.rs`;
  - 95% region/function/line coverage per production file;
  - every behavior change updates `docs/SPEC.md` in the same change;
  - never auto-approve Codex trust;
  - keep the input.lock invariant.
- Verification during tasks:
  - run `cargo build --locked`, `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`;
  - do NOT run the ignored real-tmux suite (it costs real tokens); only Task 8 runs it, once.

## Review Focus

1. **A Message sent twice.** A replayed request id after a dashboard crash between launch and receipt must never relaunch. Task 5 pins it with a recovery test in which the `attempted` row has no terminal.
2. **The dashboard never freezes.** No broker phase may sleep or poll inside a frame. Task 5 pins it by asserting `step_spawns` returns without sleeping (FakeDriver records no `sleep` calls, and readiness spans several `step_spawns` calls).
3. **A hostile or broken request file.** Symlinks, oversized files, non-UUID names, wrong `parent_session` and truncated JSON must be rejected without a panic and without staging. Task 3 pins it with scan tests; Task 5 pins that no row appears.
4. **A spawn from a session launched before this change** (no env) gives a clear `not_in_a_session`, not a crash. Task 4 pins it.
5. **The Codex sandbox (read-only `$HOME`)** must still be able to publish. The command touches only `$PMTUI_STATE_DIR`. Task 4 pins that the command never resolves `~/.config`, using an env with `HOME` pointed at a read-only temp dir. Task 8 pins it for real.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `src/tmux/launch.rs` (new) | `ManagedEnv`, `LaunchOutcome`, `LaunchError` | 1 |
| `src/tmux/driver.rs`, `src/tmux/real.rs`, `src/tmux/fake.rs` | typed `launch_interactive` + `-e` env | 1 |
| `src/registry.rs` | `spawned_by`, `LaunchRecord`, `LaunchState`, `SpawnOutcome`, `is_staged_spawn`, `children_of` | 2 |
| `src/spawn/{mod,request,receipt,files}.rs` (new) | schema, normalization/hash, hardened file I/O | 3 |
| `src/bin/pmtui/spawn_cli.rs` (new) + `main.rs` action enum | the `pmtui spawn` command | 4 |
| `src/bin/pmtui/app/spawning.rs` (new) | dashboard broker state machine + recovery | 5 |
| `src/view/mod.rs`, `app/refresh.rs`, `render/board.rs`, `render/preview/head.rs`, `render/rows.rs` | lineage + `starting…` | 6 |
| `skills/pmtui-spawn/SKILL.md` (new), `src/skills.rs`, `src/doctor/checks.rs` | skill, installer, doctor | 7 |
| `tests/integration/spawn.rs` (new) | real-tmux acceptance | 8 |

Execution waves: **Wave 1** = Tasks 1, 2, 3 in parallel. **Wave 2** = Tasks 4, 5, 6, 7 in parallel. They share only the wave-1 interfaces; conflicts are resolved at merge. **Wave 3** = Task 8.

---

### Task 1: Typed launch outcome and ManagedEnv

**Files:**
- Create: `src/tmux/launch.rs`
- Modify:
  - `src/tmux/mod.rs` (declare `launch`, re-export)
  - `src/tmux/driver.rs:130` (trait method)
  - `src/tmux/real.rs:421-470` (`launch_interactive`)
  - `src/tmux/fake.rs:322` (record env and scripted outcomes)
  - callers: `src/bin/pmtui/app/create.rs:540,572`, `src/bin/pmtui/session.rs:277,328`, `src/bin/pmtui/app/forking.rs:282`, `src/job_engine/session.rs:105`
  - pmd construction of `JobScheduler` (`src/daemon/…`, find it with `rg "JobScheduler::new|JobScheduler \{"`)
- Test:
  - `src/tmux/tests/launch.rs` (new; register it in `src/tmux/tests/mod.rs`)
  - extend `src/tmux/tests/real.rs` (ScriptedTmux argv assertions)

**Interfaces:**
- Produces:
```rust
// src/tmux/launch.rs
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManagedEnv {
    pub session_id: String,
    pub state_dir: std::path::PathBuf,
    pub pmtui_bin: Option<std::path::PathBuf>,
}
impl ManagedEnv {
    /// The `(KEY, VALUE)` pairs applied with `tmux new-session -e KEY=VALUE`, in this order:
    /// PMTUI_SESSION, PMTUI_STATE_DIR, then PMTUI_BIN when `pmtui_bin` is Some.
    pub fn vars(&self) -> Vec<(String, String)>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOutcome { Started, AlreadyAlive }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    EmptyArgv,
    NotOnPath(String),
    CommandTooLong { bytes: usize },
    NewSessionFailed(String),
    ExitedAfterStart(String),
    Probe(String), // an is_alive/tmux I/O error: ambiguous
}
impl LaunchError {
    /// True only when tmux was never invoked with our argv: EmptyArgv, NotOnPath, CommandTooLong.
    pub fn proven_not_started(&self) -> bool;
}
impl std::fmt::Display for LaunchError { /* keep today's messages verbatim, e.g. "{bin:?} was not found on PATH — is it installed?" */ }
impl std::error::Error for LaunchError {}
pub const ENV_SESSION: &str = "PMTUI_SESSION";
pub const ENV_STATE_DIR: &str = "PMTUI_STATE_DIR";
pub const ENV_BIN: &str = "PMTUI_BIN";
/// Canonical pmtui path for a pmtui process (`current_exe`) or pmd (its sibling `pmtui`, None if absent).
pub fn pmtui_bin_for_current_process() -> Option<std::path::PathBuf>;
```
- Trait signature (all impls and callers):
```rust
fn launch_interactive(&self, session: &str, cwd: &Path, argv: &[String], env: &ManagedEnv)
    -> std::result::Result<LaunchOutcome, LaunchError>;
```
- Existing callers treat `Ok(_)` as today's `Ok(())`, and map `Err(e)` to their current string via `e.to_string()`, so user-visible behavior does not change. The broker (Task 5) is the first caller that branches on the variants.
- `FakeDriver` gains `pub fn launched_env(&self) -> Vec<(String, ManagedEnv)>` and `pub fn arm_launch_error(&self, session: &str, err: LaunchError)` (one-shot). A launch on an already-alive session returns `AlreadyAlive` without recording argv.

- [ ] **Step 1: Failing tests** in `src/tmux/tests/launch.rs`:
  - `vars_orders_session_state_dir_then_bin_and_omits_absent_bin`;
  - `only_pre_tmux_failures_are_proven_not_started`, asserting `NotOnPath` / `CommandTooLong` / `EmptyArgv` give `true`, and `NewSessionFailed` / `ExitedAfterStart` / `Probe` give `false`.

  In `src/tmux/tests/real.rs` (ScriptedTmux, which records argv):
  - `interactive_launch_passes_each_managed_var_with_dash_e_before_the_command`, asserting `-e PMTUI_SESSION=s1 -e PMTUI_STATE_DIR=/x/.project-state/sessions/s1-… -e PMTUI_BIN=/bin/pmtui` appear after `-y <rows>` and before the command string;
  - `already_alive_session_returns_already_alive_and_does_not_run_new_session`;
  - `new_session_nonzero_with_absent_session_is_new_session_failed`;
  - `a_session_that_dies_right_after_creation_is_exited_after_start`.
- [ ] **Step 2:** `cargo test --lib tmux::tests::launch` fails because the types don't exist yet.
- [ ] **Step 3: Implement.** Replace each `bail!` in `real.rs::launch_interactive` with the matching variant:
  - empty argv → `EmptyArgv`;
  - missing binary → `NotOnPath`;
  - over budget → `CommandTooLong`;
  - non-zero `new-session` and not alive → `NewSessionFailed`;
  - created then dead on the re-check → `ExitedAfterStart`;
  - tmux I/O errors → `Probe`.

  Before `new-session`, return `Ok(AlreadyAlive)` when the session is already alive. After a non-zero exit, a racing creator that left the session alive also returns `Ok(AlreadyAlive)`.

  Insert `-e KEY=VALUE` pairs from `env.vars()`. Keep the size check on the command string only, as today.
- [ ] **Step 4:** Thread `ManagedEnv` through every caller:
  - pmtui builds it from `(id, ProjectPaths::for_session(root,id).state_dir(), pmtui_bin_for_current_process())`;
  - `JobScheduler` gets a new `pmtui_bin: Option<PathBuf>` field, set once by pmd from `pmtui_bin_for_current_process()`, and builds `ManagedEnv` from `self.project_id` and `self.paths.state_dir()`;
  - tests construct `ManagedEnv::default()` or a literal.
- [ ] **Step 5:** `cargo build --locked && cargo clippy --all-targets -- -D warnings && cargo test` pass. Existing launch tests pass unchanged except for the signature.
- [ ] **Step 6:** Update `docs/SPEC.md`, in the section that describes terminal launch, to name the three env vars. Then commit: `feat(tmux): typed interactive launch outcomes and managed session env`.

### Task 2: Registry fields, staged rows, fork lineage and cap

**Files:**
- Modify:
  - `src/registry.rs:83-128` (fields and types)
  - `src/bin/pmtui/app/forking.rs` (copy `spawned_by`; cap check; generalize `incomplete_fork_refusal` at ~:571-600)
  - the refusal call sites `app/autopilot.rs:46`, `app/lifecycle.rs:50,141,230`, `app/sending.rs:28`, and the fork source check in `fork_selected`
  - every `ProjectEntry { … }` literal (the compiler lists them)
- Test:
  - `src/registry.rs` existing inline tests: add serde cases in the same style (the file already uses inline tests);
  - `src/bin/pmtui/tests/spawn_rows.rs` (new, registered in `src/bin/pmtui/tests/mod.rs`);
  - extend `src/bin/pmtui/tests/forking.rs`.

**Interfaces:**
- Produces (in `agent_manager::registry`):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchState { Pending, Attempted, Started, FailedBeforeStart }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnOutcome { Ready, NeedsAttention, OutcomeUnknown }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub request_id: String,
    pub args_hash: String,
    pub state: LaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SpawnOutcome>,
}
// ProjectEntry gains (both #[serde(default, skip_serializing_if = "Option::is_none")]):
pub spawned_by: Option<String>,
pub launch: Option<LaunchRecord>,
impl ProjectEntry {
    /// Disabled and still mid-spawn: launch.state is Pending, Attempted or FailedBeforeStart.
    pub fn is_staged_spawn(&self) -> bool;
}
impl Registry {
    /// Rows (enabled or not) whose spawned_by == parent.
    pub fn children_of(&self, parent: &str) -> usize;
}
pub const MAX_CHILDREN_PER_PARENT: usize = 5;
```
- pmtui: the fn `incomplete_fork_refusal(entry) -> Option<String>` is renamed to `start_refusal(entry) -> Option<String>`, and `refuse_incomplete_fork` to `refuse_unstartable` (same bool contract).
  - For a staged spawn it returns `"<id> is still being created by a spawn request"`.
  - The incomplete-fork text is unchanged.
  - Fork refuses a staged source through the same function.

- [ ] **Step 1: Failing tests.**
  - `registry` tests:
    - `legacy_entry_without_spawn_fields_loads_with_none`;
    - `launch_record_round_trips_snake_case`;
    - `absent_spawn_fields_are_not_serialized`;
    - `children_of_counts_enabled_and_disabled_rows`.
  - `tests/spawn_rows.rs`:
    - `a_staged_spawn_row_refuses_enter_restart_autopilot_send_pause_and_fork`: build an App on a FakeDriver with one row `enabled:false, launch: Some(Pending)`; call each handler; assert the status equals the refusal text and that `launched()` is empty;
    - `a_started_spawn_row_is_not_staged`.
  - `tests/forking.rs`:
    - `a_fork_of_a_spawned_child_copies_spawned_by`;
    - `forking_a_child_is_refused_when_its_parent_has_five_children`, with the status `"parent <p> already has 5 spawned sessions"` and nothing staged.
- [ ] **Step 2:** The tests fail to compile or fail.
- [ ] **Step 3: Implement.** Add the fields and types, `is_staged_spawn` and `children_of`, and the rename plus the staged-spawn arm. In `fork_selected`, copy `source.spawned_by` into the child; when `Some(parent)`, refuse if `registry.children_of(parent) >= MAX_CHILDREN_PER_PARENT`.
- [ ] **Step 4:** `cargo test` passes and clippy is clean.
- [ ] **Step 5:** In `docs/SPEC.md`, add `spawned_by` and `launch` to the registry row description, add staged-row refusal to lifecycle, and add fork lineage copying. Commit: `feat(registry): spawn lineage, launch record and staged-row refusal`.

### Task 3: Spawn protocol library

**Files:**
- Create:
  - `src/spawn/mod.rs`, `src/spawn/request.rs`, `src/spawn/receipt.rs`, `src/spawn/files.rs`
  - `src/spawn/tests/{mod,request,receipt,files}.rs`
- Modify: `src/lib.rs` (`pub mod spawn;`)

**Interfaces:**
- Consumes: `agent_manager::registry::{Engine, LaunchState}`. Task 2 adds `LaunchState`; if Task 2 has not merged yet, define `LaunchState` exactly as in Task 2 inside `registry.rs` yourself, since the definitions are identical and the merge dedupes them.
- Produces:
```rust
// src/spawn/mod.rs
pub const SCHEMA_VERSION: u32 = 1;
pub const REQUESTS_DIR: &str = "spawn-requests";
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
pub const MAX_UNFINISHED_PER_PARENT: usize = 16;
pub const TITLE_MAX_CHARS: usize = 120;
pub const RETENTION_SECS: i64 = 7 * 24 * 3600;
pub use request::{SpawnArgs, SpawnRequest};
pub use receipt::{ErrorCode, NextAction, NextActionKind, ReceiptSession, ReceiptState, SpawnError, SpawnReceipt};
pub use files::{Publish, ScanEntry, is_valid_request_id, publish_request, read_receipt, receipt_path,
                remove_pair, request_path, requests_dir, scan_requests, write_receipt};

// request.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnArgs {
    pub message: String,
    pub title: Option<String>, pub name: Option<String>, pub dir: Option<PathBuf>,
    pub agent: Option<Engine>, pub model: Option<String>,
}
impl SpawnArgs {
    /// Trim every string; empty strings become None (message stays, trimmed); dir kept as given.
    pub fn normalized(&self) -> SpawnArgs;
    /// Lowercase hex sha256 of `serde_json::to_vec(&self.normalized())` (struct field order is stable).
    pub fn args_hash(&self) -> String;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub schema_version: u32, pub request_id: String, pub parent_session: String,
    pub created_at: agent_manager::clock::Epoch, pub args: SpawnArgs,
}

// receipt.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState { Claimed, Staged, Launching, Ready, NeedsAttention, Failed, OutcomeUnknown }
impl ReceiptState { pub fn is_final(self) -> bool /* Ready|NeedsAttention|Failed|OutcomeUnknown */; }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode { NotInASession, InvalidArgument, RequestConflict, RequestUnwritable, ParentNotFound,
    NestedSpawnRefused, ChildLimitReached, DirNotFound, MessageTooLong, InvalidRequest,
    RegistryUnreadable, LaunchFailed, ReadinessUnknown }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnError { pub code: ErrorCode, pub message: String }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextActionKind { Wait, Attach }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextAction { pub kind: NextActionKind, pub argv: Vec<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptSession { pub id: String, pub title: Option<String>, pub display_name: Option<String>,
    pub root: PathBuf, pub agent: Engine, pub model: Option<String>, pub spawned_by: String,
    pub tmux_session: String, pub state_dir: PathBuf }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnReceipt { pub schema_version: u32, pub request_id: String, pub state: ReceiptState,
    pub claimed_by: Option<String>, pub session: Option<ReceiptSession>,
    pub launch_state: Option<LaunchState>, pub error: Option<SpawnError>,
    pub next_action: Option<NextAction>, pub updated_at: agent_manager::clock::Epoch }

// files.rs
pub fn requests_dir(state_dir: &Path) -> PathBuf;               // state_dir/spawn-requests
pub fn request_path(dir: &Path, id: &str) -> PathBuf;           // dir/<id>.request.json
pub fn receipt_path(dir: &Path, id: &str) -> PathBuf;           // dir/<id>.receipt.json
pub fn is_valid_request_id(s: &str) -> bool;                    // ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$
pub enum Publish { Published, AlreadyPresent(Box<SpawnRequest>) }
/// create_dir_all(dir) + chmod 0700; write `<dir>/.<id>.<pid>.tmp` (0600); hard_link to request_path;
/// remove temp; on EEXIST read and parse the existing file and return AlreadyPresent.
pub fn publish_request(dir: &Path, req: &SpawnRequest) -> anyhow::Result<Publish>;
pub enum ScanEntry {
    Valid(Box<SpawnRequest>),
    Invalid { request_id: String, reason: String },   // valid UUID name, bad content → broker writes invalid_request
    Skipped { name: String, reason: String },          // bad name → log only
}
/// Every `*.request.json` in dir, sorted by name. Uses symlink_metadata (no follow): non-regular → Invalid;
/// len > MAX_REQUEST_BYTES → Invalid; parse error or schema_version != 1 → Invalid;
/// parent_session != owner_session or request_id != file stem → Invalid. Temp files and receipts are ignored.
pub fn scan_requests(dir: &Path, owner_session: &str) -> Vec<ScanEntry>;
pub fn read_receipt(dir: &Path, id: &str) -> anyhow::Result<Option<SpawnReceipt>>;
pub fn write_receipt(dir: &Path, receipt: &SpawnReceipt) -> anyhow::Result<()>; // state::write_json_atomic
pub fn remove_pair(dir: &Path, id: &str) -> anyhow::Result<()>;                  // NotFound is Ok
```

- [ ] **Step 1: Failing tests.**
  - `request.rs`:
    - `normalization_trims_and_drops_empty_optionals`;
    - `hash_is_stable_across_whitespace_only_differences`;
    - `hash_differs_when_any_field_differs`.
  - `receipt.rs`:
    - `final_states_are_exactly_ready_needs_attention_failed_outcome_unknown`;
    - `receipt_json_uses_snake_case_and_never_contains_a_message_field` (serialize a receipt, assert no `"message"` key except inside `error`).
  - `files.rs` (tempdir):
    - `publish_creates_0700_dir_and_final_file_without_temp_leftovers`;
    - `second_publish_of_the_same_id_returns_already_present_with_the_first_content`;
    - `publish_never_overwrites_under_a_race` (two threads publish different args under one id; exactly one Published; the file equals the winner);
    - `scan_skips_bad_names_and_ignores_receipts_and_temps`;
    - `scan_marks_symlink_oversize_bad_json_wrong_parent_and_stem_mismatch_invalid`;
    - `receipt_round_trip_and_remove_pair_is_idempotent`.
- [ ] **Step 2:** `cargo test --lib spawn` fails.
- [ ] **Step 3:** Implement using `sha2::Sha256`, `std::os::unix::fs::{PermissionsExt, OpenOptionsExt}`, and `std::fs::hard_link`.
- [ ] **Step 4:** The tests pass, clippy is clean, and `cargo test` is green.
- [ ] **Step 5:** Commit: `feat(spawn): request/receipt protocol and hardened file io`.

### Task 4: `pmtui spawn` command

**Files:**
- Create:
  - `src/bin/pmtui/spawn_cli.rs`
  - `src/bin/pmtui/tests/spawn_cli.rs` (register it)
- Modify: `src/bin/pmtui/main.rs:150-200` (parse into an action enum), `main()`, `print_help`.

**Interfaces:**
- Consumes: Task 3 (`spawn::*`), `job_engine::mint_uuid_v4`, `clock::{Clock, SystemClock, Epoch}`, `registry::Engine`.
- Produces:
```rust
enum Action { Dashboard(Args), Spawn(spawn_cli::SpawnCli), Help, SpawnHelp }
fn parse_action(argv: &[String]) -> std::result::Result<Action, spawn_cli::UsageError>;
// spawn_cli.rs
pub(crate) struct SpawnCli { pub args: SpawnArgs, pub request_id: Option<String>, pub wait_s: u64, pub json: bool }
pub(crate) struct UsageError { pub message: String, pub json: bool }
pub(crate) enum CliState { Receipt(ReceiptState), Queued, InProgress, UsageError, Failed /* command-side error */ }
pub(crate) struct CliOutput { pub state: CliState, pub request_id: Option<String>, pub receipt: Option<SpawnReceipt>, pub error: Option<SpawnError> }
pub(crate) trait Waiter { fn now_ms(&mut self) -> u128; fn sleep_ms(&mut self, ms: u64); }
pub(crate) fn run_spawn(cli: &SpawnCli, env: &dyn Fn(&str) -> Option<String>, clock: &dyn Clock, waiter: &mut dyn Waiter) -> CliOutput;
pub(crate) fn exit_code(out: &CliOutput) -> i32;       // 0 ready, 3 queued/in_progress/needs_attention, 1 failed/outcome_unknown/command-failed, 2 usage
pub(crate) fn render_json(out: &CliOutput) -> String;  // a stable JSON object: {"schema_version":1,"request_id":…,"state":…,"receipt":…,"error":…}
pub(crate) fn render_plain(out: &CliOutput) -> String; // `spawned <id> "<title>" (<agent>, from <parent>): ready` or `spawn <request_id>: queued — …`
```
- Behavior:
  - flags per the spec: `--message` required; `--title`, `--name`, `--dir`, `--agent claude|codex`, `--model`, `--request-id` (must pass `is_valid_request_id`), `--wait` (0..=120, default 20), `--json`;
  - with `--json` anywhere in argv, usage errors print JSON with `state:"usage_error"` and exit 2;
  - `PMTUI_SESSION` / `PMTUI_STATE_DIR` missing → `Failed` with `not_in_a_session`, exit 1;
  - a title longer than `TITLE_MAX_CHARS` or containing control bytes → `invalid_argument`;
  - a relative `--dir` is made absolute against the current directory; do not canonicalize (the broker resolves it);
  - publish: `AlreadyPresent` with an equal `(parent_session, request_id, args_hash)` → wait; unequal → `request_conflict`;
  - the wait loop polls `read_receipt` every 250 ms until the receipt is final or the deadline passes;
  - at the deadline: no receipt → `Queued`; a non-final receipt → `InProgress`;
  - never touches the registry, tmux or the singleton;
  - `main()` dispatches `Action::Spawn` before `ratatui::init`/`start_app`, prints, and calls `std::process::exit(exit_code)`.

- [ ] **Step 1: Failing tests** in `tests/spawn_cli.rs`, using a fake env closure, a fake `Waiter` that advances time without sleeping, and a tempdir state dir:
  - `parse_rejects_missing_message_bad_uuid_unknown_agent_and_out_of_range_wait`;
  - `json_usage_error_prints_the_envelope_with_exit_2`;
  - `missing_session_env_is_not_in_a_session_exit_1`;
  - `publishes_then_times_out_as_queued_exit_3_without_touching_home` (env HOME → a nonexistent path; assert no file outside the state dir);
  - `returns_the_final_receipt_written_during_the_wait_exit_0_for_ready` (the fake waiter writes a Ready receipt on its second sleep);
  - `a_non_final_receipt_at_deadline_is_in_progress`;
  - `identical_replay_waits_and_different_args_is_request_conflict`;
  - `plain_output_matches_the_spec_line`;
  - `generated_request_id_is_a_valid_uuid_and_is_printed`.
- [ ] **Step 2:** The tests fail.
- [ ] **Step 3:** Implement, keeping `main.rs` changes to the action enum and dispatch.
- [ ] **Step 4:** Existing `main.rs` arg tests still pass (the Dashboard defaults are unchanged).
- [ ] **Step 5:** In `docs/SPEC.md` and `README.md`, document the command, flags, exit codes and env. Commit: `feat(pmtui): spawn subcommand publishes a request and waits for its receipt`.

### Task 5: Dashboard broker

**Files:**
- Create:
  - `src/bin/pmtui/app/spawning.rs`
  - `src/bin/pmtui/tests/spawn_broker.rs` (register it)
- Modify:
  - `src/bin/pmtui/app/mod.rs` (App fields `spawn_jobs: Vec<SpawnJob>`, `spawn_recovered: bool`)
  - `src/bin/pmtui/app/refresh.rs` (discovery after the registry load)
  - `src/bin/pmtui/main.rs:423` (`finish_frame` calls `app.step_spawns()` every frame)
  - `src/bin/pmtui/app/create.rs` (expose `pub(crate)` helpers the broker reuses without changing New)
  - `src/bin/pmtui/app/forking.rs` (make the row-and-dir discard reusable: extract `discard_staged_row(id, request_or_fork_guard) -> Result<(), String>`)

**Interfaces:**
- Consumes: Task 1 (`ManagedEnv`, `LaunchOutcome`, `LaunchError`); Task 2 (`LaunchRecord`, `LaunchState`, `SpawnOutcome`, `MAX_CHILDREN_PER_PARENT`, `children_of`, `is_staged_spawn`); Task 3 (all of `spawn`); `seed::{reserve_unique_id, seed_agent_loop, sanitize_id, project_id_base}`; `create::{with_initial_prompt}`; `worker::{build_chat_create, build_codex_fresh_chat}`; `tmux::{session_name, LAUNCH_COMMAND_MAX_BYTES, launch_command}`; `tmux::dialog::classify_dialog`; `lease::try_acquire`.
- Produces:
```rust
pub(crate) struct SpawnJob { pub parent: String, pub dir: PathBuf, pub request: SpawnRequest, pub phase: SpawnPhase }
pub(crate) enum SpawnPhase {
    Claimed,
    Staged { child: String },
    Readiness { child: String, first_ok_ms: Option<u128>, deadline_ms: u128 },
    Done,
}
impl App {
    /// Called from refresh(): scan each registry row's requests dir, enqueue new Valid entries (≤16 unfinished per parent),
    /// write invalid_request receipts for Invalid, eprintln-log Skipped, and apply retention (final > RETENTION_SECS).
    pub(crate) fn discover_spawn_requests(&mut self, reg: &Registry);
    /// Advance every job by at most one phase. Never sleeps. The first call runs recovery.
    pub(crate) fn step_spawns(&mut self);
}
```
- State machine (one phase per `step_spawns`, time from an injectable `self.clock` / monotonic ms helper already used by App, or add `now_ms` to App behind the existing clock seam):
  1. **Pre-check:** find the row with `spawned_by == parent && launch.request_id == id`.
     - hash mismatch → `failed` with `request_conflict`;
     - `launch.outcome` final → rewrite the receipt from the row, Done;
     - unfinished → go to recovery.
  2. **Claim:** write a `claimed` receipt with `claimed_by = "pid:<pid>"`.
  3. **Validate:** each failure writes a `failed` receipt and ends at Done.
     - The parent exists → else `parent_not_found`.
     - `parent.spawned_by.is_none()` → else `nested_spawn_refused`.
     - `children_of(parent) < 5` → else `child_limit_reached`.
     - Resolve the defaults: `dir` = args.dir or parent.root, canonicalized (must exist) → else `dir_not_found`; agent = args.agent or parent.engine or Claude; model = args.model, or parent.worker_model when the agent equals the parent engine.
     - Title = args.title or `intent_title(message)` cut to 120 chars; name via `normalize_display_name` → else `invalid_argument`.
     - Exact launch bytes (same computation as `initial_launch_bytes`, with the child's id and paths) ≤ `LAUNCH_COMMAND_MAX_BYTES` → else `message_too_long`.
  4. **Stage:**
     - `reserve_unique_id`, `seed_agent_loop(tier = Standard, brief = "")`;
     - `Registry::update` push `ProjectEntry { enabled:false, spawned_by:Some(parent), task_title, display_name, initial_prompt:Some(message), engine, worker_model, launch: Some(LaunchRecord{request_id, args_hash, state:Pending, outcome:None}), .. }`;
     - write a `staged` receipt.
  5. **Launch:**
     - `Registry::update` sets `launch.state = Attempted`; write a `launching` receipt;
     - `try_acquire(driver.lock)`; if held by pmd, stay in Staged and retry next step;
     - build argv exactly as `start_undriven_session` does for a fresh session: Claude mints a cid with `build_chat_create` and seeds `conversation_id` after start; Codex uses `build_codex_fresh_chat`; wrap with `with_initial_prompt`;
     - `launch_interactive(&session, &dir, &argv, &ManagedEnv{…})`.
     - Outcomes:
       - `Ok(Started)` → state `Started`, `enabled = true`, Claude cid seeded, go to Readiness;
       - `Err(e) if e.proven_not_started()` → persist `FailedBeforeStart`, revalidate, `discard_staged_row`, `failed` receipt with `launch_failed`;
       - `Ok(AlreadyAlive)` or any other `Err` → keep `Attempted`, set `enabled = true`, `outcome = OutcomeUnknown`, receipt `outcome_unknown` with `next_action` Attach: `["tmux","-L",<socket>,"attach-session","-t","=<session>"]`, using the canonical tmux path that `TmuxDriver` already resolved.
  6. **Readiness:** each step, check `is_alive` and `pane_dead` and `classify_dialog(capture_tail(session, 40))`.
     - Two OK observations at least 100 ms apart → `ready`.
     - Dialog → `needs_attention` with Attach.
     - Probe error or 3 s deadline → `outcome_unknown` with `readiness_unknown`.
     - Record `launch.outcome` on the row. Status line `spawned <child> from <parent>`.
  7. **Recovery** (first `step_spawns` after start): for every `claimed`/`staged`/`launching` receipt found by discovery, apply the spec's recovery table using the row's `launch.state` and `is_alive`. `attempted` with an absent terminal → `outcome_unknown` and enable; NEVER relaunch.

- [ ] **Step 1: Failing tests** in `tests/spawn_broker.rs` (FakeDriver App fixture from `tests/mod.rs`, tempdir parent root, requests published with `spawn::publish_request`):
  - `a_valid_request_stages_a_disabled_row_then_launches_once_and_ends_ready`: step until Done; assert exactly one `launched()` entry whose argv ends with the Message; the row is enabled with `spawned_by`, `task_title` and `launch.state=Started`, `outcome=Ready`; the receipt is `ready`; env has `PMTUI_SESSION=<child>`;
  - `validation_failures_write_failed_receipts_and_stage_nothing` (table: missing parent, nested parent, sixth child, missing dir, bad name, over-budget Message);
  - `invalid_and_skipped_entries_never_stage` (symlink, oversize, wrong parent);
  - `proven_pre_start_failure_removes_the_row_and_reserved_dir` (`arm_launch_error(NotOnPath)`);
  - `ambiguous_launch_keeps_an_enabled_row_as_outcome_unknown_and_never_relaunches` (`NewSessionFailed`, `ExitedAfterStart`, `AlreadyAlive`);
  - `recovery_from_attempted_without_terminal_is_outcome_unknown_without_a_second_launch`;
  - `recovery_from_pending_launches_exactly_once`;
  - `recovery_from_failed_before_start_discards`;
  - `replay_after_receipt_cleanup_rebuilds_the_receipt_from_the_row`;
  - `a_matching_row_with_a_different_hash_is_request_conflict`;
  - `readiness_spans_steps_and_step_spawns_never_sleeps` (a fake clock advances between calls; assert ready only after ≥100 ms apart; FakeDriver has no sleep);
  - `trust_dialog_is_needs_attention_with_attach_argv`;
  - `pmd_holding_driver_lock_defers_launch_to_a_later_step`;
  - `sixteen_unfinished_requests_per_parent_are_processed_and_the_rest_wait`;
  - `retention_removes_old_final_pairs_only`.
- [ ] **Step 2:** The tests fail.
- [ ] **Step 3:** Implement `spawning.rs`. Keep each phase a small method (`claim`, `validate`, `stage`, `launch`, `check_readiness`, `recover`). Extract, rather than copy, any code shared with New/fork.
- [ ] **Step 4:** All pmtui tests pass. Interactive New and fork tests pass unchanged.
- [ ] **Step 5:** In `docs/SPEC.md`, document the broker. In `AGENTS.md` Invariants, add: "An agent requests a session only through `spawn-requests/`; the dashboard is the only process that turns a request into a registry row." Commit: `feat(pmtui): dashboard broker turns spawn requests into Standard child sessions`.

### Task 6: Lineage and staged rendering

**Files:**
- Modify:
  - `src/view/mod.rs:64-80` (`ProjectView` gains `spawned_by: Option<String>`, `spawned_by_label: Option<String>`, `spawn_staged: bool`)
  - `src/bin/pmtui/app/refresh.rs` (fill them; `spawned_by_label` = the parent row's display label, else the raw id)
  - `src/bin/pmtui/render/board.rs:222-236,330-340`
  - `src/bin/pmtui/render/preview/head.rs:220-232`
  - `src/bin/pmtui/render/rows.rs` (status label)
- Test: extend `src/bin/pmtui/tests/board_cards.rs`, `tests/preview.rs` and `tests/rows.rs`.

**Interfaces:**
- Consumes: Task 2 fields.
- Produces the view fields above. No new functions are required outside render.

- [ ] **Step 1: Failing tests:**
  - `a_spawned_card_shows_from_parent_label_in_its_context_line`;
  - `fork_lineage_wins_over_spawned_by_on_the_card`;
  - `a_deleted_parent_shows_the_raw_id`;
  - `preview_head_has_a_spawned_fact_line_from_parent`;
  - `a_staged_row_reads_starting_in_rows_and_board`;
  - `session_row_head_width_is_unchanged_for_spawned_rows` (TestBackend at the 44-col floor).
- [ ] **Step 2:** The tests fail.
- [ ] **Step 3: Implement.**
  - Board context: `forked_from` keeps `fork:<parent>` / `fork of …`; otherwise, when `spawned_by` is set, show `from <label>` in the same slot and style.
  - Preview: a `preview_fact_line("spawned", format!("from {label}"))` after the fork line.
  - Rows: the status label `starting…` where `paused` renders when `spawn_staged`.
- [ ] **Step 4:** The tests pass. Render the dashboard with `/pmtui-ui-testing` on a scratch registry seeded with a parent, a spawned child and a staged child at 100x28 and 120x32, and check the text.
- [ ] **Step 5:** Update the SPEC dashboard section. Commit: `feat(pmtui): show spawn lineage and staged rows`.

### Task 7: Spawn skill, shared installer, doctor, docs

**Files:**
- Create:
  - `skills/pmtui-spawn/SKILL.md`
  - the repo's own `.agents/skills/pmtui-spawn/SKILL.md` and `.claude/skills/pmtui-spawn` symlink, only if `tests/repo_contract_test.sh` requires shipped skills to be mirrored (check it; follow what `agent-manager-worker` does)
- Modify:
  - `src/skills.rs` (`SPAWN_SKILL_MD`, `SPAWN_SKILL_NAME`, rel-path consts, `pub fn install_spawn_skill(paths: &ProjectPaths, engine: Engine) -> anyhow::Result<()>`)
  - `src/state/paths.rs` (`canonical_spawn_skill_file`)
  - pmtui launch sites (call the installer before `launch_interactive` when `pmtui_bin` is Some): `app/create.rs` `start_undriven_session`, `session.rs` chat launches, `app/forking.rs`
  - `src/job_engine/session.rs` (install next to `install_worker_skill` when `self.pmtui_bin.is_some()`)
  - `src/doctor/checks.rs:~298` (a spawn-skill check mirroring the worker check)
  - `docs/SPEC.md`, `README.md`
- Test: `src/skills.rs` existing inline test style for body invariants; `src/doctor/tests/…` for the check; `src/bin/pmtui/tests/create.rs` asserting the file exists after a Standard create.

**Interfaces:**
- Consumes: Task 1 `ManagedEnv.pmtui_bin` / `pmtui_bin_for_current_process`.
- Produces: `skills::install_spawn_skill`, `skills::SPAWN_SKILL_MD`.

The skill body is procedural and imperative (see the memory rule on procedural prompts). It contains exactly the spec's "Agent Skill" steps 1–5, plus a copy-paste command block:

```bash
id=$(cat /proc/sys/kernel/random/uuid)
"$PMTUI_BIN" spawn --request-id "$id" --title "<short title>" --message "<Target / Change / Constraints / Ownership / Acceptance>" --json
```

- [ ] **Step 1: Failing tests:**
  - `spawn_skill_body_names_the_command_request_id_and_every_state`;
  - `standard_create_installs_the_spawn_skill_and_claude_link`;
  - `no_pmtui_bin_means_no_spawn_skill`;
  - `doctor_reports_a_stale_spawn_skill`.
- [ ] **Step 2:** The tests fail.
- [ ] **Step 3: Implement.** Reuse `write_text_atomic` and the Claude link helper pattern (`ensure_claude_worker_skill_link` generalized to take a skill name). Never touch the user's `AGENTS.md`.
- [ ] **Step 4:** The tests pass, and so do `bash tests/repo_contract_test.sh` and `bash tests/install_sh_test.sh`.
- [ ] **Step 5:** Commit: `feat(skills): ship the pmtui-spawn skill to managed sessions`.

### Task 8: Real-tmux acceptance and final verification

**Files:**
- Create: `tests/integration/spawn.rs` (register it in `tests/integration/main.rs`; reuse `pmtui_fixture.rs`)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the acceptance tests** from the spec's "Real tmux" list. Each uses unique per-pid sockets and scratch registries (the `pmtui_fixture` pattern), and the stub agent is a shell script inside the real `pm-` terminal:
  - `spawn_from_a_live_session_creates_one_child_the_dashboard_shows` (poll the capture for the title and `from <parent>` in Task view; assert exactly one new `pm-` session whose pane received the Message);
  - `spawn_without_a_dashboard_is_queued_then_created_when_one_starts`;
  - `spawn_under_codex_workspace_write_sandbox_publishes_its_request` (version-aware; skip visibly only if codex or its sandbox is missing);
  - `a_child_cannot_spawn`;
  - `killing_the_dashboard_mid_request_recovers_without_a_second_terminal`.
- [ ] **Step 2: Run the full gates** from AGENTS.md, serially, logging to `/tmp/am-spawn-*.log` and grepping for `FAILED|panicked|error`: `cargo build --locked`, fmt, clippy, `cargo test`, `cargo audit`, `bash tests/coverage_test.sh`, `bash tests/install_sh_test.sh`, `bash tests/repo_contract_test.sh`, then `cargo test --test integration -- --ignored --test-threads=1` **once**. On a failure, rerun only that test.
- [ ] **Step 3:** Render a headless spawn in a real dashboard with `/pmtui-ui-testing` and capture it.
- [ ] **Step 4:** Commit: `test(integration): real-tmux acceptance for pmtui spawn`.
