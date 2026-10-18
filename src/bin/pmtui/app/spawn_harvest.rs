//! WHAT A JOB LEAVES BEHIND: noticing its process is gone, reading what it reported, answering its
//! parent, and cancelling one that was asked to stop.
//!
//! Split out of `spawning.rs` along with the launch pipeline. A JOB'S END IS A FACT — the process is
//! gone — never an inference about idleness, and this is the half that acts on it. Two rules shape
//! everything here and are easier to see in one file: a job is ANSWERED ONCE, and ONE HARVEST RUNS
//! AT A TIME (five children finishing together re-entered this four levels deep through
//! `remove_project` -> `refresh`, and each outer level then overwrote the correct answer with
//! `ended_without_result`).
//!
//! Every method stays an inherent method of `App` at the same visibility, so nothing that calls one
//! changed.

use agent_manager::registry::{LaunchKind, LaunchRecord, LaunchState, SpawnOutcome};
use agent_manager::spawn::{self, ErrorCode, ReceiptState, SpawnError, SpawnReceipt};
use agent_manager::worktree;

use super::spawning::{
    CANCEL_GRACE_MS, job_result_of, job_state_word, job_worktree, receipt_session,
};
use crate::*;

impl App {
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
}
