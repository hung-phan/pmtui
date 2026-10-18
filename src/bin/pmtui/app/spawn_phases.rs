//! ONE REQUEST, FROM CLAIMED TO RUNNING: the launch pipeline, a phase per frame.
//!
//! Split out of `spawning.rs`, which had grown to 2,200 lines holding three jobs at once — finding
//! requests, launching them, and harvesting what they left. This half never sleeps and never waits:
//! a phase that cannot proceed (a held lock, an unreadable session list, a child not yet ready)
//! returns and is tried again next frame, which is what keeps the dashboard's event loop responsive
//! while five children come up.
//!
//! Every method stays an inherent method of `App` at the same visibility, so nothing that calls one
//! changed.

use agent_manager::registry::{
    LaunchKind, LaunchRecord, LaunchState, MAX_CHILDREN_PER_PARENT, SpawnOutcome,
};
use agent_manager::spawn::{
    self, ErrorCode, NextAction, NextActionKind, ReceiptState, SpawnError, SpawnReceipt,
};
use agent_manager::tmux::LaunchOutcome;
use agent_manager::worktree;

use super::create::{NewSession, fresh_standard_argv, reserve_new_session};
use super::spawning::{
    ChildTerminal, PlanRefusal, READINESS_AFTER_FIRST_MS, READINESS_CAPTURE_LINES,
    READINESS_GAP_MS, READINESS_WINDOW_MS, SpawnChild, SpawnJob, SpawnPhase, job_worktree,
    launch_state, plan_child, proves_staged, receipt_session, set_launch_state, tmux_program,
    worktree_path, write_job_receipt,
};
use crate::*;

impl App {
    pub(super) fn advance(&mut self, job: &SpawnJob, phase: SpawnPhase) -> SpawnPhase {
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
}
