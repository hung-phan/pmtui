//! The dashboard spawn broker: it turns a request an agent published in its own session's
//! `spawn-requests/` directory into one Standard child session, and answers with a receipt
//! beside the request.
//!
//! Only one dashboard per registry is the broker: the one holding its dashboard singleton and the
//! registry's spawn-broker lease ([`App::take_broker_role`]); any other stays idle.
//!
//! Discovery runs inside `refresh` ([`App::discover_spawn_requests`]): it lists every row's
//! requests, skips the settled ones unopened, answers malformed ones, removes expired request
//! files, and queues the rest. Processing is a non-blocking state machine: `finish_frame` calls
//! [`App::step_spawns`] every frame, and each call moves every queued request by at most one
//! [`SpawnPhase`]. Nothing here sleeps or polls; readiness is observed across frames against
//! [`SpawnBroker::now_ms`].
//!
//! The rules that keep a Message from being sent twice, and where each is enforced:
//! - only the lease holder brokers ([`App::take_broker_role`]), and the stage re-checks for a row
//!   naming the request inside the write that appends one ([`App::stage`]);
//! - a receipt past `claimed` proves a row was staged, so a request whose row is gone is never
//!   claimed again ([`App::settle_without_row`]);
//! - a `claimed` receipt is written before any side effect ([`App::claim`]);
//! - `launch.state = attempted` is persisted before the launch, and nothing ever launches from
//!   `attempted` ([`App::launch_child`], [`App::recover_row`]);
//! - `failed_before_start` is persisted before its revalidated removal ([`App::discard_unstarted`]);
//! - a row whose `launch.outcome` is final short-circuits to its receipt ([`App::precheck`]); an
//!   unfinished one resumes from its launch state ([`App::recover_row`]);
//! - the registry is written only by these methods, on the dashboard's event-loop thread, and
//!   every write revalidates the row it changes ([`App::update_child`]).
//!
//! The broker writes the registry, the child's `config.json`/`control.json`/`brief.md` (through
//! New's seed), and receipts (only through [`spawn::write_receipt`]). It never writes a request,
//! `state.json`, `needs-you.json`, `checkpoint.json`, `answers.json` or `directive.md`.

use std::collections::{HashMap, HashSet};

use agent_manager::registry::{
    LaunchKind, LaunchRecord, LaunchState, MAX_CHILDREN_PER_PARENT, SpawnOutcome,
};
use agent_manager::spawn::{
    self, ErrorCode, Listed, NextAction, NextActionKind, ReceiptSession, ReceiptState, SpawnArgs,
    SpawnError, SpawnReceipt, SpawnRequest,
};
use agent_manager::tmux::LaunchOutcome;
use agent_manager::worktree;

use super::create::{NewSession, fresh_launch_bytes, fresh_standard_argv, reserve_new_session};
use crate::*;

/// How long a started child has to show a live, dialog-free terminal.
const READINESS_WINDOW_MS: u128 = 3_000;
/// The least spacing between the two live observations that prove readiness.
const READINESS_GAP_MS: u128 = 100;
/// How long after its first live observation a child still has for the second one, however late
/// that first one came: a frame that stalled past the window must not turn a live child unknown.
const READINESS_AFTER_FIRST_MS: u128 = 1_000;
/// Pane lines captured to look for a dialog, the depth the dashboard's activity capture uses.
const READINESS_CAPTURE_LINES: usize = 40;

/// Everything the broker keeps between frames.
/// How long a cancelled job is given between the signal and the kill.
///
/// Neither engine has a "stop and emit your final payload" handshake, so TIME is the only thing a cancel
/// can grant: five seconds is long enough for a run to flush what it has written and short enough that a
/// human who pressed `d` sees the row go. The kill is unconditional once it is up.
pub(crate) const CANCEL_GRACE_MS: u128 = 5_000;

pub(crate) struct SpawnBroker {
    /// The requests in flight, in the order each frame advances them: resumed ones first.
    pub(crate) jobs: Vec<SpawnJob>,
    /// Whether the first [`App::step_spawns`] has run its startup discovery.
    pub(crate) recovered: bool,
    /// Monotonic milliseconds, the only clock readiness reads. A test replaces it.
    pub(crate) now_ms: Box<dyn Fn() -> u128>,
    /// The broker changed the registry this frame, so the rows on screen are stale.
    dirty: bool,
    /// Malformed entries already noted in the log, so each is reported once.
    noted: HashSet<String>,
    /// Requests whose receipt discovery found final, by `(requests dir, request id)`, with the
    /// time it became final. Each refresh skips them without opening anything, and removes a
    /// listed request file once its receipt is past retention.
    settled: HashMap<(PathBuf, String), Epoch>,
    /// This dashboard's hold on the registry's spawn-broker lease
    /// ([`lease::spawn_broker_lock_path`]). Taken on the first frame it is free, then held for the
    /// dashboard's life; the broker is idle without it.
    lease: Option<lease::ProjectLease>,
    /// Job terminals already sent SIGTERM for a cancel, each with the monotonic millisecond the ask
    /// went out. A frame that finds one still alive past [`CANCEL_GRACE_MS`] kills it, which is how
    /// "polite first, then a kill" happens without the dashboard ever sleeping on a process.
    asked_to_stop: HashMap<String, u128>,
    /// A harvest is running. Retiring a row ends in [`App::refresh`], which calls the harvest again,
    /// so without this ONE sweep of five finished children re-enters itself four levels deep, each
    /// level holding a snapshot that still lists rows the deeper ones already retired and purged.
    harvesting: bool,
}

impl Default for SpawnBroker {
    fn default() -> Self {
        let origin = std::time::Instant::now();
        Self {
            jobs: Vec::new(),
            recovered: false,
            now_ms: Box::new(move || origin.elapsed().as_millis()),
            dirty: false,
            noted: HashSet::new(),
            settled: HashMap::new(),
            lease: None,
            asked_to_stop: HashMap::new(),
            harvesting: false,
        }
    }
}

/// One request in flight.
pub(crate) struct SpawnJob {
    /// The session whose requests directory holds the request: the would-be parent.
    pub(crate) parent: String,
    /// That session's `spawn-requests/` directory, where the receipt goes; `None` for an orphan
    /// whose parent is no longer in the session list, which has nowhere to answer.
    pub(crate) dir: Option<PathBuf>,
    pub(crate) request: SpawnRequest,
    /// [`SpawnArgs::args_hash`] of the request, the half of its identity a row records.
    pub(crate) hash: String,
    pub(crate) phase: SpawnPhase,
    /// A staged row that no request file backs any more ([`App::adopt_orphaned_rows`]). It is
    /// finished from its row alone and never launched.
    pub(crate) orphan: bool,
}

/// The row a request staged: its stable id and its project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnChild {
    pub(crate) id: String,
    pub(crate) root: PathBuf,
}

/// Where a request is. [`App::step_spawns`] moves it by at most one phase per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnPhase {
    /// Queued by discovery; next: the pre-check against the registry, then the claim.
    Discovered,
    /// Its `claimed` receipt is written; next: validate and stage.
    Claimed,
    /// Its row is in the registry; next: launch it from `pending`, or resume whatever launch
    /// state an earlier frame or dashboard left.
    Staged { child: SpawnChild },
    /// Its terminal started; next: observe it until it is ready, needs a human, or the window
    /// closes.
    Readiness {
        child: SpawnChild,
        first_ok_ms: Option<u128>,
        deadline_ms: u128,
    },
    /// Nothing left to do; dropped at the end of the frame.
    Done,
}

/// What one readiness observation saw.
enum ChildTerminal {
    /// Alive, not dead, and no recognized dialog.
    Live,
    /// Absent or dead: not up (yet).
    NotLive,
    /// A trust, setup or permission dialog only a human may answer.
    Dialog,
}

/// A request that passed validation: the child it describes, defaults resolved from the parent.
struct ChildPlan {
    dir: PathBuf,
    engine: Engine,
    model: Option<String>,
    title: String,
    name: Option<String>,
    message: String,
}

impl SpawnJob {
    fn new(parent: String, dir: PathBuf, request: SpawnRequest) -> Self {
        let hash = request.args.args_hash();
        Self {
            parent,
            dir: Some(dir),
            request,
            hash,
            phase: SpawnPhase::Discovered,
            orphan: false,
        }
    }

    /// The job that finishes `row`, a staged row whose request file is gone: its identity comes
    /// from the row's launch record. Its request carries no arguments, and none are needed,
    /// because an orphan resumes at [`SpawnPhase::Staged`] and is never validated or launched.
    fn orphan(
        parent: String,
        dir: Option<PathBuf>,
        row: &ProjectEntry,
        launch: &LaunchRecord,
    ) -> Self {
        Self {
            request: SpawnRequest {
                schema_version: spawn::RECEIPT_SCHEMA_VERSION,
                request_id: launch.request_id.clone(),
                parent_session: parent.clone(),
                created_at: 0,
                args: SpawnArgs {
                    message: String::new(),
                    title: None,
                    name: None,
                    dir: None,
                    agent: None,
                    model: None,
                },
            },
            parent,
            dir,
            hash: launch.args_hash.clone(),
            phase: SpawnPhase::Staged {
                child: SpawnChild::of(row),
            },
            orphan: true,
        }
    }

    fn id(&self) -> &str {
        &self.request.request_id
    }

    /// Whether `entry` was staged by a request with this job's parent and request id, whatever
    /// its arguments were.
    fn names(&self, entry: &ProjectEntry) -> bool {
        entry.spawned_by.as_deref() == Some(self.parent.as_str())
            && entry
                .launch
                .as_ref()
                .is_some_and(|launch| launch.request_id == self.request.request_id)
    }

    /// Whether `entry` is exactly the row this job staged as `child`.
    fn is_row(&self, entry: &ProjectEntry, child: &SpawnChild) -> bool {
        entry.id == child.id
            && entry.root == child.root
            && self.names(entry)
            && entry
                .launch
                .as_ref()
                .is_some_and(|launch| launch.args_hash == self.hash)
    }

    /// The row this job's request staged, with its launch record: any row naming the request
    /// (`child: None`), or exactly `child`.
    fn find_row(
        &self,
        reg: &Registry,
        child: Option<&SpawnChild>,
    ) -> Option<(ProjectEntry, LaunchRecord)> {
        reg.projects.iter().find_map(|entry| {
            let launch = entry.launch.as_ref()?;
            let ours = match child {
                Some(child) => self.is_row(entry, child),
                None => self.names(entry),
            };
            ours.then(|| (entry.clone(), launch.clone()))
        })
    }

    /// A receipt in `state` naming no session yet.
    fn receipt(&self, state: ReceiptState) -> SpawnReceipt {
        SpawnReceipt {
            schema_version: spawn::RECEIPT_SCHEMA_VERSION,
            request_id: self.request.request_id.clone(),
            state,
            claimed_by: Some(format!("pid:{}", std::process::id())),
            session: None,
            launch_state: None,
            error: None,
            next_action: None,
            args_hash: Some(self.hash.clone()),
            result: None,
            work: None,
            updated_at: SystemClock.now(),
        }
    }

    /// A receipt in `state` for the staged `row`.
    fn row_receipt(
        &self,
        row: &ProjectEntry,
        state: ReceiptState,
        launch_state: LaunchState,
    ) -> SpawnReceipt {
        SpawnReceipt {
            session: Some(receipt_session(row)),
            launch_state: Some(launch_state),
            ..self.receipt(state)
        }
    }
}

impl SpawnChild {
    fn of(row: &ProjectEntry) -> Self {
        Self {
            id: row.id.clone(),
            root: row.root.clone(),
        }
    }

    fn session(&self) -> String {
        session_name(&self.id, &self.root)
    }
}

impl App {
    /// Called from `refresh` with the registry it just loaded: list each row's requests, queue
    /// the new ones (at most [`spawn::MAX_UNFINISHED_PER_PARENT`] in flight per parent, requests
    /// with an unfinished receipt first), answer invalid entries with `invalid_request`, note
    /// entries with an untrustworthy name once in the log, and remove request files whose receipt
    /// has been final for longer than [`spawn::RETENTION_SECS`].
    ///
    /// A request whose receipt is final is settled: it is remembered and never opened again. Each
    /// other entry's receipt is read before its request is parsed, and at most
    /// [`spawn::MAX_OPENED_PER_PARENT`] such entries are opened per parent per refresh.
    pub(crate) fn discover_spawn_requests(&mut self, reg: &Registry) {
        let now = SystemClock.now();
        let mut resumed = Vec::new();
        let mut fresh = Vec::new();
        for owner in &reg.projects {
            let dir = spawn::requests_dir(&entry_state_paths(owner).state_dir());
            let mut owner_resumed = Vec::new();
            let mut owner_fresh = Vec::new();
            let mut opened = 0;
            for listed in spawn::list_requests(&dir) {
                let id = match listed {
                    Listed::Request(id) => id,
                    Listed::Skipped { name, reason } => {
                        self.note_once(
                            format!("{}/{name}", dir.display()),
                            format!("spawn: skipped {name} in {}'s requests: {reason}", owner.id),
                        );
                        continue;
                    }
                };
                if self.has_job(&owner.id, &id) {
                    continue;
                }
                if let Some(&final_at) = self.spawn.settled.get(&(dir.clone(), id.clone())) {
                    self.retire(&dir, &id, final_at, now);
                    continue;
                }
                if opened == spawn::MAX_OPENED_PER_PARENT {
                    self.note_once(
                        format!("{}#capped", dir.display()),
                        format!(
                            "spawn: {}'s requests hold more than {} unsettled entries; the rest \
                             wait for a later refresh",
                            owner.id,
                            spawn::MAX_OPENED_PER_PARENT
                        ),
                    );
                    break;
                }
                opened += 1;
                let unfinished = match spawn::read_receipt(&dir, &id) {
                    Ok(Some(receipt)) if receipt.state.is_final() => {
                        self.spawn
                            .settled
                            .insert((dir.clone(), id.clone()), receipt.updated_at);
                        self.retire(&dir, &id, receipt.updated_at, now);
                        continue;
                    }
                    Ok(Some(_)) => true,
                    Ok(None) | Err(_) => false,
                };
                match spawn::load_request_entry(&dir, &id, &owner.id) {
                    Ok(request) if unfinished => owner_resumed.push(*request),
                    Ok(request) => owner_fresh.push(*request),
                    Err(reason) => self.answer_invalid(reg, &owner.id, &dir, &id, &reason, now),
                }
            }
            let active = self
                .spawn
                .jobs
                .iter()
                .filter(|job| job.parent == owner.id)
                .count();
            let room = spawn::MAX_UNFINISHED_PER_PARENT.saturating_sub(active);
            let queued = owner_resumed.len().min(room);
            resumed.extend(
                owner_resumed
                    .drain(..queued)
                    .map(|request| (owner.id.clone(), dir.clone(), request)),
            );
            fresh.extend(
                owner_fresh
                    .into_iter()
                    .take(room - queued)
                    .map(|request| (owner.id.clone(), dir.clone(), request)),
            );
        }
        for (parent, dir, request) in resumed.into_iter().chain(fresh) {
            self.spawn.jobs.push(SpawnJob::new(parent, dir, request));
        }
    }

    /// Advance every queued request by at most one phase. Never sleeps and never waits: a
    /// phase that must wait (a held driver lock, an unreadable session list, readiness) returns
    /// and is tried again next frame. The first call discovers what a dashboard that died or was
    /// replaced left unfinished, so it is resumed before anything published later.
    pub(crate) fn step_spawns(&mut self) {
        if !self.take_broker_role() {
            return;
        }
        if !self.spawn.recovered {
            self.spawn.recovered = true;
            if let Ok(reg) = Registry::load(&self.registry_path) {
                self.discover_spawn_requests(&reg);
                self.adopt_orphaned_rows(&reg);
            }
        }
        if self.spawn.jobs.is_empty() {
            return;
        }
        let mut jobs = std::mem::take(&mut self.spawn.jobs);
        for job in &mut jobs {
            let phase = std::mem::replace(&mut job.phase, SpawnPhase::Done);
            job.phase = self.advance(job, phase);
        }
        jobs.retain(|job| job.phase != SpawnPhase::Done);
        jobs.append(&mut self.spawn.jobs);
        self.spawn.jobs = jobs;
        if std::mem::take(&mut self.spawn.dirty) {
            self.refresh();
        }
    }

    /// At recovery, after the startup discovery: a staged row that no job resumed is stuck,
    /// because every start path refuses it, unless something finishes it from the row. That is
    /// a row whose request file is gone, whose parent left the session list (whose requests are
    /// never read again), or whose receipt is already final. Each gets an orphan job: `pending`
    /// and `failed_before_start` are discarded, since nothing launched them, and `attempted` is
    /// promoted when its terminal lives, or else enabled as `outcome_unknown`; none is launched.
    /// A `pending` row whose request is still waiting to be read is left for discovery.
    fn adopt_orphaned_rows(&mut self, reg: &Registry) {
        let staged = reg.projects.iter().filter(|row| row.is_staged_spawn());
        for (row, parent, launch) in staged.filter_map(|row| {
            let parent = row.spawned_by.clone()?;
            let launch = row.launch.clone()?;
            Some((row, parent, launch))
        }) {
            if self.has_job(&parent, &launch.request_id) {
                continue;
            }
            let dir = reg
                .projects
                .iter()
                .find(|entry| entry.id == parent)
                .map(|entry| spawn::requests_dir(&entry_state_paths(entry).state_dir()));
            let awaits_discovery = dir.as_ref().is_some_and(|dir| {
                std::fs::symlink_metadata(spawn::request_path(dir, &launch.request_id)).is_ok()
                    && !self
                        .spawn
                        .settled
                        .contains_key(&(dir.clone(), launch.request_id.clone()))
            });
            if launch.state == LaunchState::Pending && awaits_discovery {
                continue;
            }
            self.spawn
                .jobs
                .push(SpawnJob::orphan(parent, dir, row, &launch));
        }
    }

    /// Whether this dashboard is the spawn broker: it holds the dashboard singleton and the
    /// registry's spawn-broker lease. Only then does `refresh` discover requests.
    pub(crate) fn is_spawn_broker(&self) -> bool {
        self.dashboard_owner_nonce.is_some() && self.spawn.lease.is_some()
    }

    /// Become the spawn broker if this dashboard may: it must hold the dashboard singleton, which
    /// is per registry and socket, and the spawn-broker lease, which is per registry alone, so two
    /// dashboards on one session list never both turn a request into a child. The lease is tried
    /// on every frame until it is taken, then held for the dashboard's life. Until then the broker
    /// stays idle, and the status log says why once.
    fn take_broker_role(&mut self) -> bool {
        if self.is_spawn_broker() {
            return true;
        }
        if self.dashboard_owner_nonce.is_none() {
            self.note_once(
                "broker-idle:singleton".into(),
                "spawn: this dashboard does not hold its single-instance lock, so it leaves spawn \
                 requests to another dashboard"
                    .into(),
            );
            return false;
        }
        let path = lease::spawn_broker_lock_path(&self.registry_path);
        match lease::try_acquire(&path) {
            Ok(Some(held)) => {
                self.spawn.lease = Some(held);
                true
            }
            Ok(None) => {
                self.note_once(
                    "broker-idle:held".into(),
                    "spawn: another dashboard is brokering spawn requests for this session list; \
                     this one leaves them to it"
                        .into(),
                );
                false
            }
            Err(error) => {
                self.note_once(
                    "broker-idle:error".into(),
                    format!(
                        "spawn: could not open the spawn-broker lock {} ({error}); spawn requests \
                         wait until it opens",
                        path.display()
                    ),
                );
                false
            }
        }
    }

    fn advance(&mut self, job: &SpawnJob, phase: SpawnPhase) -> SpawnPhase {
        match phase {
            SpawnPhase::Discovered => self.precheck(job),
            SpawnPhase::Claimed => self.stage(job),
            SpawnPhase::Staged { child } => self.continue_staged(job, child),
            SpawnPhase::Readiness {
                child,
                first_ok_ms,
                deadline_ms,
            } => self.check_readiness(job, child, first_ok_ms, deadline_ms),
            SpawnPhase::Done => SpawnPhase::Done,
        }
    }

    /// Pre-check: a row this request already staged decides what happens. A final outcome is
    /// reported again from the row (so a replay after the receipt was cleaned up is safe), an
    /// unfinished row resumes from its launch state, and a row with other arguments under the
    /// same id is a conflict. With no row, the request is claimed.
    fn precheck(&mut self, job: &SpawnJob) -> SpawnPhase {
        let reg = match Registry::load(&self.registry_path) {
            Ok(reg) => reg,
            Err(error) => return self.wait_for_registry(job, SpawnPhase::Discovered, &error),
        };
        let Some((row, launch)) = job.find_row(&reg, None) else {
            return self.settle_without_row(job, true);
        };
        if launch.args_hash != job.hash {
            let message = format!(
                "request {} already created {} with different arguments",
                job.id(),
                row.id
            );
            self.status = format!("spawn request from {} failed: {message}", job.parent);
            // The id stays bound to the arguments that created its child, so a replay of those
            // arguments is recognized and any other replay is a conflict too.
            let receipt = SpawnReceipt {
                error: Some(SpawnError {
                    code: ErrorCode::RequestConflict,
                    message,
                }),
                args_hash: Some(launch.args_hash),
                ..job.receipt(ReceiptState::Failed)
            };
            self.put_receipt(job, receipt);
            return SpawnPhase::Done;
        }
        if let Some(outcome) = launch.outcome {
            let receipt = self.final_receipt(
                job,
                &SpawnChild::of(&row),
                Some(&row),
                outcome,
                launch.state,
                None,
            );
            self.put_receipt(job, receipt);
            return SpawnPhase::Done;
        }
        self.recover_row(job, SpawnChild::of(&row), row, launch.state, false)
    }

    /// No row names this request. A receipt past `claimed` (or one naming a session or a launch
    /// state) proves the request already staged a row, which is gone now, so it is never claimed
    /// again: a launch that was attempted or started may have had its Message read, so it is
    /// `outcome_unknown`; any other is `failed`, because its staged row was removed. Without such
    /// proof nothing was staged, and the pre-check (`claim`) restarts at validation.
    fn settle_without_row(&mut self, job: &SpawnJob, claim: bool) -> SpawnPhase {
        let previous = job
            .dir
            .as_deref()
            .and_then(|dir| spawn::read_receipt(dir, job.id()).ok().flatten())
            .filter(proves_staged);
        if claim && previous.is_none() {
            return self.claim(job);
        }
        if let Some(previous) = previous.filter(|previous| {
            matches!(
                previous.launch_state,
                Some(LaunchState::Attempted | LaunchState::Started)
            )
        }) {
            let next_action = previous
                .session
                .as_ref()
                .map(|session| self.attach_action(&session.tmux_session));
            let message = "its row is gone after its launch was attempted; its Message may have \
                           been read, so it is not sent again"
                .to_string();
            self.status = format!(
                "spawn request from {}: launch outcome unknown ({message})",
                job.parent
            );
            let receipt = SpawnReceipt {
                session: previous.session,
                launch_state: previous.launch_state,
                error: Some(SpawnError {
                    code: ErrorCode::ReadinessUnknown,
                    message,
                }),
                next_action,
                ..job.receipt(ReceiptState::OutcomeUnknown)
            };
            self.put_receipt(job, receipt);
            return SpawnPhase::Done;
        }
        self.fail(
            job,
            ErrorCode::LaunchFailed,
            "its staged row was removed before its launch, so nothing ran; it is never staged \
             again under this request id"
                .into(),
        )
    }

    /// Claim: the `claimed` receipt is written before any side effect, and a request whose claim
    /// cannot be written does nothing (discovery offers it again).
    fn claim(&mut self, job: &SpawnJob) -> SpawnPhase {
        match write_job_receipt(job, &job.receipt(ReceiptState::Claimed)) {
            Ok(()) => SpawnPhase::Claimed,
            Err(error) => {
                self.status = format!(
                    "spawn request from {}: could not claim {} ({error:#})",
                    job.parent,
                    job.id()
                );
                SpawnPhase::Done
            }
        }
    }

    /// Validate, then stage: reserve the id and seed the child exactly as New does, append its
    /// disabled row with `launch.state = pending`, and write the `staged` receipt.
    fn stage(&mut self, job: &SpawnJob) -> SpawnPhase {
        let reg = match Registry::load(&self.registry_path) {
            Ok(reg) => reg,
            Err(error) => {
                return self.fail(
                    job,
                    ErrorCode::RegistryUnreadable,
                    format!("could not read the session list: {error}"),
                );
            }
        };
        let plan = match plan_child(&reg, &job.request) {
            Ok(plan) => plan,
            Err(PlanRefusal::Refused(refusal)) => {
                return self.fail(job, refusal.code, refusal.message);
            }
            // WAITS, rather than being answered. Its `claimed` receipt reads as `in_progress` to the
            // parent — `queued` is what the command says when there is NO receipt at all — so the next
            // frame after a sibling retires stages it with nothing to re-ask.
            Err(PlanRefusal::NotYet) => {
                self.note_once(
                    format!("queued:{}", job.id()),
                    format!(
                        "spawn: {} already has {MAX_CHILDREN_PER_PARENT} children running; {} is queued",
                        job.request.parent_session,
                        job.id()
                    ),
                );
                return SpawnPhase::Claimed;
            }
        };
        let seed = NewSession {
            tier: Tier::Standard,
            engine: plan.engine,
            decider_engine: Engine::Claude,
            decider_model: None,
            brief: "",
            cadence_s: None,
        };
        let id = match reserve_new_session(&reg, &plan.dir, seed, SystemClock.now()) {
            Ok(id) => id,
            Err(error) => {
                return self.fail(
                    job,
                    ErrorCode::LaunchFailed,
                    format!("could not set up the session: {error:#}"),
                );
            }
        };
        let row = ProjectEntry {
            id: id.clone(),
            display_name: plan.name,
            root: plan.dir,
            enabled: false,
            mode: Mode::AgentLoop,
            engine: Some(plan.engine),
            worker_model: plan.model,
            initial_prompt: Some(plan.message),
            task_title: Some(plan.title),
            forked_from: None,
            spawned_by: Some(job.parent.clone()),
            launch: Some(LaunchRecord {
                request_id: job.request.request_id.clone(),
                args_hash: job.hash.clone(),
                state: LaunchState::Pending,
                outcome: None,
                // Every spawn this build stages is a JOB: a one-shot run that reports and exits.
                // `Chat` exists only for rows an older build staged.
                kind: LaunchKind::Job,
                // Filled when the launch creates the child's worktree, which is the first moment there
                // is a branch to name.
                branch: None,
                base_commit: None,
            }),
            conversation_id: None,
            cadence_s: None,
        };
        // The pre-check again, against the session list this write loads: a row that names the
        // request by now is the request's row, and a second one would be a second child.
        let staged = row.clone();
        let mut existing = None;
        let pushed = Registry::update(&self.registry_path, |reg| {
            existing = reg
                .projects
                .iter()
                .find(|entry| job.names(entry))
                .map(|entry| entry.id.clone());
            if existing.is_none() {
                reg.projects.push(staged);
            }
        });
        if let Err(error) = pushed {
            return self.fail(
                job,
                ErrorCode::RegistryUnreadable,
                format!("could not save the session list: {error}"),
            );
        }
        if let Some(existing) = existing {
            let released =
                super::forking::cleanup_unstaged_fork(&ProjectPaths::for_session(&row.root, &id));
            self.status = match released {
                Ok(()) => format!(
                    "spawn: {existing} already stages request {}; resuming it",
                    job.id()
                ),
                Err(error) => format!(
                    "spawn: {existing} already stages request {}; resuming it, but could not \
                     release {id} ({error:#})",
                    job.id()
                ),
            };
            return SpawnPhase::Discovered;
        }
        self.spawn.dirty = true;
        self.put_receipt(
            job,
            job.row_receipt(&row, ReceiptState::Staged, LaunchState::Pending),
        );
        SpawnPhase::Staged {
            child: SpawnChild::of(&row),
        }
    }

    /// A staged request: re-read its row, launch it from `pending`, or resume any other state.
    fn continue_staged(&mut self, job: &SpawnJob, child: SpawnChild) -> SpawnPhase {
        let reg = match Registry::load(&self.registry_path) {
            Ok(reg) => reg,
            Err(error) => {
                return self.wait_for_registry(job, SpawnPhase::Staged { child }, &error);
            }
        };
        let Some((row, launch)) = job.find_row(&reg, Some(&child)) else {
            return self.settle_without_row(job, false);
        };
        self.recover_row(job, child, row, launch.state, true)
    }

    /// RETIRE EVERY FINISHED JOB. Called from `refresh` with the registry it just loaded, after the
    /// request scan, because a job's row outlives its request's receipt going final: the receipt is
    /// `Ready` (launched and running) while the work happens, and this is what writes what became of
    /// it.
    ///
    /// Completion is a FACT here, never an inference: a job is finished when its tmux session is gone,
    /// and what it achieved is read from what the harness left — the tee'd `job.log`, codex's `-o`
    /// file, and the exit code `spawn_step`'s wrapper lands in `job.done`. Nothing polls for idleness
    /// (this codebase's own scar: the status glyph means working, not alive) and nothing depends on the
    /// agent having remembered a final step.
    ///
    /// `done` retires the row. Every other outcome KEEPS it, inert, so an outcome nobody has seen
    /// cannot be swept away — Claude Code holds a failed subagent's row for the same reason, and
    /// agent-deck archives rather than deletes.
    pub(crate) fn retire_finished_jobs(&mut self, reg: &Registry) {
        // ONE SWEEP AT A TIME. `remove_project` ends in `refresh`, which calls this again: five
        // children finishing together re-entered this four levels deep, and each outer level then
        // carried on down its own stale candidate list, re-harvesting rows whose state directory the
        // inner level had already purged. The second harvest of a `done` child finds no `job.done`
        // and no payload, so it overwrote the parent's correct answer with `ended_without_result` —
        // three of five children, every time, reproduced by
        // `five_children_dispatched_at_once_each_get_their_own_answer`.
        if self.spawn.harvesting {
            return;
        }
        self.spawn.harvesting = true;
        let candidates: Vec<(ProjectEntry, String)> = reg
            .projects
            .iter()
            .filter(|row| {
                row.launch
                    .as_ref()
                    .is_some_and(|launch| launch.kind == LaunchKind::Job)
            })
            // READINESS OWNS THE ROW UNTIL IT FINISHES. A row is swept only once its launch recorded
            // an outcome: before that the request scan is still deciding whether the child came up at
            // all, and two passes answering the same request would race (caught by
            // `a_child_whose_second_observation_never_comes_is_unknown…`, which this sweep was
            // finishing as `ended_without_result` before readiness could call it unknown).
            //
            // `outcome_unknown` IS SWEPT TOO, because for a JOB it is not a verdict about the work:
            // readiness wants two live observations, and a job that did its task — or died on a bad
            // argument — in under that window never shows them. Found by running a real `claude -p`
            // job, which exited in milliseconds and left a row nobody would ever finish, with its
            // reason sitting unread in `job.log`. A `needs_attention` launch is still left alone: that
            // one asked for a person.
            .filter(|row| {
                row.launch.as_ref().is_some_and(|launch| {
                    launch.state == LaunchState::Started
                        && matches!(
                            launch.outcome,
                            Some(SpawnOutcome::Ready | SpawnOutcome::OutcomeUnknown)
                        )
                })
            })
            .map(|row| (row.clone(), session_name(&row.id, &row.root)))
            .collect();
        for (row, session) in candidates {
            // A CANCEL OUTRANKS EVERYTHING ELSE here, including the result the job may be about to
            // write: someone asked for this to stop, so the answer is `cancelled` rather than whatever
            // a half-finished run left behind.
            if self.cancel_asked(&row) {
                self.stop_cancelled_job(&row, &session);
                continue;
            }
            // STILL RUNNING, or a human is watching it: either way, not now. A live terminal is the
            // job working; an attached human is someone reading it, and killing a pane out from under
            // them is the one thing retirement must never do.
            if self.agent_tmux.is_alive(&session).unwrap_or(true) {
                continue;
            }
            if self.agent_tmux.has_clients(&session).unwrap_or(false) {
                continue;
            }
            self.finish_job_row(&row);
        }
        self.spawn.harvesting = false;
    }

    /// Whether this job's parent (or anyone else with its requests directory) asked it to stop.
    fn cancel_asked(&self, row: &ProjectEntry) -> bool {
        let Some(launch) = row.launch.as_ref() else {
            return false;
        };
        self.parent_requests_dir(row)
            .is_some_and(|dir| spawn::cancel_requested(&dir, &launch.request_id))
    }

    /// Stop a job a cancel was asked for: SIGTERM, a KILL once its grace window is up, then the
    /// `cancelled` receipt and retirement.
    ///
    /// POLITE FIRST, NEVER BLOCKING, AND NOT ON THE NEXT FRAME. The dashboard must not sleep waiting for
    /// a process to die, so the steps are spread over frames — but frames are milliseconds apart, and a
    /// kill that lands that fast gives the run no chance to flush what it had. There is no handshake for
    /// "stop and emit your final payload" in either engine, so the only thing that can be granted is
    /// TIME: [`CANCEL_GRACE_MS`] after the signal, and only then the kill. A job that was already gone
    /// skips both, and a job that exits inside the window is harvested with whatever it reported.
    fn stop_cancelled_job(&mut self, row: &ProjectEntry, session: &str) {
        if self.agent_tmux.is_alive(session).unwrap_or(false) {
            let now = (self.spawn.now_ms)();
            match self.spawn.asked_to_stop.get(session).copied() {
                Some(asked) if now.saturating_sub(asked) < CANCEL_GRACE_MS => return,
                Some(_) => {
                    if let Err(error) = self.agent_tmux.terminate(session) {
                        self.log_line(&format!("spawn: could not kill {} ({error:#})", row.id));
                        return;
                    }
                }
                None => {
                    self.spawn.asked_to_stop.insert(session.to_string(), now);
                    if let Err(error) = self.agent_tmux.request_stop(session) {
                        self.log_line(&format!("spawn: could not signal {} ({error:#})", row.id));
                    }
                    self.status = format!("cancelling {} \u{2014} asked it to stop", row.id);
                    self.record_status();
                    return;
                }
            }
        }
        self.spawn.asked_to_stop.remove(session);
        self.clear_cancel_marker(row);
        // THE CANCEL LOST THE RACE. The job had already reported before anyone asked it to stop, so its
        // own outcome stands: reporting `cancelled` here would throw away work that was delivered.
        if job_result_of(row).is_some() {
            self.finish_job_row(row);
            return;
        }
        self.write_cancel_receipt(row, &format!("{} was cancelled by its parent", row.id));
        // UNCOMMITTED WORK OUTRANKS RETIREMENT HERE TOO. A killed run is the likeliest of all to have left
        // work it never committed, and that work exists nowhere but its worktree — which retiring the row
        // is what removes. The `done` path has always kept the row for this; the cancel path dropped it,
        // leaving the only copy with nothing on screen pointing at it.
        if self.job_left_uncommitted_work(row) {
            self.disable_row(&row.id);
            self.status = format!(
                "{} \u{b7} cancelled, with uncommitted work in its worktree",
                row.id
            );
            self.record_status();
            return;
        }
        self.remove_project(&row.id, session);
        self.status = format!("retired {} \u{b7} cancelled", row.id);
        self.record_status();
    }

    /// Drop a job's cancel marker once it has been acted on, so the request directory does not keep a
    /// stale instruction for an id that is finished.
    fn clear_cancel_marker(&mut self, row: &ProjectEntry) {
        if let Some(launch) = row.launch.as_ref()
            && let Some(dir) = self.parent_requests_dir(row)
            && let Err(error) = spawn::remove_cancel(&dir, &launch.request_id)
        {
            self.log_line(&format!(
                "spawn: could not clear {}'s cancel marker ({error:#})",
                row.id
            ));
        }
    }

    /// Whether this job's worktree holds changes it never committed — the one thing about a job that
    /// exists nowhere but that directory, and so the one thing no cleanup may remove.
    fn job_left_uncommitted_work(&self, row: &ProjectEntry) -> bool {
        job_worktree(row).is_some_and(|wt| worktree::collect(&wt).dirty)
    }

    /// Whether this job's parent already holds a final answer for it, which a later pass must not
    /// replace. A receipt that is merely `ready` (launched and running) is not an answer.
    fn job_already_answered(&self, row: &ProjectEntry, launch: &LaunchRecord) -> bool {
        self.parent_requests_dir(row).is_some_and(|dir| {
            spawn::read_receipt(&dir, &launch.request_id)
                .ok()
                .flatten()
                .is_some_and(|receipt| receipt.state.is_job_terminal())
        })
    }

    /// Record one finished job's outcome in its parent's receipt, then retire or keep its row.
    ///
    /// A JOB IS ANSWERED ONCE. The answer lives in the job's own state directory, which retirement
    /// deletes, so a second pass over the same row can only read an empty directory and report
    /// `ended_without_result` over an outcome that was already correct. Whatever route brought the row
    /// here twice, the recorded answer wins.
    fn finish_job_row(&mut self, row: &ProjectEntry) {
        let Some(launch) = row.launch.as_ref() else {
            return;
        };
        if self.job_already_answered(row, launch) {
            return;
        }
        let paths = entry_state_paths(row);
        let engine = row.engine.unwrap_or(Engine::Claude);
        let exit_code = std::fs::read_to_string(paths.job_done_signal())
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok());
        let result = job_result_of(row);

        let (state, error) = match result.as_ref().map(|found| found.outcome) {
            Some(spawn::JobOutcome::Done) => (ReceiptState::Done, None),
            Some(spawn::JobOutcome::NeedsHuman) => (ReceiptState::NeedsHuman, None),
            // A job failure still carries an `error.code`: v1's contract is "act on `error.code`", and
            // this must not be the one `failed` that has none.
            Some(spawn::JobOutcome::Failed) => (
                ReceiptState::Failed,
                Some(SpawnError {
                    code: ErrorCode::JobFailed,
                    message: result
                        .as_ref()
                        .map(|found| found.summary.clone())
                        .unwrap_or_else(|| "the job reported a failure".into()),
                }),
            ),
            None => (
                ReceiptState::EndedWithoutResult,
                Some(SpawnError {
                    code: ErrorCode::JobFailed,
                    message: match exit_code {
                        // 127 is the shell's "not found": the agent executable was not on PATH, which
                        // is worth naming rather than reporting as a mystery.
                        Some(127) => format!(
                            "the job's agent ({}) is not on PATH, so nothing ran",
                            engine.bin()
                        ),
                        Some(code) => {
                            format!("the job exited {code} without reporting a result")
                        }
                        None => "the job's terminal is gone and it left no result".into(),
                    },
                }),
            ),
        };

        // WHERE THE WORK IS. Read before anything is retired, because retirement removes the directory it
        // is read from. A commit survives that; uncommitted changes do not, which is why `uncommitted`
        // travels with it — the parent should be able to say "it left work behind" without guessing.
        let work = job_worktree(row).map(|wt| {
            let found = worktree::collect(&wt);
            spawn::ReceiptWork {
                branch: found.branch,
                commit: found.commit,
                touched: found.touched,
                uncommitted: found.dirty,
            }
        });
        let receipt = SpawnReceipt {
            schema_version: spawn::RECEIPT_SCHEMA_VERSION,
            request_id: launch.request_id.clone(),
            state,
            claimed_by: Some(format!("pid:{}", std::process::id())),
            session: Some(receipt_session(row)),
            launch_state: Some(LaunchState::Started),
            error,
            next_action: None,
            args_hash: Some(launch.args_hash.clone()),
            result,
            work: work.clone(),
            updated_at: SystemClock.now(),
        };
        // The receipt goes where every other one for this request went: the PARENT's requests
        // directory. A parent that has since left the session list has nowhere to be answered, and the
        // row is retired regardless — the work is over either way.
        if let Some(dir) = self.parent_requests_dir(row)
            && let Err(error) = spawn::write_receipt(&dir, &receipt)
        {
            self.log_line(&format!(
                "spawn: could not record {}'s result ({error:#})",
                row.id
            ));
        }

        let summary = receipt
            .result
            .as_ref()
            .map(|found| found.summary.clone())
            .or_else(|| receipt.error.as_ref().map(|e| e.message.clone()))
            .unwrap_or_default();
        let headline = summary.lines().next().unwrap_or("").trim().to_string();
        if state == ReceiptState::Done {
            // UNCOMMITTED WORK OUTRANKS RETIREMENT. Everything else about a job is in the receipt above,
            // but changes the child never committed exist only in its worktree — and retiring the row
            // deletes that. A `done` job that ignored its one instruction keeps its row instead, so the
            // work is still there for a person.
            if work.as_ref().is_some_and(|found| found.uncommitted) {
                // (The same question `job_left_uncommitted_work` answers, from the work already read.)
                self.disable_row(&row.id);
                self.status = format!(
                    "{} \u{b7} done, with uncommitted work in its worktree \u{2014} {headline}",
                    row.id
                );
                self.record_status();
                return;
            }
            self.remove_project(&row.id, &session_name(&row.id, &row.root));
            self.status = match work.as_ref().and_then(|found| found.commit.as_deref()) {
                Some(commit) => format!(
                    "retired {} \u{b7} done \u{2014} {headline} (commit {} on {})",
                    row.id,
                    &commit[..commit.len().min(8)],
                    work.as_ref()
                        .map(|found| found.branch.as_str())
                        .unwrap_or("")
                ),
                None => format!("retired {} \u{b7} done \u{2014} {headline}", row.id),
            };
            self.record_status();
            return;
        }
        // KEPT, and inert: its process is gone, so the row exists to be seen and cleared with `d`. A
        // codex row also learns its thread id here — codex has no caller-chosen conversation id, so the
        // only record of which conversation this run created is the one it announced in its own stream.
        // Without this the conversation of a job that failed, or asked for a person, dies with the row.
        self.pin_codex_thread(row);
        self.disable_row(&row.id);
        self.status = format!(
            "{} \u{b7} {} \u{2014} {headline}",
            row.id,
            job_state_word(state)
        );
        self.record_status();
    }

    /// Record the thread id a codex run announced, when its row does not already name a conversation.
    ///
    /// A no-op for claude (its id is minted before the launch and pinned then), for a row that already
    /// has one, and for a run whose stream never said. Failing to write it is not worth failing a
    /// harvest over: the outcome is already recorded, and this only decides whether a human can pick the
    /// conversation up.
    fn pin_codex_thread(&mut self, row: &ProjectEntry) {
        if row.engine != Some(Engine::Codex) || row.conversation_id.is_some() {
            return;
        }
        let log = std::fs::read_to_string(entry_state_paths(row).job_log()).unwrap_or_default();
        let Some(thread) = spawn::codex_thread_id(&log) else {
            return;
        };
        let Ok(mut reg) = Registry::load(&self.registry_path) else {
            return;
        };
        if let Some(entry) = reg.projects.iter_mut().find(|p| p.id == row.id) {
            entry.conversation_id = Some(thread);
            let _ = reg.save(&self.registry_path);
        }
    }

    /// Answer a job's parent when a HUMAN removes the row with `d`: the job was stopped deliberately,
    /// so its receipt goes `cancelled`.
    ///
    /// Without this a parent polls `ready` for a child that no longer exists — the one way this feature
    /// could leave an agent waiting forever. A no-op for a chat child and for a job whose own outcome is
    /// already recorded.
    pub(crate) fn cancel_job_receipt(&mut self, row: &ProjectEntry) {
        self.write_cancel_receipt(
            row,
            &format!("{} was stopped and removed by a human", row.id),
        );
    }

    /// Write a job's `cancelled` receipt, `reason` naming who stopped it. Shared by the human's `d` and
    /// the parent's cancel marker, so both routes leave the same record.
    fn write_cancel_receipt(&mut self, row: &ProjectEntry, reason: &str) {
        if !row.is_job() {
            return;
        }
        let Some(launch) = row.launch.as_ref() else {
            return;
        };
        let Some(dir) = self.parent_requests_dir(row) else {
            return;
        };
        if spawn::read_receipt(&dir, &launch.request_id)
            .ok()
            .flatten()
            .is_some_and(|found| found.state.is_job_terminal())
        {
            return;
        }
        let receipt = SpawnReceipt {
            schema_version: spawn::RECEIPT_SCHEMA_VERSION,
            request_id: launch.request_id.clone(),
            state: ReceiptState::Cancelled,
            claimed_by: Some(format!("pid:{}", std::process::id())),
            session: Some(receipt_session(row)),
            launch_state: Some(launch.state),
            error: Some(SpawnError {
                code: ErrorCode::JobFailed,
                message: reason.to_string(),
            }),
            next_action: None,
            args_hash: Some(launch.args_hash.clone()),
            result: None,
            work: None,
            updated_at: SystemClock.now(),
        };
        if let Err(error) = spawn::write_receipt(&dir, &receipt) {
            self.log_line(&format!(
                "spawn: could not record {}'s cancellation ({error:#})",
                row.id
            ));
        }
    }

    /// The `spawn-requests/` directory of `row`'s parent, or `None` when that parent is no longer in
    /// the session list.
    fn parent_requests_dir(&self, row: &ProjectEntry) -> Option<PathBuf> {
        let parent = row.spawned_by.as_deref()?;
        let reg = Registry::load(&self.registry_path).ok()?;
        let entry = reg.projects.iter().find(|p| p.id == parent)?;
        Some(spawn::requests_dir(&entry_state_paths(entry).state_dir()))
    }

    /// Mark a finished job's row paused, so nothing tries to drive what is already over. Best effort:
    /// the outcome is already in the receipt, which is the durable record.
    fn disable_row(&mut self, id: &str) {
        let Ok(mut reg) = Registry::load(&self.registry_path) else {
            return;
        };
        if let Some(entry) = reg.projects.iter_mut().find(|p| p.id == id) {
            entry.enabled = false;
            let _ = reg.save(&self.registry_path);
        }
    }

    /// Start a job child: write the result schema, build the one-shot argv, and hand it to
    /// [`tmux::Driver::spawn_step`], whose wrapper tees the run's combined output into `job.log` and
    /// lands its exit code in `job.done`.
    ///
    /// Reported as a [`LaunchOutcome`] so the caller's outcome mapping is the same for both kinds. A
    /// failure here is `NewSessionFailed`, which the caller treats as AMBIGUOUS: `spawn_step` can fail
    /// after tmux has already been handed the argv, and a Message must never be sent twice.
    fn launch_job_step(
        &mut self,
        row: &ProjectEntry,
        paths: &ProjectPaths,
        engine: Engine,
        session: &str,
        conversation: &str,
    ) -> std::result::Result<LaunchOutcome, agent_manager::tmux::LaunchError> {
        let schema = paths.job_schema();
        if let Some(dir) = schema.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            return Err(agent_manager::tmux::LaunchError::NewSessionFailed(format!(
                "could not create {}: {error}",
                dir.display()
            )));
        }
        if let Err(error) = state::write_text_atomic(&schema, &spawn::result_schema_json()) {
            return Err(agent_manager::tmux::LaunchError::NewSessionFailed(format!(
                "could not write the job schema: {error:#}"
            )));
        }
        // ITS OWN CHECKOUT, when there is a repository to branch from. Children run at the same time in
        // the same project, so a shared working tree means they overwrite each other; a worktree gives
        // each one its own files and its own branch, and the branch is what carries the work back. A
        // project that is not a repository keeps the old behaviour and runs in the project directory.
        let worktree = row
            .launch
            .as_ref()
            .and_then(|launch| self.prepare_worktree(row, paths, &launch.request_id.clone()));
        let cwd = worktree
            .as_ref()
            .map(|wt| wt.path.clone())
            .unwrap_or_else(|| row.root.clone());
        let argv = agent_manager::worker::build_job_command(
            engine,
            &spawn::job_prompt(
                row.initial_prompt.as_deref().unwrap_or_default(),
                worktree.as_ref().map(|wt| wt.branch.as_str()),
            ),
            row.worker_model.as_deref(),
            conversation,
            &schema,
            &paths.job_last_message(),
        );
        match self.agent_tmux.spawn_step(
            session,
            &cwd,
            &argv,
            &paths.job_done_signal(),
            &paths.job_log(),
        ) {
            Ok(_handle) => Ok(LaunchOutcome::Started),
            Err(error) => Err(agent_manager::tmux::LaunchError::NewSessionFailed(format!(
                "{error:#}"
            ))),
        }
    }

    /// Create this job's worktree, and record its branch and base on the row.
    ///
    /// `None` when the project is not a git repository, when the worktree cannot be made, or when this
    /// launch already made one (a relaunch reuses it). A failure is logged and the job runs in the project
    /// directory instead: isolation is worth having, but not worth refusing to do the work over.
    pub(crate) fn prepare_worktree(
        &mut self,
        row: &ProjectEntry,
        paths: &ProjectPaths,
        request_id: &str,
    ) -> Option<worktree::JobWorktree> {
        if let Some(existing) = job_worktree(row) {
            return Some(existing);
        }
        if !worktree::is_repo(&row.root) {
            return None;
        }
        let branch = worktree::branch_name(&row.id, request_id);
        let at = worktree_path(paths);
        match worktree::create(&row.root, &at, &branch) {
            Ok(created) => {
                let base = created.base.clone();
                let branch = created.branch.clone();
                // THE ROW IS HOW ANYONE FINDS THIS AGAIN. Without the branch and base on it, the harvest
                // cannot read what the child did and removal cannot clean the directory up, so a worktree
                // whose identity could not be recorded is REMOVED and the job runs in the project
                // directory — an unreachable checkout is worse than a shared one.
                if !self.record_worktree(&row.id, &branch, &base) {
                    let _ = worktree::prune(&row.root, &created);
                    self.log_line(&format!(
                        "spawn: {} runs in the project directory (its worktree could not be recorded)",
                        row.id
                    ));
                    return None;
                }
                Some(created)
            }
            Err(error) => {
                self.log_line(&format!(
                    "spawn: {} runs in the project directory ({error:#})",
                    row.id
                ));
                None
            }
        }
    }

    /// Write a job's branch and base commit onto its row, reporting whether they are now on disk.
    ///
    /// Both halves have to land: a row that names neither is a worktree nobody can find again.
    fn record_worktree(&mut self, id: &str, branch: &str, base: &str) -> bool {
        let Ok(mut reg) = Registry::load(&self.registry_path) else {
            return false;
        };
        let Some(launch) = reg
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .and_then(|entry| entry.launch.as_mut())
        else {
            return false;
        };
        launch.branch = Some(branch.to_string());
        launch.base_commit = Some(base.to_string());
        reg.save(&self.registry_path).is_ok()
    }

    /// Launch the staged row once, under its `driver.lock` like interactive New, with `attempted`
    /// persisted first, then map the typed outcome: `Started` is promoted, a proven pre-start
    /// failure is discarded, and anything ambiguous is reported and never launched again.
    fn launch_child(&mut self, job: &SpawnJob, child: SpawnChild, row: ProjectEntry) -> SpawnPhase {
        let paths = entry_state_paths(&row);
        let lease = match lease::try_acquire(&paths.daemon_dir().join("driver.lock")) {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                self.status = format!(
                    "spawn: {} is claimed by another driver; launching it on a later frame",
                    child.id
                );
                return SpawnPhase::Staged { child };
            }
            Err(error) => {
                let reason = format!("could not claim it ({error})");
                return self.discard_unstarted(job, child, &row, LaunchState::Pending, reason);
            }
        };
        // The conversation a Claude launch creates is saved with the attempt, before the launch,
        // so a dashboard that stops at any later point leaves a row that names it.
        let engine = row.engine.unwrap_or(Engine::Claude);
        let conversation = job_engine::mint_uuid_v4();
        let pinned = (engine == Engine::Claude).then(|| conversation.clone());
        if let Err(error) = self.update_child(job, &child, Some(LaunchState::Pending), |entry| {
            set_launch_state(entry, LaunchState::Attempted);
            if pinned.is_some() {
                entry.conversation_id = pinned;
            }
        }) {
            return self.wait_for_registry(job, SpawnPhase::Staged { child }, &error);
        }
        self.put_receipt(
            job,
            job.row_receipt(&row, ReceiptState::Launching, LaunchState::Attempted),
        );
        let session = child.session();
        let job_kind = row
            .launch
            .as_ref()
            .is_some_and(|launch| launch.kind == LaunchKind::Job);
        let launched = if job_kind {
            // A JOB: one headless run that reports a result and exits, wrapped by `spawn_step` so its
            // combined output is tee'd and its exit code lands in a done-signal. No `ManagedEnv` and
            // no spawn skill — a job cannot spawn, by construction rather than by a check.
            self.launch_job_step(&row, &paths, engine, &session, &conversation)
        } else {
            // A CHAT child, as v1 staged: the persistent interactive agent a human talks to.
            let argv = fresh_standard_argv(
                engine,
                &conversation,
                row.worker_model.as_deref(),
                &paths.turn_signal(),
                row.initial_prompt.as_deref(),
            );
            let env = self.managed_env(&row.id, &row.root);
            crate::session::install_spawn_skill(&row.root, engine, &env);
            self.agent_tmux
                .launch_interactive(&session, &row.root, &argv, &env)
        };
        drop(lease);
        match launched {
            Ok(LaunchOutcome::Started) => self.promote(job, child, &row),
            Err(error) if error.proven_not_started() => {
                self.discard_unstarted(job, child, &row, LaunchState::Attempted, error.to_string())
            }
            Ok(LaunchOutcome::AlreadyAlive) => self.finish_attempted(
                job,
                &child,
                SpawnError {
                    code: ErrorCode::LaunchFailed,
                    message: format!(
                        "a terminal named {session} was already running, so this launch was not used"
                    ),
                },
            ),
            Err(error) => self.finish_attempted(
                job,
                &child,
                SpawnError {
                    code: ErrorCode::LaunchFailed,
                    message: error.to_string(),
                },
            ),
        }
    }

    /// Continue an unfinished row from its launch state, which is also the recovery table for a
    /// row a previous dashboard left: `pending` launches (on the frame after the pre-check, which
    /// only re-reports it as staged); `attempted` with a live terminal is promoted, and without
    /// one is `outcome_unknown` and never relaunched; `started` finishes readiness; and
    /// `failed_before_start` is discarded.
    fn recover_row(
        &mut self,
        job: &SpawnJob,
        child: SpawnChild,
        row: ProjectEntry,
        state: LaunchState,
        launch_now: bool,
    ) -> SpawnPhase {
        match state {
            LaunchState::Pending if job.orphan => {
                let reason = "its spawn request is gone, and nothing launched it".to_string();
                self.discard_unstarted(job, child, &row, LaunchState::Pending, reason)
            }
            LaunchState::Pending if launch_now => self.launch_child(job, child, row),
            LaunchState::Pending => {
                self.put_receipt(
                    job,
                    job.row_receipt(&row, ReceiptState::Staged, LaunchState::Pending),
                );
                SpawnPhase::Staged { child }
            }
            LaunchState::Attempted => {
                // This dashboard's claim, written before it changes the row.
                self.put_receipt(
                    job,
                    job.row_receipt(&row, ReceiptState::Launching, LaunchState::Attempted),
                );
                if self
                    .fork_terminal_running(&child.session())
                    .unwrap_or(false)
                {
                    return self.promote(job, child, &row);
                }
                let message = format!(
                    "a dashboard stopped before it confirmed {}'s launch, and its terminal is \
                     gone; its Message may have been read, so it is not sent again",
                    child.id
                );
                self.finish_attempted(
                    job,
                    &child,
                    SpawnError {
                        code: ErrorCode::ReadinessUnknown,
                        message,
                    },
                )
            }
            LaunchState::Started => {
                self.put_receipt(
                    job,
                    job.row_receipt(&row, ReceiptState::Launching, LaunchState::Started),
                );
                self.readiness(child)
            }
            LaunchState::FailedBeforeStart => self.remove_unstarted(
                job,
                child,
                &row,
                "an earlier launch proved it never started".into(),
            ),
        }
    }

    /// `attempted` → `started` and enabled, then readiness. This promotion, and
    /// [`App::finish_attempted`] for an attempt that can never be proven, are the only places the
    /// broker enables a row. A write that fails leaves `attempted` with a live terminal, which the
    /// next frame promotes the same way.
    fn promote(&mut self, job: &SpawnJob, child: SpawnChild, row: &ProjectEntry) -> SpawnPhase {
        let promoted = self.update_child(job, &child, Some(LaunchState::Attempted), |entry| {
            entry.enabled = true;
            set_launch_state(entry, LaunchState::Started);
        });
        match promoted {
            Ok(_) => {
                self.put_receipt(
                    job,
                    job.row_receipt(row, ReceiptState::Launching, LaunchState::Started),
                );
                self.readiness(child)
            }
            Err(error) => self.wait_for_registry(job, SpawnPhase::Staged { child }, &error),
        }
    }

    fn readiness(&self, child: SpawnChild) -> SpawnPhase {
        SpawnPhase::Readiness {
            child,
            first_ok_ms: None,
            deadline_ms: (self.spawn.now_ms)() + READINESS_WINDOW_MS,
        }
    }

    /// A launch proven never to have started: persist `failed_before_start` first, then remove.
    fn discard_unstarted(
        &mut self,
        job: &SpawnJob,
        child: SpawnChild,
        row: &ProjectEntry,
        from: LaunchState,
        reason: String,
    ) -> SpawnPhase {
        match self.update_child(job, &child, Some(from), |entry| {
            set_launch_state(entry, LaunchState::FailedBeforeStart)
        }) {
            Ok(_) => self.remove_unstarted(job, child, row, reason),
            Err(error) => self.wait_for_registry(job, SpawnPhase::Staged { child }, &error),
        }
    }

    /// Remove a `failed_before_start` row and its exact reserved directory, as a failed fork
    /// does, revalidating the row id, request id, hash and state inside the registry write. The
    /// receipt records the proof before the removal, so a dashboard that dies in between knows
    /// nothing ran. Retrying would fail the same way, so the receipt sends the agent to the human.
    fn remove_unstarted(
        &mut self,
        job: &SpawnJob,
        child: SpawnChild,
        row: &ProjectEntry,
        reason: String,
    ) -> SpawnPhase {
        self.put_receipt(
            job,
            job.row_receipt(row, ReceiptState::Launching, LaunchState::FailedBeforeStart),
        );
        self.spawn.dirty = true;
        let removed = self.discard_staged_row(&child.id, &child.root, |entry| {
            job.is_row(entry, &child)
                && !entry.enabled
                && launch_state(entry) == Some(LaunchState::FailedBeforeStart)
        });
        if let Err(kept) = removed {
            self.status = format!("spawn: could not discard {} ({kept}); will retry", child.id);
            return SpawnPhase::Staged { child };
        }
        let message = format!(
            "{} did not start: {reason}; nothing ran — tell the human, who can fix the cause and ask again",
            child.id
        );
        self.put_receipt(
            job,
            SpawnReceipt {
                launch_state: Some(LaunchState::FailedBeforeStart),
                error: Some(SpawnError {
                    code: ErrorCode::LaunchFailed,
                    message: message.clone(),
                }),
                ..job.receipt(ReceiptState::Failed)
            },
        );
        self.status = format!("spawn request from {} failed: {message}", job.parent);
        SpawnPhase::Done
    }

    /// One readiness observation. Two live, dialog-free observations at least
    /// [`READINESS_GAP_MS`] apart are `ready`; a dialog is `needs_attention` and is never
    /// answered; a probe error, or no proof by the deadline, is `outcome_unknown`. Once a live
    /// observation is recorded, the deadline is at least [`READINESS_AFTER_FIRST_MS`] after it.
    fn check_readiness(
        &mut self,
        job: &SpawnJob,
        child: SpawnChild,
        first_ok_ms: Option<u128>,
        deadline_ms: u128,
    ) -> SpawnPhase {
        let now = (self.spawn.now_ms)();
        let first_ok_ms = match self.observe_child(&child.session()) {
            Err(error) => {
                let message = format!("could not observe {}'s terminal ({error:#})", child.id);
                return self.finish_unknown(job, &child, message);
            }
            Ok(ChildTerminal::Dialog) => {
                return self.finish(
                    job,
                    &child,
                    SpawnOutcome::NeedsAttention,
                    LaunchState::Started,
                    None,
                );
            }
            Ok(ChildTerminal::Live) => match first_ok_ms {
                Some(first) if now.saturating_sub(first) >= READINESS_GAP_MS => {
                    return self.finish(
                        job,
                        &child,
                        SpawnOutcome::Ready,
                        LaunchState::Started,
                        None,
                    );
                }
                Some(first) => Some(first),
                None => Some(now),
            },
            Ok(ChildTerminal::NotLive) => None,
        };
        let deadline_ms = match first_ok_ms {
            Some(first) => deadline_ms.max(first + READINESS_AFTER_FIRST_MS),
            None => deadline_ms,
        };
        if now >= deadline_ms {
            let message = format!(
                "{} did not show a live terminal within {} s",
                child.id,
                READINESS_WINDOW_MS / 1000
            );
            return self.finish_unknown(job, &child, message);
        }
        SpawnPhase::Readiness {
            child,
            first_ok_ms,
            deadline_ms,
        }
    }

    fn observe_child(&self, session: &str) -> Result<ChildTerminal> {
        if !self.fork_terminal_running(session)? {
            return Ok(ChildTerminal::NotLive);
        }
        let capture = self
            .agent_tmux
            .capture_tail(session, READINESS_CAPTURE_LINES)?;
        Ok(match tmux::classify_dialog(&capture) {
            Some(_) => ChildTerminal::Dialog,
            None => ChildTerminal::Live,
        })
    }

    fn finish_unknown(
        &mut self,
        job: &SpawnJob,
        child: &SpawnChild,
        message: String,
    ) -> SpawnPhase {
        self.finish(
            job,
            child,
            SpawnOutcome::OutcomeUnknown,
            LaunchState::Started,
            Some(SpawnError {
                code: ErrorCode::ReadinessUnknown,
                message,
            }),
        )
    }

    /// Record the final `outcome` on the row, write the final receipt, and report the child in
    /// the status line. Never changes `enabled`: a started child was enabled when it was
    /// promoted, and a human may have paused it since.
    fn finish(
        &mut self,
        job: &SpawnJob,
        child: &SpawnChild,
        outcome: SpawnOutcome,
        launch_state: LaunchState,
        error: Option<SpawnError>,
    ) -> SpawnPhase {
        self.record_outcome(job, child, outcome, launch_state, error, false)
    }

    /// An `attempted` launch whose result can never be proven, at launch or at recovery: the row
    /// leaves the staged state enabled, so a human sees and inspects it, with `outcome_unknown`.
    /// Its Message is never sent again.
    fn finish_attempted(
        &mut self,
        job: &SpawnJob,
        child: &SpawnChild,
        error: SpawnError,
    ) -> SpawnPhase {
        self.record_outcome(
            job,
            child,
            SpawnOutcome::OutcomeUnknown,
            LaunchState::Attempted,
            Some(error),
            true,
        )
    }

    fn record_outcome(
        &mut self,
        job: &SpawnJob,
        child: &SpawnChild,
        outcome: SpawnOutcome,
        launch_state: LaunchState,
        error: Option<SpawnError>,
        enable: bool,
    ) -> SpawnPhase {
        let row = self
            .update_child(job, child, None, |entry| {
                entry.enabled |= enable;
                entry.launch = entry.launch.take().map(|launch| LaunchRecord {
                    outcome: Some(outcome),
                    ..launch
                });
            })
            .ok();
        let detail = error.as_ref().map(|error| error.message.clone());
        let receipt = self.final_receipt(job, child, row.as_ref(), outcome, launch_state, error);
        self.put_receipt(job, receipt);
        let spawned = format!("dashboard spawned {} from {}", child.id, job.parent);
        self.status = match (outcome, detail) {
            (SpawnOutcome::Ready, _) => spawned,
            (SpawnOutcome::NeedsAttention, _) => {
                format!("{spawned}; it needs you at its terminal (Enter)")
            }
            (SpawnOutcome::OutcomeUnknown, detail) => format!(
                "{spawned}, but its launch outcome is unknown: {}",
                detail.unwrap_or_default()
            ),
        };
        SpawnPhase::Done
    }

    /// The final receipt for `outcome`: every state but `ready` carries the exact attach argv.
    fn final_receipt(
        &self,
        job: &SpawnJob,
        child: &SpawnChild,
        row: Option<&ProjectEntry>,
        outcome: SpawnOutcome,
        launch_state: LaunchState,
        error: Option<SpawnError>,
    ) -> SpawnReceipt {
        let (state, error) = match outcome {
            SpawnOutcome::Ready => (ReceiptState::Ready, error),
            SpawnOutcome::NeedsAttention => (ReceiptState::NeedsAttention, error),
            SpawnOutcome::OutcomeUnknown => (
                ReceiptState::OutcomeUnknown,
                error.or_else(|| {
                    Some(SpawnError {
                        code: ErrorCode::ReadinessUnknown,
                        message: format!(
                            "{}'s launch outcome is unknown; its Message may have been read, so \
                             do not spawn a replacement",
                            child.id
                        ),
                    })
                }),
            ),
        };
        SpawnReceipt {
            session: row.map(receipt_session),
            launch_state: Some(launch_state),
            error,
            next_action: (outcome != SpawnOutcome::Ready)
                .then(|| self.attach_action(&child.session())),
            ..job.receipt(state)
        }
    }

    /// A final `failed` receipt naming no session: nothing was launched for this request.
    fn fail(&mut self, job: &SpawnJob, code: ErrorCode, message: String) -> SpawnPhase {
        let status = format!("spawn request from {} failed: {message}", job.parent);
        self.put_receipt(
            job,
            SpawnReceipt {
                error: Some(SpawnError { code, message }),
                ..job.receipt(ReceiptState::Failed)
            },
        );
        self.status = status;
        SpawnPhase::Done
    }

    /// Keep `phase` for the next frame while the session list cannot be read or written, or the
    /// row no longer matches what this phase expects; the next frame re-reads it.
    fn wait_for_registry(
        &mut self,
        job: &SpawnJob,
        phase: SpawnPhase,
        error: &anyhow::Error,
    ) -> SpawnPhase {
        self.status = format!("spawn request from {}: {error:#}; will retry", job.parent);
        phase
    }

    /// Change exactly the row this job staged as `child`, and only while its launch state is
    /// `expect` (any state for `None`), inside one registry write. Returns the changed row; a
    /// session list that cannot be read or written, or no matching row, is an error.
    fn update_child(
        &mut self,
        job: &SpawnJob,
        child: &SpawnChild,
        expect: Option<LaunchState>,
        change: impl FnOnce(&mut ProjectEntry),
    ) -> Result<ProjectEntry> {
        let mut changed = None;
        Registry::update(&self.registry_path, |reg| {
            changed = reg
                .projects
                .iter_mut()
                .find(|entry| {
                    job.is_row(entry, child)
                        && expect.is_none_or(|state| launch_state(entry) == Some(state))
                })
                .map(|entry| {
                    change(entry);
                    entry.clone()
                });
        })
        .context("the session list is unusable")?;
        self.spawn.dirty = true;
        changed.with_context(|| format!("{} no longer matches this request", child.id))
    }

    /// Write a receipt through [`spawn::write_receipt`], the only way the broker writes one. A
    /// receipt that cannot be written is reported; the row stays the truth, and a later pre-check
    /// rebuilds the receipt from it.
    fn put_receipt(&mut self, job: &SpawnJob, receipt: SpawnReceipt) {
        if let Err(error) = write_job_receipt(job, &receipt) {
            self.status = format!(
                "spawn request from {}: could not write the receipt of {} ({error:#})",
                job.parent,
                job.id()
            );
        }
    }

    /// The exact argv that attaches a human to `session` on this dashboard's tmux server.
    fn attach_action(&self, session: &str) -> NextAction {
        let path = std::env::var_os("PATH");
        NextAction {
            kind: NextActionKind::Attach,
            argv: vec![
                tmux_program(path.as_deref()),
                "-L".into(),
                self.socket.clone(),
                "attach-session".into(),
                "-t".into(),
                format!("={session}"),
            ],
        }
    }

    fn has_job(&self, parent: &str, request_id: &str) -> bool {
        self.spawn
            .jobs
            .iter()
            .any(|job| job.parent == parent && job.request.request_id == request_id)
    }

    /// Answer a validly named entry whose content is unusable, and whose receipt is not final,
    /// with `invalid_request`, unless a queued request owns the id or a row already names it: a
    /// child that exists is never reported as failed, which would invite a replacement.
    fn answer_invalid(
        &mut self,
        reg: &Registry,
        owner: &str,
        dir: &Path,
        request_id: &str,
        reason: &str,
        now: Epoch,
    ) {
        let claimed = reg.projects.iter().any(|entry| {
            entry.spawned_by.as_deref() == Some(owner)
                && entry
                    .launch
                    .as_ref()
                    .is_some_and(|launch| launch.request_id == request_id)
        });
        if claimed || self.has_job(owner, request_id) {
            return;
        }
        let receipt = SpawnReceipt {
            schema_version: spawn::RECEIPT_SCHEMA_VERSION,
            request_id: request_id.to_string(),
            state: ReceiptState::Failed,
            claimed_by: Some(format!("pid:{}", std::process::id())),
            session: None,
            launch_state: None,
            error: Some(SpawnError {
                code: ErrorCode::InvalidRequest,
                message: reason.to_string(),
            }),
            next_action: None,
            args_hash: None,
            result: None,
            work: None,
            updated_at: now,
        };
        if let Err(error) = spawn::write_receipt(dir, &receipt) {
            self.note_once(
                format!("{}/{request_id}", dir.display()),
                format!("spawn: could not answer invalid request {request_id} ({error:#})"),
            );
        }
    }

    /// Remove the request file whose receipt has been final for longer than
    /// [`spawn::RETENTION_SECS`]. The receipt stays for good as the request's tombstone, so a
    /// replay of the id finds its final answer and its `args_hash` even after a human deleted the
    /// child's row.
    fn retire(&mut self, dir: &Path, id: &str, final_at: Epoch, now: Epoch) {
        if now.saturating_sub(final_at) > spawn::RETENTION_SECS
            && let Err(error) = spawn::remove_request(dir, id)
        {
            self.note_once(
                format!("{}/{id}", dir.display()),
                format!("spawn: could not remove the expired request {id} ({error:#})"),
            );
        }
    }

    /// Append `line` to the status log once per `key`, without replacing the status a key
    /// handler may have just set.
    fn note_once(&mut self, key: String, line: String) {
        if self.spawn.noted.insert(key) {
            self.log_line(&line);
        }
    }
}

/// Why a request cannot be staged right now: because it never can, or because the parent is full.
enum PlanRefusal {
    /// Answer the parent with this error. Nothing will change by waiting.
    Refused(SpawnError),
    /// THE PARENT IS AT ITS CONCURRENCY LIMIT, so this request WAITS. The limit is how many children run
    /// at once, not how many a parent may ever ask for: refusing the sixth made an agent choose between
    /// doing that work itself and bothering a human, when the honest answer is "shortly". Queue depth is
    /// bounded by [`spawn::MAX_UNFINISHED_PER_PARENT`], which discovery already enforces.
    NotYet,
}

impl From<SpawnError> for PlanRefusal {
    fn from(error: SpawnError) -> Self {
        PlanRefusal::Refused(error)
    }
}

/// Validate a request against the registry and resolve its defaults from the parent row.
fn plan_child(reg: &Registry, request: &SpawnRequest) -> Result<ChildPlan, PlanRefusal> {
    let args = request.args.normalized();
    let parent = reg
        .projects
        .iter()
        .find(|entry| entry.id == request.parent_session)
        .ok_or_else(|| {
            PlanRefusal::from(refusal(
                ErrorCode::ParentNotFound,
                format!(
                    "parent session {} is not in the session list",
                    request.parent_session
                ),
            ))
        })?;
    if let Some(grandparent) = parent.spawned_by.as_deref() {
        return Err(refusal(
            ErrorCode::NestedSpawnRefused,
            format!(
                "{} was spawned by {grandparent}, and a spawned session cannot spawn",
                parent.id
            ),
        )
        .into());
    }
    if reg.children_of(&parent.id) >= MAX_CHILDREN_PER_PARENT {
        return Err(PlanRefusal::NotYet);
    }
    let dir = resolve_dir(args.dir.as_deref(), &parent.root, reg)?;
    let parent_engine = parent.engine.unwrap_or(Engine::Claude);
    let engine = args.agent.unwrap_or(parent_engine);
    let model = match args.model.clone() {
        Some(model) => Some(model),
        None if engine == parent_engine => parent.worker_model.clone(),
        None => None,
    };
    let title = child_title(&args)?;
    let name = agent_manager::registry::normalize_display_name(args.name.as_deref().unwrap_or(""))
        .map_err(|error| refusal(ErrorCode::InvalidArgument, format!("name: {error}")))?;
    let bytes = fresh_launch_bytes(reg, &dir, engine, model.as_deref(), &args.message);
    if bytes > tmux::LAUNCH_COMMAND_MAX_BYTES {
        return Err(refusal(
            ErrorCode::MessageTooLong,
            format!(
                "the Message's launch command is {bytes} bytes after shell quoting; tmux accepts \
                 {} — shorten it by at least {} bytes",
                tmux::LAUNCH_COMMAND_MAX_BYTES,
                bytes - tmux::LAUNCH_COMMAND_MAX_BYTES
            ),
        )
        .into());
    }
    Ok(ChildPlan {
        dir,
        engine,
        model,
        title,
        name,
        message: args.message,
    })
}

/// The directory the child runs in: the request's (absolute and existing) or the parent's root,
/// canonicalized like New's. An agent names it, so it is confined after canonicalizing (a symlink
/// cannot lead out): it must lie inside the parent's canonical root, or be exactly the canonical
/// root of a session already in the list. Anything else is `dir_not_allowed`, so a request can
/// never make the dashboard write session state into an arbitrary folder such as `$HOME`.
fn resolve_dir(
    requested: Option<&Path>,
    parent_root: &Path,
    reg: &Registry,
) -> Result<PathBuf, SpawnError> {
    let dir = requested.unwrap_or(parent_root);
    if !dir.is_absolute() {
        return Err(refusal(
            ErrorCode::InvalidArgument,
            format!("dir {} is not an absolute path", dir.display()),
        ));
    }
    let canonical = std::fs::canonicalize(dir)
        .ok()
        .filter(|dir| dir.is_dir())
        .ok_or_else(|| {
            refusal(
                ErrorCode::DirNotFound,
                format!("{} is not an existing directory", dir.display()),
            )
        })?;
    let canonical_root = |root: &Path| std::fs::canonicalize(root).ok();
    let inside_parent = canonical_root(parent_root).is_some_and(|root| canonical.starts_with(root));
    let a_session_root = reg
        .projects
        .iter()
        .any(|row| canonical_root(&row.root).as_deref() == Some(canonical.as_path()));
    if inside_parent || a_session_root {
        return Ok(canonical);
    }
    Err(refusal(
        ErrorCode::DirNotAllowed,
        format!(
            "{} is outside {}, and no session has it as its root",
            canonical.display(),
            parent_root.display()
        ),
    ))
}

/// The child's `task_title`: the request's title, or the Message's first line cut to
/// [`spawn::TITLE_MAX_CHARS`].
fn child_title(args: &SpawnArgs) -> Result<String, SpawnError> {
    if args.message.is_empty() {
        return Err(refusal(
            ErrorCode::InvalidArgument,
            "the Message is empty".into(),
        ));
    }
    match args.title.as_deref() {
        Some(title)
            if title.chars().count() > spawn::TITLE_MAX_CHARS
                || title.chars().any(char::is_control) =>
        {
            Err(refusal(
                ErrorCode::InvalidArgument,
                format!(
                    "the title must be {} characters or fewer, with no control bytes",
                    spawn::TITLE_MAX_CHARS
                ),
            ))
        }
        Some(title) => Ok(title.to_string()),
        None => intent_title(&args.message)
            .map(|title| title.chars().take(spawn::TITLE_MAX_CHARS).collect())
            .ok_or_else(|| {
                refusal(
                    ErrorCode::InvalidArgument,
                    "the Message has no visible text".into(),
                )
            }),
    }
}

/// Write `receipt` into `job`'s requests directory through [`spawn::write_receipt`]. An orphan
/// whose parent left the session list has nowhere to answer, because nothing reads that session's
/// requests any more, so it writes nothing.
fn write_job_receipt(job: &SpawnJob, receipt: &SpawnReceipt) -> Result<()> {
    match job.dir.as_deref() {
        Some(dir) => spawn::write_receipt(dir, receipt),
        None => Ok(()),
    }
}

/// Whether `receipt` proves its request already staged a row: its state is past `claimed`, or it
/// names a session or a launch state.
fn proves_staged(receipt: &SpawnReceipt) -> bool {
    receipt.state != ReceiptState::Claimed
        || receipt.session.is_some()
        || receipt.launch_state.is_some()
}

fn refusal(code: ErrorCode, message: String) -> SpawnError {
    SpawnError { code, message }
}

fn launch_state(entry: &ProjectEntry) -> Option<LaunchState> {
    entry.launch.as_ref().map(|launch| launch.state)
}

fn set_launch_state(entry: &mut ProjectEntry, state: LaunchState) {
    entry.launch = entry
        .launch
        .take()
        .map(|launch| LaunchRecord { state, ..launch });
}

/// What a job child REPORTED, read from what its harness left in the child's own state directory:
/// claude's `result` event in the tee'd `job.log`, or codex's `-o` file. `None` when it reported
/// nothing — it crashed, was killed, or has not got there yet.
/// Where a job's worktree lives: inside its own state directory, so a purge takes it with the row.
fn worktree_path(paths: &ProjectPaths) -> PathBuf {
    paths.state_dir().join("worktree")
}

/// Rebuild a job's worktree handle from its row. `None` for a row whose launch never made one.
pub(crate) fn job_worktree(row: &ProjectEntry) -> Option<worktree::JobWorktree> {
    let launch = row.launch.as_ref()?;
    Some(worktree::JobWorktree {
        path: worktree_path(&entry_state_paths(row)),
        branch: launch.branch.clone()?,
        base: launch.base_commit.clone()?,
    })
}

fn job_result_of(row: &ProjectEntry) -> Option<spawn::JobResult> {
    let paths = entry_state_paths(row);
    let log = std::fs::read_to_string(paths.job_log()).unwrap_or_default();
    let last_message = std::fs::read_to_string(paths.job_last_message()).ok();
    spawn::read_job_result(
        row.engine.unwrap_or(Engine::Claude),
        &log,
        last_message.as_deref(),
    )
}

/// The one word a finished job's status line uses for its outcome.
fn job_state_word(state: ReceiptState) -> &'static str {
    match state {
        ReceiptState::Done => "done",
        ReceiptState::NeedsHuman => "needs a human",
        ReceiptState::Failed => "failed",
        ReceiptState::Cancelled => "cancelled",
        ReceiptState::EndedWithoutResult => "ended without a result",
        // Not reachable from `finish_job_row`, which only writes the five above.
        _ => "finished",
    }
}

/// The child a receipt reports on, from its row. Never carries the Message.
fn receipt_session(row: &ProjectEntry) -> ReceiptSession {
    ReceiptSession {
        id: row.id.clone(),
        title: row.task_title.clone(),
        display_name: row.display_name.clone(),
        root: row.root.clone(),
        agent: row.engine.unwrap_or(Engine::Claude),
        model: row.worker_model.clone(),
        spawned_by: row.spawned_by.clone().unwrap_or_default(),
        tmux_session: session_name(&row.id, &row.root),
        state_dir: entry_state_paths(row).state_dir(),
    }
}

/// The tmux executable a human runs to attach: the first executable `tmux` on `path`, or the
/// bare name when there is none.
pub(crate) fn tmux_program(path: Option<&std::ffi::OsStr>) -> String {
    use std::os::unix::fs::PermissionsExt;
    path.into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("tmux"))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .map(|program| program.display().to_string())
        .unwrap_or_else(|| "tmux".into())
}
