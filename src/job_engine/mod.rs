//! `JobScheduler` — drives ONE `Mode::AgentLoop` session as a **persistent
//! interactive `claude`/`codex`** (the "agent-deck" model; design
//! `docs/superpowers/specs/2026-08-15-persistent-agent-loop-session-design.md`,
//! Slice 1). The agent is born once in the project's unified tmux terminal
//! ([`crate::tmux::session_name`]) and lives for the session's lifetime; the
//! daemon is only its **metronome + observer**.
//!
//! The heartbeat is no longer "spawn a process": on the daemon's sweep, when the
//! cadence is due AND the agent is idle-at-prompt AND no human is attached/chatting,
//! the harness types a **nudge** into the live REPL via `tmux send-keys`
//! ([`Driver::send_keys`]) — the agent never dies between wakes. There is
//! deliberately **no machine `Done`** — done is human-only.
//!
//! Two decoupled clocks:
//!   - the daemon's 500ms **liveness** sweep drives [`JobScheduler::tick`], zero token
//!     cost;
//!   - the ledger's **cadence** ([`JobRun::Monitoring`]`{until}`, reinterpreted as the
//!     nudge interval) gates when the next nudge is due.
//!
//! Escalation-by-scope is transport-agnostic and preserved: the bounded-runtime
//! budgets (`max_wakes`/`max_wall_clock_s`) and [`JobScheduler::park_stuck`] still
//! park [`JobRun::Blocked`], and [`JobScheduler::on_blocked`] resumes on a fresh human
//! answer — now by nudging the live session rather than spawning. Crash recovery
//! rebuilds run state from the per-session `driver.json` + ledger; the persistent
//! session is re-launched (resuming the SAME `conversation_id`), and any stray
//! ephemeral `pmj-<seq>` worker from a pre-upgrade ledger is terminated.
//!
//! Slice-1 scope: spawn-once + nudge-when-idle + retire the ephemeral `pmj-` path.
//! Marker-file completion/blocked detection (report parsing → the tier oracle) and
//! robust busy detection land in later slices.
//!
//! The engine is split by concern, and the split is the map: `session` is the persistent
//! agent's own lifecycle (launch it if it is not alive, and decide which conversation it
//! runs on), `drive` is what one tick may do to a live pane, `nudge` is when a heartbeat
//! is due and what it types, `marker` is what the agent's own report may do to the ledger,
//! `supervisor` + `fallback` are the consult and everything that happens when it cannot
//! answer, `stops` is parking on a human and coming back, `budget` holds the two
//! bounded-runtime budgets that stop the loop driving an unattended session forever, and
//! this file keeps the parts every one of them needs: the struct, the tick router, and the
//! ONE ledger writer.

use std::cell::Cell;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::advise;
use crate::clock::{Clock, Epoch};
use crate::job::{self, AgentLoopState, AutopilotEventKind, JobRun};
use crate::registry::Engine;
use crate::state::{self, Config, DriverState, ExitReason, ProjectPaths};
use crate::tmux::{self, Driver};

use self::fallback::AdviseHealth;
use self::supervisor::{AdviseInFlight, SUPERVISOR_ENV};

mod budget;
mod dialog_advice;
mod drive;
mod fallback;
mod marker;
mod nudge;
mod progress;
mod session;
mod stops;
mod supervisor;

// Every item the rest of the crate reaches as `agent_manager::job_engine::<Item>` is
// re-exported here, so splitting the engine into submodules moved no public path:
// src/daemon/, src/bin/pmtui/ and tests/integration/ name these exactly as before.
pub use self::budget::{DEFAULT_MAX_WAKES, DEFAULT_MAX_WALL_CLOCK_S};
// The recheck/confirmation interval, re-exported for the VIEW: `agent_loop_next_action`
// floors the monitoring countdown at this value so the row never shows a `check in 0s`
// that `drive` will only re-park (user: *"when the timer countdown to 0s on autopilot, it
// doesn't send immediately. it resets to 5 or 10s"*). The const lives in `drive` because
// that is where the re-park happens; the view is the only out-of-engine consumer.
pub(crate) use self::drive::BUSY_RECHECK_S;
pub use self::marker::parse_report;
#[cfg(test)]
pub(crate) use self::nudge::loop_nudge_prompt;
pub use self::nudge::{
    CADENCE_MAX_S, CADENCE_MIN_S, DEFAULT_CADENCE_S, LoopNudgePromptInput, STALE_PLAN_NUDGE_STREAK,
    SinceLastWake, human_cadence, loop_nudge_prompt_for_engine,
};
pub use self::session::{claude_conversation_exists, mint_uuid_v4};
// The Milestone-C recent-history projection, re-exported so the decider-benchmark integration
// test (`tests/integration/decider_bench.rs`) can drive arm B through the REAL projection path
// rather than a hand-built approximation.
pub use self::supervisor::project_situation;

/// `raw.jsonl` size ceiling (~2 MB) before it rotates to a single `.1` generation
/// (~4 MB/session total). Machine full-fidelity, so the ceiling is generous.
const RAW_JSONL_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// `decisions.md` size ceiling before it rotates to a single `.1` generation. The human
/// file is sparse (Escalated/Stalled only), so this is small — it exists so even the sparse
/// file cannot grow unbounded over a multi-day session.
const DECISIONS_MD_MAX_BYTES: u64 = 256 * 1024;

/// What one [`JobScheduler::tick`] did — the daemon routes escalations / stuck
/// notifications from this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobTick {
    /// No per-session ledger yet: creating it is pmtui's job at intake (S4); the
    /// engine never bootstraps a session. This is the idle/no-op tick.
    WaitingForIntake,
    /// Parked between nudges (or on cold-start grace / a busy-pane recheck / a
    /// human-attached defer); the next nudge is due at `until` (the cadence timer).
    Monitoring { until: Epoch },
    /// Parked on the human; carries the escalating open-stop ids.
    Escalated(Vec<String>),
    /// A bounded budget was hit (too many nudges / too long unattended) — surfaced
    /// once, then parked as a `Blocked` stuck stop. Carries the reason.
    Stuck(String),
}

/// Drives one `Mode::AgentLoop` session over its per-session ledger + `driver.json`.
pub struct JobScheduler {
    /// The registry id of the session (raw; used for tmux naming + diagnostics).
    project_id: String,
    /// The REAL project root = the worker's cwd (shared across sessions in a folder).
    work_dir: PathBuf,
    /// The per-session state subtree (`.project-state/sessions/<id>/`).
    paths: ProjectPaths,
    /// The engine (`claude`/`codex`) the persistent session runs.
    engine: Engine,
    /// The per-session worker model passed to the engine as `--model`/`-m` at launch
    /// (`ProjectEntry.worker_model`, read from the pmtui-owned registry). `None` ⇒ no flag,
    /// so the engine uses its own default.
    worker_model: Option<String>,
    /// In-memory run state (the drive guard + crash-recovery mirror); kept in
    /// lock-step with the ledger's `run` field.
    run: JobRun,
    /// Bounded-runtime budget: nudges since the session was last touched by a human
    /// (a `Blocked` answer resets it). At `max_wakes` the loop escalates instead of
    /// nudging forever. In-memory (a daemon restart starts a fresh window, like the
    /// notify-dedup set); `0` disables the budget.
    wakes: u64,
    max_wakes: u64,
    /// Bounded wall-clock budget (seconds; `0` disables). Escalates — never
    /// terminates — once `now - window_start >= max_wall_clock_s`.
    max_wall_clock_s: u64,
    /// When the current wall-clock window opened — the first tick that DROVE the live
    /// session after a reset ([`JobScheduler::budget_backstop`]) — or `None` before any
    /// such tick / after a human answer. Deliberately NOT "the first successful nudge":
    /// tying it to a send made the budget vacuous for a session that never nudges (an agent
    /// that keeps self-scheduling `monitoring` naps ran unbounded). In-memory, like
    /// `wakes`: a daemon restart starts a fresh window (consistent with the notify-dedup
    /// set), which is fail-safe — it can only ever DELAY an escalation.
    window_start: Option<Epoch>,
    /// Stall backstop window: when the pane was FIRST seen continuously Busy with no
    /// intervening progress, or `None` when not currently stalling. Opened on the first
    /// Busy observation (see [`JobScheduler::busy_recheck`]) and RESET on every sign of
    /// progress / life — an idle nudge, a valid marker bump, a human attaching, a
    /// cold-start launch, or a human-answer resume — so a healthy agent that
    /// periodically signals never trips [`DEFAULT_STALL_BUSY_S`]. In-memory only,
    /// mirroring `window_start`/`last_marker_mtime`: a lost value after a daemon restart
    /// just restarts the timer (a fresh Busy observation reopens it), which is fail-safe
    /// (never escalates early).
    busy_since: Option<Epoch>,
    /// Normalized transcript fingerprint from the last positively captured Busy pane. A change is
    /// observable progress and rebases `busy_since`; volatile spinner/footer counters are excluded.
    /// In-memory only, so a daemon restart conservatively starts a fresh inactivity window.
    last_busy_progress_fingerprint: Option<u64>,
    /// Confirmation gate counter: how many CONSECUTIVE `PaneActivity::Idle` observations
    /// the heartbeat has seen since the last nudge / Busy observation / human touch. The
    /// `drive` Idle arm only nudges once this reaches
    /// [`IDLE_CONFIRMATIONS_REQUIRED`]; below that it re-parks a short
    /// [`BUSY_RECHECK_S`] recheck and consumes nothing, exactly like the Busy arm. RESET
    /// to 0 on ANY Busy observation (so the two Idles must be *consecutive*), on a
    /// delivered nudge, and wherever `busy_since` is reset for a "human is here / fresh
    /// session" reason (the human-present defers, the JustLaunched cold start, the
    /// answer-resume path) — all of which invalidate earlier observations of the pane.
    /// In-memory only, mirroring `busy_since`/`window_start`/`wakes`: nothing is added to
    /// the persisted ledger, and a lost value after a daemon restart just costs one extra
    /// [`BUSY_RECHECK_S`] before the first nudge (fail-safe — it can only ever DELAY a
    /// nudge, never type one early).
    idle_confirmations: u32,
    /// The transcript fingerprint ([`tmux::idle_fingerprint`]) of the LAST Idle
    /// observation the confirmation gate counted, or `None` when the gate is unarmed. The
    /// gate advances `idle_confirmations` only when a fresh Idle capture's fingerprint
    /// MATCHES this — i.e. the transcript above the prompt did not change between two
    /// observations [`BUSY_RECHECK_S`] apart. This is the real fix for a working claude
    /// getting nudged: `classify_pane` returns Idle during answer streaming (no busy marker
    /// is on screen then), but a streaming pane's transcript GROWS between captures, so a
    /// changed fingerprint re-arms the gate instead of nudging — independent of any spinner
    /// string. Reset alongside `idle_confirmations` by [`JobScheduler::reset_idle_gate`].
    /// In-memory only, mirroring `idle_confirmations`: a lost value after a daemon restart
    /// just re-arms the gate (one extra [`BUSY_RECHECK_S`] before the first nudge), which
    /// is the fail-safe direction.
    last_idle_fingerprint: Option<u64>,
    /// The turn-complete count (bytes in `paths.turn_signal()`) observed at the LAST
    /// delivered nudge, or `None` when no nudge is outstanding since the gate was reset.
    /// The launched engine's turn-end hook (claude `Stop` / codex `notify`, injected by
    /// `worker::build_loop_command`) appends one byte per completed turn; so a count that has
    /// NOT advanced past this baseline means "the agent has not finished the turn I nudged" —
    /// a DEFINITIVE "still mid-turn, do not nudge" signal `classify_pane` cannot give when a
    /// working agent has merely fallen quiet (see [`JobScheduler::turn_in_progress`]). A count
    /// that HAS advanced is not trusted as "idle now" (a human on the same session may have
    /// started another turn) — it defers to the fingerprint gate. Set on a delivered `nudge`
    /// and cleared only
    /// at cold-start relaunch (`ensure_session`) — NOT in `reset_idle_gate`, because it must
    /// survive the `busy_recheck` that fires on every in-progress recheck (it IS the "still
    /// awaiting the turn I nudged" state). In-memory only, mirroring `last_idle_fingerprint`:
    /// a lost value after a daemon restart just re-baselines from the file (fail-safe —
    /// `turn_signal_status` returns `Unknown` until the next nudge, so the fingerprint gate
    /// covers the gap). ABSENT signal file ⇒ the hook never fired ⇒ fingerprint fallback.
    turns_at_nudge: Option<u64>,
    /// The pmtui-owned registry seed (`registry.conversation_id`): the id of the
    /// conversation the human's first interactive chat created. Consulted by
    /// `ensure_session` ONLY when the ledger's `conversation_id` is `None` on the
    /// first launch — the daemon then ADOPTS (resumes) it instead of minting a fresh
    /// one, so the persistent session picks up the conversation the human started.
    /// Refreshed each sweep by [`JobScheduler::set_registry_seed`] (pmtui writes the
    /// seed AFTER the runner was built).
    registry_seed: Option<String>,
    /// Exact file revision last observed for `needs-you.json` (mtime, length, inode). This is the
    /// cheap process-local pre-check; durable replay protection is the ledger's semantic marker
    /// fingerprint. Reading metadata and bytes from one opened file prevents an atomic replacement
    /// from mixing one revision's metadata with another revision's content.
    last_marker_stamp: Option<marker::MarkerStamp>,
    /// Whether the supervisor may be consulted at all (m20, Rule 8). Read ONCE from
    /// `PM_SUPERVISOR` when the scheduler is built, and default ON — see
    /// [`advise::supervisor_enabled`] for the argument. Held as a field rather than
    /// re-read per tick so it can be driven directly by
    /// [`JobScheduler::set_supervisor_enabled`]: mutating process env is `unsafe` in
    /// edition 2024 and unsound under parallel tests, and pmd's env is fixed at launch
    /// anyway, so a field loses nothing.
    advise_enabled: bool,
    /// The supervisor consult spawned and not yet reaped, or `None` (m20). At most ONE
    /// per session at a time: a consult is only started from the auto-flow branch, and
    /// that branch cannot run again until this one resolves (the tick it starts parks a
    /// short recheck and never nudges).
    ///
    /// `Box`ed because a `JobScheduler` is one variant of `daemon::Driven`, and inlining
    /// ~200 bytes that are `None` on virtually every tick made that enum lopsided enough
    /// to trip `clippy::large_enum_variant`. A consult is rare, so one allocation when it
    /// starts is free; keeping the sweep's per-session state small is not.
    advise: Option<Box<AdviseInFlight>>,
    /// A consult that was in flight when the PREVIOUS daemon process died, recovered from
    /// the ledger by [`JobScheduler::restore_from_disk`] (m23 bug 1).
    ///
    /// Deliberately NOT rebuilt as an `advise` — its nonce and option list are gone, so
    /// nothing could validate a reply against them, and re-adopting the reply would mean
    /// trusting bytes the harness can no longer prove are an answer to the question it
    /// asked. All the restarted daemon can honestly do is surface the original audited
    /// question to the human. Consumed once by
    /// [`JobScheduler::advise_step`]; dropped by [`JobScheduler::abandon_advice`] when the
    /// question stops being live.
    advise_orphaned: Option<job::ParkedAdvice>,
    /// Process-local decision-audit sequence. Deterministic preflight skips consume a number
    /// without spawning anything; actual consults also use the number in their `pmsup-` session
    /// and done-signal/log paths, so a stale consult's exit code cannot be read as a fresh one's.
    /// Resumed from the bounded completed audit and `advice_inflight` after restart. Numbers older
    /// than retained history may eventually repeat, which is harmless because no matching files
    /// survive that long.
    advise_seq: u64,
    /// Highest completed decider sequence whose transient files this process has already cleaned.
    ///
    /// The durable audit remains in the ledger; this cursor only prevents every ordinary ledger
    /// save from repeating filesystem probes for the same bounded history.
    advice_cleanup_seq: Cell<u64>,
    /// Whether the `claude` binary the supervisor needs is on `PATH`, probed lazily ONCE
    /// per session (`None` = not probed yet).
    ///
    /// A missing binary does NOT make [`Driver::spawn_step`] fail: the spawn succeeds and
    /// the shell exits 127, which reaches the harness as an ordinary "no result" — so on a
    /// codex-only box every session paid three `Capability` escalations before the latch
    /// noticed. Probing first turns that into one quiet latch and an immediate escalation.
    advise_binary: Option<bool>,
    /// The supervisor's latch + streaks (see [`AdviseHealth`]).
    advise_health: AdviseHealth,
    /// `(pane fingerprint, advice text)` of the last verdict actually delivered — the
    /// anti-ping-pong memory (Rule 7). If a consult proposes byte-identical advice about
    /// a byte-identical pane, the worker did not act on what we already typed, so typing
    /// it again would loop; the harness refuses and escalates instead. In-memory, like
    /// every other counter here: losing it after a restart only costs one extra delivery.
    advise_last: Option<(u64, String)>,
    /// Whether THIS session's worker got its canonical `.agents/skills` copy and any
    /// engine-compatibility link installed. Set on launch and first adoption; read by the nudge to
    /// pick the lean skill-available branch. A write failure takes the compact inline degrade.
    /// In-memory only: a daemon restart re-installs on the first sweep.
    worker_skill_installed: bool,
    /// Override for claude's config dir (the `.claude` directory) used ONLY by the
    /// registry-seed existence probe ([`JobScheduler::seed_conversation_exists`]). `None`
    /// in production (resolved from `CLAUDE_CONFIG_DIR` / `$HOME/.claude` at probe time);
    /// tests point it at an isolated temp dir so the probe never reads the real store. Set
    /// via [`JobScheduler::set_claude_home`].
    claude_home: Option<PathBuf>,
    /// The canonical `pmtui` executable handed to the persistent terminal as `PMTUI_BIN`, or
    /// `None` to omit it. Set once by pmd ([`JobScheduler::set_pmtui_bin`]) from
    /// [`tmux::pmtui_bin_for_current_process`]: the scheduler has no pmtui path of its own.
    pmtui_bin: Option<PathBuf>,
    /// NO-PROGRESS circuit breaker (slice 1): escalate once the working tree is byte-identical
    /// across this many CONSECUTIVE nudges (`0` = disabled). Read ONCE from `PM_NO_PROGRESS`
    /// when the scheduler is built (default [`progress::DEFAULT_NO_PROGRESS_NUDGES`]); tests set
    /// it directly, like `max_wakes`.
    no_progress_threshold: u32,
    /// The [`progress::tree_fingerprint`] observed at the LAST nudge, or `None` when the run is
    /// unarmed / the tree is not observable. Compared against a fresh fingerprint on the next
    /// nudge to tell "the agent produced something" from "it is spinning". In-memory only,
    /// mirroring `busy_since`/`last_idle_fingerprint`: a lost value after a daemon restart just
    /// re-baselines (fail-safe — it can only ever DELAY an escalation, never fire one early).
    last_progress_fp: Option<u64>,
    /// How many consecutive nudges have produced NO change to the working tree. Advanced by
    /// [`JobScheduler::no_progress_backstop`], reset on a tree change (by the pure evaluator) or
    /// a human touch ([`JobScheduler::reset_no_progress`]). In-memory only (fail-safe like
    /// `last_progress_fp`).
    no_progress_streak: u32,
}

impl JobScheduler {
    /// Build a scheduler for one session. `project_id` names the session (tmux +
    /// logs); `work_dir` is the real project root (the worker's cwd); `session_id`
    /// selects the per-session state subtree via [`ProjectPaths::for_session`].
    pub fn new(
        project_id: impl Into<String>,
        work_dir: impl Into<PathBuf>,
        session_id: &str,
        engine: Engine,
        worker_model: Option<String>,
    ) -> Self {
        let work_dir = work_dir.into();
        let paths = ProjectPaths::for_session(&work_dir, session_id);
        let mut s = Self {
            project_id: project_id.into(),
            work_dir,
            paths,
            engine,
            worker_model,
            run: JobRun::Idle,
            wakes: 0,
            max_wakes: DEFAULT_MAX_WAKES,
            max_wall_clock_s: DEFAULT_MAX_WALL_CLOCK_S,
            window_start: None,
            busy_since: None,
            last_busy_progress_fingerprint: None,
            idle_confirmations: 0,
            last_idle_fingerprint: None,
            turns_at_nudge: None,
            registry_seed: None,
            last_marker_stamp: None,
            advise_enabled: advise::supervisor_enabled(
                std::env::var(SUPERVISOR_ENV).ok().as_deref(),
            ),
            advise: None,
            advise_orphaned: None,
            advise_seq: 0,
            advice_cleanup_seq: Cell::new(0),
            advise_binary: None,
            advise_health: AdviseHealth::default(),
            advise_last: None,
            worker_skill_installed: false,
            claude_home: None,
            pmtui_bin: None,
            no_progress_threshold: progress::no_progress_threshold_from_env(
                std::env::var(progress::NO_PROGRESS_ENV).ok().as_deref(),
            ),
            last_progress_fp: None,
            no_progress_streak: 0,
        };
        s.restore_from_disk();
        s
    }

    /// Turn the LLM supervisor (m20) on or off for this session, overriding whatever
    /// `PM_SUPERVISOR` said when the scheduler was built.
    ///
    /// Exists because the env var cannot be driven from a test: `std::env::set_var` is
    /// `unsafe` in edition 2024 and process-global, so a test that flipped it would race
    /// every other test in the binary. Also the natural hook for a future per-session
    /// config field, which is exactly why the kill switch is a var and not a schema change
    /// yet. Turning it OFF is not a latch; policy-eligible decisions escalate because no
    /// independent verdict is available.
    pub fn set_supervisor_enabled(&mut self, enabled: bool) {
        self.advise_enabled = enabled;
    }

    /// Override the cached decider-binary availability result.
    ///
    /// Custom drivers and deterministic harnesses may deliberately execute children with a
    /// different `PATH` from the scheduler process (for example, a tmux wrapper that injects a
    /// stub CLI). Production leaves this unset and uses the one-shot process `PATH` probe.
    pub fn set_decider_binary_available(&mut self, available: bool) {
        self.advise_binary = Some(available);
    }

    /// Whether THIS session's worker got its engine-native project skill written, so the nudge may
    /// take its lean skill-available branch. A write failure takes the compact path-reference
    /// degrade, which still names the fallback file and keeps the non-negotiable floor.
    pub(crate) fn worker_skill_available(&self) -> bool {
        self.worker_skill_installed
    }

    /// Test hook (mirrors [`JobScheduler::set_supervisor_enabled`]): force the flag so a
    /// `FakeDriver` scenario can exercise either nudge branch deterministically.
    pub fn set_worker_skill_available(&mut self, v: bool) {
        self.worker_skill_installed = v;
    }

    /// Point the registry-seed existence probe at a specific claude config dir (the
    /// `.claude` directory) instead of the real `CLAUDE_CONFIG_DIR` / `$HOME/.claude`.
    /// Test/isolation hook only: a real deployment leaves this `None`. Used so a
    /// `FakeDriver`/real-tmux scenario can decide the probe's answer (present the
    /// transcript ⇒ RESUME; a `projects` dir with no transcript ⇒ CREATE; no `projects`
    /// dir ⇒ the optimistic-resume fallback) without touching the machine's real store.
    pub fn set_claude_home(&mut self, home: impl Into<PathBuf>) {
        self.claude_home = Some(home.into());
    }

    /// Set the `pmtui` executable every launch of this session's terminal names as
    /// `PMTUI_BIN` (`None` omits the variable). pmd calls this once per runner with the
    /// `pmtui` beside its own executable.
    pub fn set_pmtui_bin(&mut self, bin: Option<PathBuf>) {
        self.pmtui_bin = bin;
    }

    /// Who this session's terminal belongs to, for the agent inside it: the stable id, the
    /// per-session state directory, and pmtui when pmd found one.
    pub(crate) fn managed_env(&self) -> tmux::ManagedEnv {
        tmux::ManagedEnv {
            session_id: self.project_id.clone(),
            state_dir: self.paths.state_dir(),
            pmtui_bin: self.pmtui_bin.clone(),
        }
    }

    /// Store the latest registry seed (pmtui's first-spawn `registry.conversation_id`).
    /// Called each sweep BEFORE the tick so a seed pmtui writes AFTER the runner was
    /// built still reaches the live scheduler. It only records the seed — it never
    /// touches the de-poison latch (`seed_discarded`) or the verify latch
    /// (`adopted_unverified`), so a discarded seed stays discarded across sweeps.
    pub fn set_registry_seed(&mut self, seed: Option<&str>) {
        self.registry_seed = seed.map(|s| s.to_string());
    }

    /// Rebuild run state from disk after a restart (crash recovery). The seq counter
    /// and failure count resume from the last `driver.json` record so a restart
    /// never reuses a done-signal / tmux name and preserves backoff. An in-flight
    /// worker (`driver.json` still `Running`) is reconstructed as `Running` so the
    /// next tick *observes* it (no double-spawn); otherwise the ledger's persisted
    /// `run` (`Monitoring`/`Blocked`/`Idle`) is restored verbatim — a `Blocked` park
    /// resumes without re-notifying (the daemon's dedup starts fresh for one tick).
    fn restore_from_disk(&mut self) {
        let driver = state::read_json_opt::<DriverState>(&self.paths.driver())
            .ok()
            .flatten();
        let ledger = job::load(&self.paths).ok().flatten();

        // Crash recovery for the persistent model. A `Running` handle in `driver.json`
        // means an EPHEMERAL `pmj-<seq>` worker was in flight when the last daemon died:
        // it is preserved AS `Running` so the FIRST `tick`'s `Running` arm terminates that
        // stray worker (so it cannot keep editing) and re-drives — which re-launches the
        // persistent session resuming the SAME `conversation_id` (kept in the ledger, never
        // lost).
        //
        // THE PANE CHECK IS LOAD-BEARING, and its absence killed the user's running agent
        // on every single `pmd` restart. `ensure_session` records the PERSISTENT session's
        // own handle the same way — `write_driver_running(0, &self.loop_session(), …)` — and
        // never marks it ended while it lives, so a healthy live session's `driver.json`
        // names the unified project terminal. Restored as `Running`, the next tick's
        // `Running` arm then `terminate`d that pane: the harness reaped the very agent it
        // exists to keep alive, and the relaunch that followed lost the live REPL's
        // in-flight turn. So: a `pmj-…` pane still routes to terminate-and-re-drive (that
        // path is real — a pre-upgrade ledger can have one — and must keep working), while
        // the persistent session's own pane falls through to the ledger restore below,
        // where its `Monitoring`/`Blocked` park resumes and `ensure_session` finds it
        // already alive.
        //
        // Otherwise the ledger's persisted run is restored verbatim: `Blocked` resumes
        // without re-notifying (a human still owes an answer; the daemon's dedup starts
        // fresh for one tick), `Monitoring`/`Idle` as-is. A persistent session never itself
        // sets `Running`.
        if let Some(d) = &driver
            && d.exit_reason == ExitReason::Running
            && d.pane != self.loop_session()
        {
            self.run = JobRun::Running {
                seq: d.step_id,
                session: d.pane.clone(),
                deadline: d.deadline,
            };
            return;
        }
        if let Some(l) = &ledger {
            self.run = l.run.clone();
            self.advise_health.latched = l.decider_latched;
            self.advise_health.reason = l.decider_latch_reason.clone();
            self.advise_seq = self
                .advise_seq
                .max(l.decider_runs.iter().map(|run| run.seq).max().unwrap_or(0));
            // A supervisor consult the dead daemon spawned and never honoured. The reply is
            // unrecoverable (see `advise_orphaned`), but the DEBT is: resume the consult
            // counter so a fresh consult cannot inherit this one's done-signal path, and
            // hand the record to `advise_step`, which escalates the original audit.
            if let Some(parked) = l.advice_inflight.clone() {
                self.advise_seq = self.advise_seq.max(parked.seq);
                // Marker debt is escalated with its original audit. Dialog debt first terminates
                // the old consult and clears its audit, then the live screen is re-detected and
                // re-consulted without ever converting the choice into prose.
                self.advise_orphaned = Some(parked);
            }
        }
    }

    /// Advance the session by at most one tick. Never blocks: launching is detached
    /// and a nudge is a non-blocking `send-keys`.
    pub fn tick(&mut self, driver: &dyn Driver, clock: &dyn Clock) -> Result<JobTick> {
        let now = clock.now();
        // Validate the per-session config and thread it into the drive path: Slice 2
        // routes each marker `Blocked` stop through `config.autonomy` (the tier oracle)
        // and gates the malformed-marker stall on `config.stuck_threshold`.
        //
        // A missing per-session config = pmtui intake (S4) hasn't finished writing it yet
        // (a create-time race), mirroring the missing-ledger case below — NOT corrupt
        // state. Return the benign idle tick so the daemon's poison counter
        // (POISON_THRESHOLD) never strikes a brand-new session. A config that EXISTS but
        // is malformed still errors → poison (pmtui writes it atomically tmp+rename, so a
        // present config is always complete — a parse error means real corruption).
        //
        // Rationale for `.try_exists().unwrap_or(false)`: an existence probe that itself
        // errors (e.g. permission) is also "can't drive right now" — fail SAFE toward the
        // benign idle tick, never toward a poison strike.
        if !self.paths.config().try_exists().unwrap_or(false) {
            return Ok(JobTick::WaitingForIntake);
        }
        let config: Config = state::read_json(&self.paths.config())
            .with_context(|| format!("read config for {}", self.project_id))?;
        // pmd creates the ledger on the first driven tick. Human-owned cadence and
        // wake requests arrive through control.json and are folded into that ledger.
        let loaded = job::load(&self.paths)
            .with_context(|| format!("load agent-loop ledger for {}", self.project_id))?;
        let was_missing = loaded.is_none();
        let mut ledger = loaded.unwrap_or_else(|| AgentLoopState::fresh(self.engine, None, now));
        let control = state::read_control(&self.paths)
            .with_context(|| format!("read control for {}", self.project_id))?;
        let mut control_changed = was_missing;
        let mut human_cadence_changed = false;
        if let Some(secs) = control.human_cadence_s
            && (ledger.cadence_s != Some(secs) || !ledger.cadence_pinned)
        {
            ledger.retime(secs, now);
            control_changed = true;
            human_cadence_changed = true;
        }
        if control.wake_generation > ledger.applied_wake_generation {
            ledger.applied_wake_generation = control.wake_generation;
            ledger.nudged_at_seq = None;
            ledger.nudged_at_report_generation = None;
            if matches!(ledger.run, JobRun::Monitoring { .. }) {
                ledger.run = JobRun::Monitoring { until: now };
            }
            ledger.updated_at = now;
            control_changed = true;
        }
        if control_changed {
            self.save_control_ledger(&mut ledger, human_cadence_changed)?;
            self.run = ledger.run.clone();
        }

        match self.run.clone() {
            // Idle / a due Monitoring → the DRIVE path (launch-once + nudge-if-idle).
            JobRun::Idle => self.drive(driver, now, &ledger, &config),
            JobRun::Monitoring { until } => {
                if now >= until {
                    self.drive(driver, now, &ledger, &config)
                } else if self.marker_revision_advanced() {
                    // OQ1 mtime-fast-path: a mid-cadence marker bump (e.g. the agent
                    // just wrote `blocked`) should park/escalate within ONE sweep, not
                    // after the full cadence. Gated by a single `metadata()` stat — when
                    // nothing changed this arm stays O(1) and returns the parked timer
                    // unchanged, so the no-marker path is byte-identical to today. This
                    // path is DISPOSER-ONLY: it never nudges and never shrinks the
                    // cadence (the daemon sweeps every 500ms, so falling into the full
                    // nudge path here would collapse the cadence — regression I1).
                    self.drive_marker_only(driver, now, &ledger, &config, until)
                } else {
                    Ok(JobTick::Monitoring { until })
                }
            }
            // Back-compat: a persistent session NEVER sets `Running`. A `Running` here
            // is an old ephemeral handle (an upgraded ledger). Terminate the stray
            // `pmj-<seq>` worker so it can't keep editing, then re-drive as if a due
            // Monitoring — which re-launches the persistent session resuming the SAME
            // conversation id.
            JobRun::Running { session, .. } => {
                let _ = driver.terminate(&session);
                self.run = JobRun::Monitoring { until: now };
                self.drive(driver, now, &ledger, &config)
            }
            JobRun::Blocked { stop_ids, since } => {
                self.on_blocked(driver, now, &ledger, &stop_ids, since)
            }
        }
    }

    /// The tmux session name for this session's ONE persistent interactive agent.
    fn loop_session(&self) -> String {
        tmux::session_name(&self.project_id, &self.work_dir)
    }

    /// The ONE ledger writer in this engine. Every `job::save` goes through here.
    ///
    /// Its whole job is to stamp `advice_inflight` from the in-memory truth on EVERY write,
    /// which is what makes the m20 supervisor's durability structural instead of a list of
    /// places somebody has to remember. The alternative — a durable placeholder that each
    /// exit must consciously clear — was tried on paper and is a trap: a consult has FIVE
    /// exits, three of them funnel through `park_capability_stop`, and `on_blocked` then
    /// APPENDS the human's answer to whatever is parked in `pending_context`. Miss one exit
    /// and a human who answers "no, don't do that" receives their refusal with a blanket
    /// auto-approval stapled in front of it — an authority leak worse than the bug being
    /// fixed. Here there is nothing to miss: dropping the consult in memory
    /// ([`JobScheduler::abandon_advice`], or `advise.take()` on a reap) is what clears the
    /// record, on whatever save the exit was already going to do.
    ///
    /// Ordering obligation for callers, and the only one: whoever STARTS a consult must
    /// have set `self.advise` before saving (see [`JobScheduler::spawn_advice`]).
    fn save_ledger(&self, next: &mut AgentLoopState) -> Result<()> {
        self.save_ledger_inner(next, true)
    }

    /// Save state after folding in `control.json`.
    ///
    /// A newly read human cadence is authoritative over the older pinned value on disk. Re-merging
    /// that old value would undo the edit and make every sweep restart the deadline from `now`.
    /// Wake-only control changes still use the ordinary pinned-cadence race protection.
    fn save_control_ledger(
        &self,
        next: &mut AgentLoopState,
        human_cadence_changed: bool,
    ) -> Result<()> {
        self.save_ledger_inner(next, !human_cadence_changed)
    }

    fn save_ledger_inner(
        &self,
        next: &mut AgentLoopState,
        preserve_pinned_cadence: bool,
    ) -> Result<()> {
        next.advice_inflight = self.parked_advice();
        next.decider_latched = self.advise_health.latched;
        next.decider_latch_reason = self.advise_health.reason.clone();
        // PRESERVE A HUMAN-PINNED CADENCE across this write. pmtui's `c` edit
        // (`AgentLoopState::retime`) sets `cadence_s` AND `cadence_pinned` on disk out of
        // band; pmd's `next` was cloned from the `base` it loaded at the START of this tick,
        // so a human edit that lands mid-tick is REVERTED when pmd writes that stale clone
        // back — a last-writer-wins loss on a field the human explicitly owns (the exact
        // "5 minutes … revert my cadence setting later" class of bug). Re-reading the pinned
        // dial immediately before the write closes that window. It is safe because pmd never
        // legitimately writes cadence on a pinned ledger: `adopt_cadence` bails when
        // `cadence_pinned`, so once the human has turned the dial pmd has nothing to say. An
        // UNPINNED on-disk copy is left untouched — there pmd's own value (which may be a
        // fresh `adopt_cadence` this very tick) is the authority.
        if preserve_pinned_cadence
            && let Ok(disk) = state::read_json::<AgentLoopState>(&self.paths.pmstate())
            && disk.cadence_pinned
        {
            next.cadence_s = disk.cadence_s;
            next.cadence_pinned = true;
        }
        job::save(&self.paths, next)?;
        self.cleanup_completed_advice_artifacts(next);
        Ok(())
    }

    /// The consult debt this session currently owes: the live in-flight consult, or one
    /// inherited from a dead daemon and not yet honoured. `None` = nothing owed.
    fn parked_advice(&self) -> Option<job::ParkedAdvice> {
        self.advise
            .as_ref()
            .map(|i| i.parked.clone())
            .or_else(|| self.advise_orphaned.clone())
    }

    /// Reset the idle-confirmation gate: BOTH the consecutive-Idle counter and the
    /// transcript fingerprint it compares across observations. Called wherever earlier Idle
    /// observations of the pane are invalidated — a Busy observation, a human touch, a
    /// cold-start launch, a delivered nudge, a dead pane, an answer-resume — so the next
    /// nudge must re-earn two consecutive, byte-STABLE Idle captures rather than inheriting
    /// a stale count OR a stale fingerprint (a fingerprint outliving its count could let a
    /// single post-reset Idle that happens to match confirm on its own).
    fn reset_idle_gate(&mut self) {
        self.idle_confirmations = 0;
        self.last_idle_fingerprint = None;
        // NB: `turns_at_nudge` is deliberately NOT cleared here. `busy_recheck` calls this on
        // EVERY in-progress recheck, and the turn-count baseline must SURVIVE those (it is
        // exactly the "still awaiting the turn I nudged" state). It is set on a delivered
        // nudge and cleared only at cold-start relaunch (`ensure_session`), where a persisted
        // signal file would otherwise be compared against a stale baseline.
    }

    /// The number of turns the launched engine has completed so far = the SIZE of the
    /// per-session turn-complete signal (one byte appended per turn by the engine's turn-end
    /// hook), or `0` when the file is absent (the hook never fired). Read-only stat.
    fn turn_count(&self) -> u64 {
        std::fs::metadata(self.paths.turn_signal())
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Persist `run` (+ `updated_at`) onto the ledger and mirror it in memory — the
    /// small "re-park" save used for a busy-pane recheck or a transient-error recheck.
    ///
    /// `event` records ONE autopilot decision onto the ledger's [`AgentLoopState::events`]
    /// feed as part of the same write (coalesced, so a held nudge rechecked every few
    /// seconds is one "held ×N" line). `None` for a re-park that is not itself a
    /// human-facing decision (a supervisor poll, the marker-only fast-path).
    fn persist_run(
        &mut self,
        base: &AgentLoopState,
        now: Epoch,
        run: JobRun,
        event: Option<AutopilotEventKind>,
    ) -> Result<()> {
        let mut next = base.clone();
        next.run = run.clone();
        next.updated_at = now;
        if let Some(ev) = event {
            next.record_event(now, ev);
        }
        self.save_ledger(&mut next)?;
        self.run = run;
        Ok(())
    }

    /// Append one full-fidelity line to `raw.jsonl`, rotating to a single `.1` generation
    /// FIRST if the file is at/over `max_bytes`. Size-check-then-rename-then-append (the
    /// `append_decision_note` style; there is no append-atomic primitive, so a crash
    /// mid-`writeln` can tear the last line — readers must parse leniently). Returns `Err`
    /// on any I/O failure; the caller error-ISOLATES it (a side-file failure must never fail
    /// the state.json write).
    fn append_raw(&self, line: &str, max_bytes: u64) -> Result<()> {
        use std::io::Write as _;
        let path = self.paths.raw_jsonl();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create_dir_all {}", dir.display()))?;
        }
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= max_bytes {
            // Best-effort rotation: on failure we simply keep appending to the current file.
            let _ = std::fs::rename(&path, self.paths.raw_jsonl_rotated());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        writeln!(f, "{line}").with_context(|| format!("append {}", path.display()))?;
        Ok(())
    }

    /// Append a source-attributed line to `decisions.md` for a NOTABLE decision only
    /// (`Escalated`/`Stalled`; everything else is `raw.jsonl`-only). Rotates to a single
    /// `.1` generation at `DECISIONS_MD_MAX_BYTES`. Line format: `- {epoch} pmd {kind}:
    /// {summary}`. Error-isolated by the caller.
    fn append_decision_md(
        &self,
        now: Epoch,
        kind: crate::job::DecisionKind,
        summary: Option<&str>,
    ) -> Result<()> {
        use crate::job::DecisionKind;
        use std::io::Write as _;
        if !matches!(kind, DecisionKind::Escalated | DecisionKind::Stalled) {
            return Ok(());
        }
        let path = self.paths.decisions();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create_dir_all {}", dir.display()))?;
        }
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= DECISIONS_MD_MAX_BYTES {
            let _ = std::fs::rename(&path, self.paths.decisions_rotated());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        writeln!(
            f,
            "- {now} pmd {}: {}",
            kind.as_str(),
            summary.unwrap_or("").trim()
        )
        .with_context(|| format!("append {}", path.display()))?;
        Ok(())
    }

    /// Record the live persistent-session handle in `driver.json` (best-effort
    /// observability + the crash-recovery handle written BEFORE the ledger `run`
    /// flip). For the persistent model the handle is session-scoped: `seq` is a stable
    /// `0` and `deadline` is vestigial (a restart re-launches by name, not by seq).
    fn write_driver_running(&self, seq: u64, session: &str, now: Epoch, deadline: Epoch) {
        let _ = state::write_driver(
            &self.paths,
            &DriverState {
                step_id: seq,
                pane: session.to_string(),
                spawned_at: now,
                deadline,
                ended_at: None,
                exit_code: None,
                exit_reason: ExitReason::Running,
                consecutive_failures: 0,
                observed_at: now,
            },
        );
    }

    /// Record a session's terminal fate in `driver.json` (best-effort).
    fn write_driver_end(
        &self,
        seq: u64,
        session: &str,
        reason: ExitReason,
        exit_code: Option<i32>,
        now: Epoch,
    ) {
        let _ = state::write_driver(
            &self.paths,
            &DriverState {
                step_id: seq,
                pane: session.to_string(),
                spawned_at: 0,
                deadline: 0,
                ended_at: Some(now),
                exit_code,
                exit_reason: reason,
                consecutive_failures: 0,
                observed_at: now,
            },
        );
    }

    /// Stop the persistent session immediately (used when a human closes the
    /// session). Terminates the unified terminal so the agent cannot keep editing
    /// — plus any stray ephemeral `pmj-<seq>` worker still named in a pre-upgrade
    /// `Running` handle — records the end, and returns to Idle.
    pub fn abort(&mut self, driver: &dyn Driver, clock: &dyn Clock) -> Result<()> {
        self.interrupt_advice(driver, clock.now(), "the managed session was removed")?;
        if let JobRun::Running { seq, session, .. } = self.run.clone() {
            driver.terminate(&session)?;
            self.write_driver_end(seq, &session, ExitReason::Failed, None, clock.now());
        }
        let session = self.loop_session();
        driver.terminate(&session)?;
        self.write_driver_end(0, &session, ExitReason::Failed, None, clock.now());
        self.run = JobRun::Idle;
        Ok(())
    }

    pub fn interrupt_advice(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        reason: &str,
    ) -> Result<()> {
        let seq = self
            .advise
            .as_ref()
            .map(|inflight| inflight.parked.seq)
            .or_else(|| self.advise_orphaned.as_ref().map(|parked| parked.seq));
        let Some(seq) = seq else {
            return Ok(());
        };
        if let Some(inflight) = self.advise.as_ref() {
            driver.terminate(&inflight.handle.session)?;
        } else {
            let session = tmux::supervisor_session_name(&self.project_id, &self.work_dir, seq);
            driver.terminate(&session)?;
        }
        let live = self.advise.take();
        let orphaned = self.advise_orphaned.take();
        let persisted = if let Some(mut current) = job::load(&self.paths)? {
            current.finish_decider_run(
                seq,
                now,
                job::DeciderOutcome::Interrupted {
                    reason: reason.to_string(),
                },
            );
            current.updated_at = now;
            self.save_ledger(&mut current)
        } else {
            Ok(())
        };
        if let Err(error) = persisted {
            self.advise = live;
            self.advise_orphaned = orphaned;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
