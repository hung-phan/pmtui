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

use agent_manager::registry::{LaunchRecord, LaunchState, MAX_CHILDREN_PER_PARENT};
use agent_manager::spawn::{
    self, ErrorCode, Listed, ReceiptSession, ReceiptState, SpawnArgs, SpawnError, SpawnReceipt,
    SpawnRequest,
};
use agent_manager::worktree;

use super::create::fresh_launch_bytes;
use crate::*;

/// How long a started child has to show a live, dialog-free terminal.
pub(super) const READINESS_WINDOW_MS: u128 = 3_000;
/// The least spacing between the two live observations that prove readiness.
pub(super) const READINESS_GAP_MS: u128 = 100;
/// How long after its first live observation a child still has for the second one, however late
/// that first one came: a frame that stalled past the window must not turn a live child unknown.
pub(super) const READINESS_AFTER_FIRST_MS: u128 = 1_000;
/// Pane lines captured to look for a dialog, the depth the dashboard's activity capture uses.
pub(super) const READINESS_CAPTURE_LINES: usize = 40;

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
    pub(super) dirty: bool,
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
    pub(super) asked_to_stop: HashMap<String, u128>,
    /// A harvest is running. Retiring a row ends in [`App::refresh`], which calls the harvest again,
    /// so without this ONE sweep of five finished children re-enters itself four levels deep, each
    /// level holding a snapshot that still lists rows the deeper ones already retired and purged.
    pub(super) harvesting: bool,
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
pub(super) enum ChildTerminal {
    /// Alive, not dead, and no recognized dialog.
    Live,
    /// Absent or dead: not up (yet).
    NotLive,
    /// A trust, setup or permission dialog only a human may answer.
    Dialog,
}

/// A request that passed validation: the child it describes, defaults resolved from the parent.
pub(super) struct ChildPlan {
    pub(super) dir: PathBuf,
    pub(super) engine: Engine,
    pub(super) model: Option<String>,
    pub(super) title: String,
    pub(super) name: Option<String>,
    pub(super) message: String,
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

    pub(super) fn id(&self) -> &str {
        &self.request.request_id
    }

    /// Whether `entry` was staged by a request with this job's parent and request id, whatever
    /// its arguments were.
    pub(super) fn names(&self, entry: &ProjectEntry) -> bool {
        entry.spawned_by.as_deref() == Some(self.parent.as_str())
            && entry
                .launch
                .as_ref()
                .is_some_and(|launch| launch.request_id == self.request.request_id)
    }

    /// Whether `entry` is exactly the row this job staged as `child`.
    pub(super) fn is_row(&self, entry: &ProjectEntry, child: &SpawnChild) -> bool {
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
    pub(super) fn find_row(
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
    pub(super) fn receipt(&self, state: ReceiptState) -> SpawnReceipt {
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
    pub(super) fn row_receipt(
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
    pub(super) fn of(row: &ProjectEntry) -> Self {
        Self {
            id: row.id.clone(),
            root: row.root.clone(),
        }
    }

    pub(super) fn session(&self) -> String {
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
    pub(super) fn note_once(&mut self, key: String, line: String) {
        if self.spawn.noted.insert(key) {
            self.log_line(&line);
        }
    }
}

/// Why a request cannot be staged right now: because it never can, or because the parent is full.
pub(super) enum PlanRefusal {
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
pub(super) fn plan_child(reg: &Registry, request: &SpawnRequest) -> Result<ChildPlan, PlanRefusal> {
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
pub(super) fn resolve_dir(
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
pub(super) fn child_title(args: &SpawnArgs) -> Result<String, SpawnError> {
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
pub(super) fn write_job_receipt(job: &SpawnJob, receipt: &SpawnReceipt) -> Result<()> {
    match job.dir.as_deref() {
        Some(dir) => spawn::write_receipt(dir, receipt),
        None => Ok(()),
    }
}

/// Whether `receipt` proves its request already staged a row: its state is past `claimed`, or it
/// names a session or a launch state.
pub(super) fn proves_staged(receipt: &SpawnReceipt) -> bool {
    receipt.state != ReceiptState::Claimed
        || receipt.session.is_some()
        || receipt.launch_state.is_some()
}

pub(super) fn refusal(code: ErrorCode, message: String) -> SpawnError {
    SpawnError { code, message }
}

pub(super) fn launch_state(entry: &ProjectEntry) -> Option<LaunchState> {
    entry.launch.as_ref().map(|launch| launch.state)
}

pub(super) fn set_launch_state(entry: &mut ProjectEntry, state: LaunchState) {
    entry.launch = entry
        .launch
        .take()
        .map(|launch| LaunchRecord { state, ..launch });
}

/// What a job child REPORTED, read from what its harness left in the child's own state directory:
/// claude's `result` event in the tee'd `job.log`, or codex's `-o` file. `None` when it reported
/// nothing — it crashed, was killed, or has not got there yet.
/// Where a job's worktree lives: inside its own state directory, so a purge takes it with the row.
pub(super) fn worktree_path(paths: &ProjectPaths) -> PathBuf {
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

pub(super) fn job_result_of(row: &ProjectEntry) -> Option<spawn::JobResult> {
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
pub(super) fn job_state_word(state: ReceiptState) -> &'static str {
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
pub(super) fn receipt_session(row: &ProjectEntry) -> ReceiptSession {
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
