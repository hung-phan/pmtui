//! The supervisor consult (m20): obtain an independent verdict for one decision typed policy
//! found eligible, by asking a headless `claude -p` what to do — then reap and apply its
//! verdict on a later sweep. Spawning and
//! reaping are one unit precisely because the consult is DETACHED: the tick that
//! asks can never be the tick that hears back, so the in-memory record that ties
//! them together (the nonce, the option list, the deadline) may only be touched
//! here.

use std::path::Path;

use anyhow::Result;

use crate::advise::{self, Consult, Refusal};
use crate::clock::Epoch;
use crate::job::{self, AgentLoopState, AutopilotEventKind, JobRun};
use crate::tmux::{self, Driver};
use crate::worker;

use super::nudge::append_context;
use super::session::mint_uuid_v4;
use super::{JobScheduler, JobTick};

/// Env kill switch for the supervisor (m20, Rule 8). An env var rather than a config
/// field on purpose: it needs no ledger-schema migration and no serde change. Parsed by
/// [`advise::supervisor_enabled`], which defaults it ON.
pub(super) const SUPERVISOR_ENV: &str = "PM_SUPERVISOR";
/// The harness's OWN reap deadline for a supervisor consult, enforced in Rust
/// **independently of `tmux::observe`**.
///
/// Both bounds are needed and neither is redundant. The shell `timeout` (see
/// [`SUPERVISOR_SHELL_TIMEOUT_S`]) is inside the consult's own process tree, so it dies
/// with it; this one is on the harness's side of the fence, so a consult whose session
/// vanished, whose wrapper never ran, or whose done-signal was never written still
/// resolves. A headless `claude -p` was measured hanging for 180s — 3× a 60s cadence —
/// so "no decision this tick" has to be reachable without observing anything.
/// Raised from 90s with the supervisor's tools: a consult that reads files to check a claim
/// takes several turns. The session is PARKED while a consult is in flight, so a longer
/// deadline is a longer park — bounded, and cheaper than escalating to a sleeping human.
pub(super) const SUPERVISOR_TIMEOUT_S: i64 = 210;
/// The shell-level bound (`timeout -k <grace> <secs>`), set deliberately BELOW
/// [`SUPERVISOR_TIMEOUT_S`] so an overrunning consult is normally killed by the shell and
/// observed as an honest non-zero exit (which feeds the latch streak), rather than
/// reaching the harness deadline as an unexplained "no result".
pub(super) const SUPERVISOR_SHELL_TIMEOUT_S: u64 = 180;
/// `timeout`'s SIGKILL grace after its SIGTERM.
pub(super) const SUPERVISOR_KILL_GRACE_S: u64 = 5;
/// How soon to re-check an IN-FLIGHT consult. Short, because the whole point is that the
/// worker's answer arrives on a later sweep rather than blocking this one; the cost is
/// one `has-session` (plus one `stat`) per interval for as long as a consult is running,
/// which is bounded by [`SUPERVISOR_TIMEOUT_S`].
pub(super) const SUPERVISOR_POLL_S: i64 = 2;

/// A supervisor consult the harness spawned and has not yet reaped (m20).
///
/// The reply-validation state (the nonce, the option list, the handle) is in MEMORY,
/// because losing it is what makes a late reply unreachable by construction. But the FACT
/// that the harness owes the worker an answer is DURABLE — see `parked` and
/// [`crate::job::ParkedAdvice`].
///
/// A restart cannot validate the lost reply because the nonce and grant lived in memory.
/// Recovery therefore escalates the audited original question; it never invents an approval
/// or nudges a bare heartbeat past the consumed marker.
pub(super) struct AdviseInFlight {
    /// The detached `pmsup-` session + its done-signal + its tee'd log.
    pub(super) handle: tmux::StepHandle,
    /// What we asked. Keeping the whole [`Consult`] is what lets the reply be validated
    /// against the SAME nonce and the SAME option list it was built from.
    pub(super) consult: Consult,
    /// Where a validated verdict is delivered. Marker advice becomes prose on the
    /// next nudge; dialog advice becomes raw navigation keys after a locked recheck.
    pub(super) target: AdviceTarget,
    /// The harness-side reap deadline ([`SUPERVISOR_TIMEOUT_S`] past the spawn), frozen
    /// once while a human is attached — see `frozen`.
    pub(super) deadline: Epoch,
    /// Whether this consult has already spent its ONE deadline freeze (m23 bug 3).
    ///
    /// `drive`'s human-present gate returns ~44 lines before `advise_step`, so while a
    /// client is attached an in-flight consult is neither polled nor reaped. Without a
    /// freeze the deadline passes unattended and the detach tick reaps it as "the
    /// supervisor produced no result: no answer within 90s" — about a supervisor that
    /// answered in ~0s — which also counts toward `SUPERVISOR_DEAD_AFTER`, so three
    /// routine attaches across a session's life latch the feature off with nothing said to
    /// the user. The freeze is granted AT MOST ONCE rather than sliding for as long as the
    /// human stays, because the same gate precedes the marker disposer: a human who
    /// hand-answers in the pane produces no marker bump, so nothing abandons the consult,
    /// and an unbounded freeze would eventually type an arbitrarily STALE verdict naming a
    /// concrete action the human already took. One grant bounds the staleness at
    /// 2×[`SUPERVISOR_TIMEOUT_S`] past the spawn; past that the consult is reaped into an
    /// escalation carrying the worker's real question (never silently dropped — that would
    /// consume the marker bump and deliver nothing).
    pub(super) frozen: bool,
    /// The DURABLE half: what a restarted daemon needs to honour this consult's debt.
    /// Stamped onto every ledger write by [`JobScheduler::save_ledger`], so it can never
    /// drift from this in-memory record.
    pub(super) parked: job::ParkedAdvice,
    /// Where the reap reads this consult's verdict FROM. `None` ⇒ the claude path: read the
    /// tee'd `handle.log` (a `--output-format json` envelope). `Some(path)` ⇒ the codex path:
    /// read `codex exec --output-last-message <path>`, which holds ONLY the final message, so no
    /// log-scraping. Engine-agnostic at the reap: it just reads this path if set, else the log.
    pub(super) verdict_path: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub(super) enum AdviceTarget {
    Marker {
        stop_ids: Vec<String>,
        report_seq: u64,
    },
    Dialog {
        dialog: tmux::PaneDialog,
        /// Consult option index -> desired original pane-option set. Single
        /// choices map to one index; bounded multi-select combinations map to
        /// multiple. Meta choices remain only in the stale-pane snapshot.
        option_sets: Vec<Vec<usize>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AbandonedAdvice {
    pub(super) decider_seq: u64,
    pub(super) report_seq: Option<u64>,
}

/// What [`JobScheduler::advise_step`] decided about an in-flight consult.
pub(super) enum AdviseStep {
    /// No consult in flight — carry on with the tick unchanged.
    NotEngaged,
    /// A consult is still running (or was just reaped into an escalation): return this
    /// tick as-is and do NOT nudge.
    Yields(JobTick),
    /// A verdict was validated and written to `pending_context`: reload the ledger and
    /// carry on, so the ordinary idle-gated nudge delivers it.
    Applied,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdviceSpawn {
    Spawned(JobTick),
    Empty,
    Unavailable,
    Unconsultable,
}

#[derive(Clone, Copy)]
pub(super) struct DeciderSelection<'a> {
    pub(super) engine: crate::registry::Engine,
    pub(super) model: Option<&'a str>,
    pub(super) reported_kind: Option<crate::pmstate::StopKind>,
    pub(super) effect: Option<crate::worker::StopEffect>,
    pub(super) policy: job::DeciderPolicy,
}

pub(super) struct DeciderSkip<'a> {
    pub(super) engine: crate::registry::Engine,
    pub(super) model: Option<&'a str>,
    pub(super) target: job::DeciderTarget,
    pub(super) question: &'a str,
    pub(super) options: &'a [String],
    pub(super) policy: job::DeciderPolicy,
    pub(super) reason: String,
    pub(super) reported_kind: Option<crate::pmstate::StopKind>,
    pub(super) effect: Option<crate::worker::StopEffect>,
}

impl JobScheduler {
    pub(super) fn cleanup_completed_advice_artifacts(&self, state: &AgentLoopState) {
        let active = state.advice_inflight.as_ref().map(|parked| parked.seq);
        let mut cleaned_through = self.advice_cleanup_seq.get();
        for run in &state.decider_runs {
            if run.finished_at.is_some() && run.seq > cleaned_through && Some(run.seq) != active {
                if !self.cleanup_advice_artifacts(run.seq) {
                    break;
                }
                cleaned_through = cleaned_through.max(run.seq);
            }
        }
        self.advice_cleanup_seq.set(cleaned_through);
    }

    pub(super) fn cleanup_advice_artifacts(&self, seq: u64) -> bool {
        if std::fs::symlink_metadata(self.paths.steps_dir())
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            eprintln!(
                "refusing to clean decider artifacts through symlinked steps directory {}",
                self.paths.steps_dir().display()
            );
            return false;
        }
        let session = tmux::supervisor_session_name(&self.project_id, &self.work_dir, seq);
        let done = self.paths.advice_done_signal(seq);
        let mut cleanup_succeeded = true;
        for path in [
            done.clone(),
            path_with_suffix(&done, ".code"),
            path_with_suffix(&done, ".tmp"),
            self.paths.advice_log(seq),
            self.paths.advice_last_message(seq),
            self.paths.steps_dir().join(format!("{session}.run.sh")),
        ] {
            cleanup_succeeded &= remove_advice_artifact(&path);
        }
        cleanup_succeeded && remove_empty_steps_dir(&self.paths.steps_dir())
    }

    pub(super) fn next_decider_seq(&mut self) -> u64 {
        self.advise_seq = self.advise_seq.saturating_add(1);
        self.advise_seq
    }

    pub(super) fn record_decider_skip(
        &mut self,
        state: &mut AgentLoopState,
        now: Epoch,
        skip: DeciderSkip<'_>,
    ) {
        state.record_decider_run(job::DeciderRun {
            seq: self.next_decider_seq(),
            started_at: now,
            finished_at: Some(now),
            engine: skip.engine,
            model: skip.model.map(str::to_string),
            target: skip.target,
            question: skip.question.to_string(),
            options: skip.options.to_vec(),
            reported_kind: skip.reported_kind,
            effect: skip.effect,
            policy: skip.policy,
            outcome: job::DeciderOutcome::Skipped {
                reason: skip.reason,
                reported_kind: skip.reported_kind,
                effect: skip.effect,
            },
        });
    }

    /// Abandon any in-flight supervisor consult, killing its `pmsup-` session so none is
    /// leaked. Its reply becomes unreachable by construction: the [`AdviseInFlight`] that
    /// held the nonce and the option list is gone, so there is nothing left to validate a
    /// late answer AGAINST — and [`advise::validate`] cannot pass without them.
    ///
    /// Called wherever the question the consult was about stops being the live question: a
    /// fresh marker bump (the agent moved on), a human answer (a decision outranks an
    /// opinion), or a cold-start relaunch (the pane it was about is gone).
    ///
    /// Also drops an INHERITED debt (`advise_orphaned`) for the same reason, and this is
    /// the load-bearing half of the m23 durability fix: because
    /// [`JobScheduler::save_ledger`] stamps `advice_inflight` from these two fields, this is
    /// the ONE place that has to know a consult stopped mattering — every exit's own save
    /// then clears the durable record for free. In particular it is what stops a human's
    /// "no, don't do that" from being delivered with a blanket auto-approval stapled in
    /// front of it: `on_blocked` calls this BEFORE it appends the answer to
    /// `pending_context`, so the note is never in the payload to begin with.
    pub(super) fn abandon_advice(
        &mut self,
        driver: &dyn Driver,
        ledger: &AgentLoopState,
    ) -> Result<Option<AbandonedAdvice>> {
        let abandoned = if let Some(inflight) = self.advise.as_ref() {
            let report_seq = match &inflight.target {
                AdviceTarget::Marker { report_seq, .. } => Some(*report_seq),
                AdviceTarget::Dialog { .. } => None,
            };
            Some(AbandonedAdvice {
                decider_seq: inflight.parked.seq,
                report_seq,
            })
        } else {
            self.advise_orphaned.as_ref().map(|parked| {
                let report_seq = ledger
                    .advice_queue
                    .iter()
                    .find(|queued| parked.stop_ids.contains(&queued.stop_id))
                    .map(|queued| queued.report_seq);
                AbandonedAdvice {
                    decider_seq: parked.seq,
                    report_seq,
                }
            })
        };
        if let Some(inflight) = self.advise.as_ref() {
            driver.terminate(&inflight.handle.session)?;
        } else if let Some(parked) = self.advise_orphaned.as_ref() {
            let session =
                tmux::supervisor_session_name(&self.project_id, &self.work_dir, parked.seq);
            driver.terminate(&session)?;
        }
        self.advise = None;
        self.advise_orphaned = None;
        Ok(abandoned)
    }

    /// Start ONE supervisor consult about the first auto-flow stop and park a short recheck so this
    /// tick wins and no nudge is typed while the harness still has nothing to say.
    ///
    /// [`AdviceSpawn`] distinguishes session-wide unavailability from one malformed or oversized
    /// decision so a bad current item cannot force valid queued siblings to the human.
    ///
    /// Non-blocking by construction: [`Driver::spawn_step`] is the existing DETACHED
    /// substrate (tee'd wrapper + atomic done-signal), and the reply is read on a later
    /// sweep by [`JobScheduler::advise_step`]. It deliberately does NOT call
    /// [`JobScheduler::ensure_session`] — that is the WORKER's lifecycle, and calling it
    /// would mint or resume the worker's `conversation_id`, breaking
    /// one-conversation-one-writer. A blocking wait here would freeze every other
    /// session's heartbeat *and* its escalation routing, and a headless `claude -p` has
    /// been measured hanging for 180 seconds.
    ///
    /// One stop is consulted at a time. Siblings remain in the durable advice queue and receive
    /// their own independently validated verdict on later sweeps.
    pub(super) fn spawn_advice(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        next: &AgentLoopState,
        auto: &[(String, crate::worker::StopDraft)],
        decider_engine: crate::registry::Engine,
        decider_model: Option<&str>,
    ) -> Result<AdviceSpawn> {
        if !self.advise_enabled || self.advise_health.latched {
            return Ok(AdviceSpawn::Unavailable);
        }
        let Some((first_id, first)) = auto.first() else {
            return Ok(AdviceSpawn::Empty);
        };
        let consult = Consult {
            nonce: mint_uuid_v4(),
            // The goal is the same `brief.md` the nudge quotes. Unreadable/missing reads
            // as empty rather than erroring — the supervisor is then told the goal is
            // blank and will refuse, which is the right answer. CLAMPED because `brief.md`
            // is unbounded human input and rides into every consult of the session, so an
            // uncounted goal eats the consult's TIME (and, if the operator opted into
            // `PM_SUPERVISOR_BUDGET_USD`, its spend) and surfaces as three escalations and a
            // latch rather than as "too big" (`advise::clamp_goal` / `Consult::is_consultable`).
            // The clamp still earns its place with no spend cap in play: a design doc pasted
            // into `brief.md` crowds out the question the supervisor is meant to answer.
            goal: advise::clamp_goal(
                &std::fs::read_to_string(self.paths.brief()).unwrap_or_default(),
            ),
            question: first.question.clone(),
            options: first.options.clone(),
            reported_effect: Some(first.effect.summary()),
            // Milestone C: fresh, in-memory recent-history projection (newest-first, current
            // decision excluded, coherent open-stops, clamped). Empty ⇒ the prompt omits the
            // SITUATION fence and this consult runs on goal+question exactly as before.
            situation: project_situation(next),
            // A standing human directive, read fresh like the goal. Missing/unreadable ⇒
            // empty ⇒ no DIRECTIVE fence (consult runs on goal+question as before). CLAMPED
            // on its own budget (excluded from is_consultable), TRUSTED+restrictive framing
            // lives in build_consult_prompt.
            directive: advise::clamp_directive(
                &std::fs::read_to_string(self.paths.directive()).unwrap_or_default(),
            ),
        };
        let target = AdviceTarget::Marker {
            stop_ids: vec![first_id.clone()],
            report_seq: next.report_generation,
        };
        let policy = job::DeciderPolicy {
            kind: first.kind,
            labelled_risk: first.risk_class,
            effective_risk: crate::policy::effective_risk_kind(first.kind, first.risk_class),
        };
        let decider_bin = decider_engine.bin();
        if !self.supervisor_binary_present(decider_bin) {
            self.advise_health.latch_off(
                &self.project_id,
                &format!("`{decider_bin}` is not on PATH, so no consult can run"),
            );
            return Ok(AdviceSpawn::Unavailable);
        }
        if !consult.is_consultable() {
            return Ok(AdviceSpawn::Unconsultable);
        }
        Ok(
            match self.spawn_consult(
                driver,
                now,
                next,
                consult,
                target,
                DeciderSelection {
                    engine: decider_engine,
                    model: decider_model,
                    reported_kind: Some(first.kind),
                    effect: Some(first.effect),
                    policy,
                },
            )? {
                Some(tick) => AdviceSpawn::Spawned(tick),
                None => AdviceSpawn::Unavailable,
            },
        )
    }

    pub(super) fn spawn_consult(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        next: &AgentLoopState,
        consult: Consult,
        target: AdviceTarget,
        decider: DeciderSelection<'_>,
    ) -> Result<Option<JobTick>> {
        let DeciderSelection {
            engine: decider_engine,
            model: decider_model,
            reported_kind,
            effect,
            policy,
        } = decider;
        // A missing binary does not fail `spawn_step`: its shell exits 127 later.
        // Probe first so a missing binary reaches the human without paying for a doomed consult.
        let decider_bin = decider_engine.bin();
        if !self.supervisor_binary_present(decider_bin) {
            self.advise_health.latch_off(
                &self.project_id,
                &format!("`{decider_bin}` is not on PATH, so no consult can run"),
            );
            return Ok(None);
        }
        if !consult.is_consultable() {
            return Ok(None);
        }
        let seq = self.next_decider_seq();
        let session = tmux::supervisor_session_name(&self.project_id, &self.work_dir, seq);
        // Milestone E: the decider skill is APPENDED to the consult's `--system-prompt` because the
        // consult runs `--bare` (which disables skill auto-discovery, so a file-based skill cannot
        // reach it). The `SUPERVISOR_SYSTEM_PROMPT` const now carries a conditional DIRECTIVE
        // paragraph (added for the human-directive channel), but C's supervisor tests assert its
        // exact SUBSTRINGS via `.contains()`, so those still hold; the decider body is purely
        // additive after it.
        let system_prompt = format!(
            "{}\n\n{}",
            advise::SUPERVISOR_SYSTEM_PROMPT,
            crate::skills::DECIDER_SKILL_MD
        );
        // Dispatch on the SELECTED engine. The claude arm is byte-for-byte the pre-switch path
        // (model pin + `--json-schema` + opt-in budget, verdict read from the tee'd log); the
        // codex arm builds `codex exec … --output-last-message <path>` and points the reap at that
        // isolated file. `advise::validate` re-derives every guarantee from whatever the engine
        // emits, so a non-verdict reply on either arm escalates rather than being trusted.
        let (argv, verdict_path, resolved_model) = match decider_engine {
            crate::registry::Engine::Claude => {
                // The pinned model is overridable because WHICH id resolves is deployment
                // specific, and a wrong one fails SILENTLY (see `build_supervisor_command`).
                // The per-session `config.decider_model` wins when set; otherwise the existing
                // env→const fallback is used byte-for-byte (backward-compat: the decider
                // benchmark relies on the unset behaviour). An empty/whitespace override reads
                // as unset (filtered here, exactly like the env branch below does its own).
                let model = decider_model
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        std::env::var(worker::SUPERVISOR_MODEL_ENV)
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| worker::SUPERVISOR_MODEL.to_string())
                    });
                let argv = worker::build_supervisor_command(
                    &model,
                    &system_prompt,
                    advise::OUTPUT_SCHEMA,
                    &advise::build_consult_prompt(&consult),
                    SUPERVISOR_SHELL_TIMEOUT_S,
                    SUPERVISOR_KILL_GRACE_S,
                    // Unset => no cap, deliberately. Read here rather than inside
                    // `build_supervisor_command` so that function stays pure over its arguments,
                    // exactly as the model override already is.
                    std::env::var(worker::SUPERVISOR_BUDGET_USD_ENV)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .as_deref(),
                );
                (argv, None, Some(model))
            }
            crate::registry::Engine::Codex => {
                // Opt-in model pin (codex resolves a default itself); no `--json-schema`/budget.
                // The per-session `config.decider_model` wins when set; otherwise the existing
                // env→`None` fallback. `None`/empty ⇒ no `-m`, exactly as the builder documents:
                // an empty/whitespace override is filtered here so it behaves identically to `None`.
                let model = decider_model
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        std::env::var(worker::SUPERVISOR_CODEX_MODEL_ENV)
                            .ok()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                    });
                let last = self.paths.advice_last_message(seq);
                let argv = worker::build_supervisor_command_codex(
                    model.as_deref(),
                    &system_prompt,
                    &advise::build_consult_prompt(&consult),
                    &last.to_string_lossy(),
                    SUPERVISOR_SHELL_TIMEOUT_S,
                    SUPERVISOR_KILL_GRACE_S,
                );
                (argv, Some(last), model)
            }
        };
        let audit = job::DeciderRun {
            seq,
            started_at: now,
            finished_at: None,
            engine: decider_engine,
            model: resolved_model,
            target: match &target {
                AdviceTarget::Marker { .. } => job::DeciderTarget::Marker,
                AdviceTarget::Dialog { .. } => job::DeciderTarget::Dialog,
            },
            question: consult.question.clone(),
            options: consult.options.clone(),
            reported_kind,
            effect,
            policy,
            outcome: job::DeciderOutcome::Consulting,
        };
        let done = self.paths.advice_done_signal(seq);
        let log = self.paths.advice_log(seq);
        // Clear any files a PREVIOUS consult left at these paths, because the seq counter
        // guarantees uniqueness only within one daemon process. Resuming it from the ledger
        // covers a restart that happened MID-consult, but a restart after one finished leaves
        // no record to resume from, so the counter really does return to 0 and this consult
        // inherits a completed one's done-signal + log. `tmux::observe` reads the done-signal
        // BEFORE the wrapper has written anything, so the very next sweep would reap the dead
        // consult's exit code as this one's. The nonce would still refuse the stale REPLY —
        // that is exactly what it is for — but at the cost of a spurious `Capability`
        // escalation and a step toward the refusal latch, for a decision policy had already
        // approved. Removing them first makes the question moot rather than survivable.
        let _ = std::fs::remove_file(&done);
        let _ = std::fs::remove_file(&log);
        // Same reason for the codex verdict file: a restart after a prior consult FINISHED
        // returns the seq counter to 0, so this consult could inherit a completed one's
        // `advice-<seq>.last` and reap a stale verdict. Removing it first makes that moot.
        if let Some(vp) = &verdict_path {
            let _ = std::fs::remove_file(vp);
        }
        let until = now + SUPERVISOR_POLL_S;
        let parked_record = job::ParkedAdvice {
            seq,
            stop_ids: match &target {
                AdviceTarget::Marker { stop_ids, .. } => stop_ids.clone(),
                AdviceTarget::Dialog { .. } => Vec::new(),
            },
            pane_dialog: matches!(&target, AdviceTarget::Dialog { .. }),
        };
        // Persist ownership BEFORE spawning. A daemon crash after the tmux session starts can then
        // reconstruct its deterministic name, terminate it, and close the audit instead of leaking
        // an unowned decider process.
        self.advise_orphaned = Some(parked_record.clone());
        let mut parked = next.clone();
        parked.record_decider_run(audit.clone());
        parked.run = JobRun::Monitoring { until };
        parked.updated_at = now;
        self.save_ledger(&mut parked)?;

        let handle = match driver.spawn_step(&session, &self.work_dir, &argv, &done, &log) {
            Ok(h) => h,
            Err(e) => {
                // Rule 6: a spawn failure is a real fault, so it is surfaced ONCE to the
                // human rather than papered over — and it latches the feature off
                // immediately, so the next eligible report escalates without another spawn. Same
                // split as
                // `DesktopNotifier`: a spawn error has nothing transient about it.
                self.advise_health.latch_off(
                    &self.project_id,
                    &format!("a consult could not be spawned ({e})"),
                );
                let refusal = Refusal::NoResult {
                    detail: format!("the consult could not be started ({e})"),
                };
                self.advise_orphaned = None;
                let mut audited = parked;
                audited.finish_decider_run(
                    seq,
                    now,
                    job::DeciderOutcome::Failed {
                        reason: refusal.to_string(),
                    },
                );
                return self
                    .park_advice_refusal(now, &audited, &consult, &target, &refusal)
                    .map(Some);
            }
        };
        // Park a SHORT recheck (not the cadence): the answer is worth waiting a couple of
        // seconds for, and this tick must not nudge. Nothing is consumed — no wake, no
        // `pending_context`, no answer — so this is the same shape as a busy re-park. The
        // marker bookkeeping in `next` (`last_marker_seq`, `last_status`, `continuations`)
        // IS persisted, so the same bump is never disposed twice.
        self.advise_orphaned = None;
        self.advise = Some(Box::new(AdviseInFlight {
            handle,
            consult,
            deadline: now + SUPERVISOR_TIMEOUT_S,
            frozen: false,
            parked: parked_record,
            target,
            verdict_path,
        }));
        self.run = JobRun::Monitoring { until };
        Ok(Some(JobTick::Monitoring { until }))
    }

    /// Whether the supervisor's binary is on `PATH`, probed at most once per session and
    /// cached (a `PATH` scan per auto-flow report would be silly, and pmd's env is fixed at
    /// launch anyway).
    ///
    /// Fails SAFE toward "present": an unreadable `PATH` leaves the behaviour exactly as it
    /// was before this probe existed (spawn, and let the exit code speak), because refusing
    /// to consult on the strength of a failed probe would disable a working supervisor.
    fn supervisor_binary_present(&mut self, bin: &str) -> bool {
        *self
            .advise_binary
            .get_or_insert_with(|| binary_on_path(bin))
    }

    /// Resolve an in-flight consult, if there is one (m20). Runs on every driven tick,
    /// between the marker disposer and the pane classifier — see the call site in
    /// [`JobScheduler::drive`] for why that ordering is the safe one.
    ///
    /// Every path out of here either yields a tick (so no nudge is typed) or reports
    /// `Applied` after writing the answer to `pending_context`, which the ordinary
    /// idle-gated nudge then delivers through the ONE carrier. Nothing here types
    /// anything itself, so the budget/consumption invariants stay in `nudge`.
    ///
    /// A human attached while a consult is in flight no longer costs the feature anything:
    /// `drive`'s human-present gate still returns BEFORE this (it must — nothing may be
    /// typed at a pane someone is using), but it now FREEZES the consult's deadline once, so
    /// a routine attach cannot expire a supervisor that already answered. See
    /// [`AdviseInFlight::frozen`] for why the freeze is granted at most once, and why a
    /// consult that outlives the grant is escalated rather than dropped.
    pub(super) fn advise_step(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<AdviseStep> {
        let Some(inflight) = self.advise.take() else {
            // Nothing live — but this daemon may have INHERITED a debt from one that died
            // mid-consult, which only the first driven tick after a restart can pay.
            if let Some(parked) = self.advise_orphaned.take() {
                let recovered = self.recover_parked_advice(driver, now, base, &parked);
                if recovered.is_err() {
                    self.advise_orphaned = Some(parked);
                }
                return recovered;
            }
            return Ok(AdviseStep::NotEngaged);
        };
        // (1) The harness's OWN reap deadline, checked BEFORE `observe` and completely
        // independently of it. The shell `timeout` lives inside the consult's process
        // tree, so it cannot help when the tree is what went missing: a session killed
        // from outside, a wrapper that never ran, a done-signal never written. This is
        // the check that makes "no decision this tick" reachable without observing
        // anything at all.
        if now >= inflight.deadline {
            if let Err(error) = driver.terminate(&inflight.handle.session) {
                self.advise = Some(inflight);
                return Err(error);
            }
            if inflight.frozen {
                // The deadline was already extended once for an attached human and STILL
                // expired, so a human has owned this pane for longer than the whole freeze
                // allowance. Escalate (the worker's real question reaches them — never drop
                // it, which would consume the marker bump and deliver nothing), but say
                // something TRUE and do not count it toward `SUPERVISOR_DEAD_AFTER`: a
                // consult that could not be reaped because a human was using the terminal is
                // no evidence at all that the transport is broken, and counting it is how
                // three routine attaches used to latch the feature off in silence.
                let refusal = Refusal::NoResult {
                    detail: format!(
                        "a human was attached to the session for longer than the \
                         {SUPERVISOR_TIMEOUT_S}s freeze allowance, so this decision was never \
                         collected — it is yours"
                    ),
                };
                let mut audited = base.clone();
                audited.finish_decider_run(
                    inflight.parked.seq,
                    now,
                    job::DeciderOutcome::Interrupted {
                        reason: refusal.to_string(),
                    },
                );
                return self
                    .park_advice_refusal(
                        now,
                        &audited,
                        &inflight.consult,
                        &inflight.target,
                        &refusal,
                    )
                    .map(AdviseStep::Yields);
            }
            let detail = format!("no answer within {SUPERVISOR_TIMEOUT_S}s");
            return self
                .advice_failed(now, base, &inflight, detail)
                .map(AdviseStep::Yields);
        }
        let exit = match tmux::observe(driver, &inflight.handle) {
            Ok(tmux::Observation::Running) => {
                // Still thinking. Come back soon and consume NOTHING (no nudge, no wake,
                // no `pending_context`, no answer) — byte-for-byte the Busy re-park's
                // shape. Deliberately does NOT touch `busy_since`: a session waiting on
                // its own supervisor is not evidence about the WORKER's progress either
                // way, so the stall backstop keeps whatever window the pane earned.
                let until = now + SUPERVISOR_POLL_S;
                self.advise = Some(inflight);
                self.persist_run(base, now, JobRun::Monitoring { until }, None)?;
                return Ok(AdviseStep::Yields(JobTick::Monitoring { until }));
            }
            Ok(tmux::Observation::Completed { exit_code }) => exit_code,
            Ok(tmux::Observation::Orphaned) => {
                if let Err(error) = driver.terminate(&inflight.handle.session) {
                    self.advise = Some(inflight);
                    return Err(error);
                }
                let detail = "the consult session died without recording an exit code".to_string();
                return self
                    .advice_failed(now, base, &inflight, detail)
                    .map(AdviseStep::Yields);
            }
            // A genuine I/O fault reading the done-signal. Treated as "no result" rather
            // than propagated: an unreadable signal must not poison the whole session.
            Err(e) => {
                if let Err(error) = driver.terminate(&inflight.handle.session) {
                    self.advise = Some(inflight);
                    return Err(error);
                }
                let detail = format!("the consult's done-signal was unreadable ({e})");
                return self
                    .advice_failed(now, base, &inflight, detail)
                    .map(AdviseStep::Yields);
            }
        };
        // Reaped either way, so the session has served its purpose: terminate it before
        // anything else, so a `remain-on-exit` corpse cannot outlive the consult.
        if let Err(error) = driver.terminate(&inflight.handle.session) {
            self.advise = Some(inflight);
            return Err(error);
        }
        if exit != 0 {
            let detail = format!("the consult exited {exit}");
            let tick = self.advice_failed(now, base, &inflight, detail)?;
            return Ok(AdviseStep::Yields(tick));
        }
        // Exit 0. Read the verdict from wherever this engine put it: the codex path isolates it in
        // `verdict_path` (`--output-last-message`); the claude path leaves it in the tee'd log
        // (never the pane, which is a rendering of the reply rather than the reply itself). An
        // unreadable/absent source is an empty string, which `validate` refuses as `NoJson` — no
        // `unwrap` on untrusted bytes on either path.
        let source = inflight
            .verdict_path
            .as_deref()
            .unwrap_or(&inflight.handle.log);
        let raw = std::fs::read_to_string(source).unwrap_or_default();
        let verdict = match advise::validate(&inflight.consult, &raw) {
            Ok(v) => v,
            Err(refusal) => {
                let tick = self.advice_refused(now, base, &inflight, &refusal)?;
                return Ok(AdviseStep::Yields(tick));
            }
        };
        if matches!(&inflight.target, AdviceTarget::Dialog { .. }) {
            return self.apply_dialog_verdict(driver, now, base, inflight, verdict);
        }
        let audit_outcome = decider_resolved_outcome(&verdict);
        let text = match advise::apply_text(&inflight.consult, &verdict) {
            Ok(t) => t,
            Err(refusal) => {
                let tick = self.advice_refused(now, base, &inflight, &refusal)?;
                return Ok(AdviseStep::Yields(tick));
            }
        };
        // Rule 7 — ANTI-PING-PONG. If the pane looks byte-for-byte as it did when we last
        // delivered advice AND the advice is byte-for-byte the same, the worker did not
        // act on what we already typed, so typing it again is a loop that spends tokens
        // forever and tells the human nothing. Refuse and escalate instead. One
        // `capture-pane`, and only on a reap tick (which is rare), so this adds no cost to
        // the 500ms sweep.
        let pane = driver
            .capture_tail(&self.loop_session(), 40)
            .unwrap_or_default();
        let fingerprint = (advise::text_hash(&pane), text);
        if self.advise_last.as_ref() == Some(&fingerprint) {
            return self
                .advice_refused(now, base, &inflight, &Refusal::NoProgress)
                .map(AdviseStep::Yields);
        }
        // Usable advice. It rides to the agent as `pending_context` — the ONE durable
        // carrier — so it is on DISK before anything tries to type it, and APPENDED
        // rather than assigned so an undelivered human answer parked there is not
        // destroyed (the m18 closure; see `append_context`).
        self.advise_health.note_success();
        let mut applied = base.clone();
        applied.pending_context = Some(append_context(
            applied.pending_context.take(),
            fingerprint.1.clone(),
        ));
        // Leave an AUDIT TRAIL for a decision taken in the user's name. Until now only
        // REFUSALS wrote `last_status`, which is exactly backwards: the dashboard showed
        // every time the harness declined to decide and nothing at all when it decided FOR
        // them. `last_status` is the line pmtui already renders, so this costs no new
        // surface — and the m20 sanitizers ran on `fingerprint.1` before it got here, which
        // is why quoting it is safe.
        applied.last_status = Some(format!(
            "the session supervisor resolved a low-stakes decision on your behalf: \
             {}",
            one_line(&fingerprint.1)
        ));
        applied.updated_at = now;
        if let AdviceTarget::Marker {
            stop_ids,
            report_seq,
        } = &inflight.target
        {
            applied
                .advice_queue
                .retain(|queued| !stop_ids.contains(&queued.stop_id));
            applied.record_decision(job::DecisionRecord::at(
                now,
                None,
                job::DecisionKind::AutoFlow,
                Some(inflight.consult.question.clone()),
                stop_ids.clone(),
            ));
            if applied.advice_queue.is_empty() {
                applied.finish_turn_review(*report_seq, job::TurnReviewOutcome::AutoFlow);
            }
        }
        applied.finish_decider_run(inflight.parked.seq, now, audit_outcome);
        applied.record_event(
            now,
            AutopilotEventKind::SupervisorResolved(Some(one_line(&fingerprint.1))),
        );
        self.save_ledger(&mut applied)?;
        self.advise_last = Some(fingerprint);
        Ok(AdviseStep::Applied)
    }
}

pub(super) fn path_with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

fn remove_advice_artifact(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            eprintln!(
                "completed decider artifact was preserved at {}: {error}",
                path.display()
            );
            false
        }
    }
}

fn remove_empty_steps_dir(path: &Path) -> bool {
    match std::fs::remove_dir(path) {
        Ok(()) => true,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            true
        }
        Err(error) => {
            eprintln!(
                "empty decider artifact directory was preserved at {}: {error}",
                path.display()
            );
            false
        }
    }
}

fn decider_resolved_outcome(verdict: &crate::advise::Verdict) -> job::DeciderOutcome {
    match verdict {
        crate::advise::Verdict::Select {
            index,
            option,
            reason,
        } => job::DeciderOutcome::Resolved {
            answer: format!("option {} \u{2014} {option}", index + 1),
            reason: reason.clone(),
        },
        crate::advise::Verdict::Answer { text, reason } => job::DeciderOutcome::Resolved {
            answer: text.clone(),
            reason: reason.clone(),
        },
    }
}

/// Flatten text to one line for a `last_status` dashboard entry. The supervisor's own text
/// is already control-byte-free and length-capped by [`advise::validate`]; the harness
/// assembles it into a multi-line payload for the pane, and a status line is not the place
/// for that shape.
pub(super) fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 200 {
        return flat;
    }
    let cut: String = flat.chars().take(200).collect();
    format!("{cut}…")
}

/// Project the run's recent decider history into the `SITUATION` DATA the supervisor sees
/// (Milestone C). Pure over `next`; NEVER persisted. Ordered NEWEST-FIRST so
/// [`advise::clamp_situation`] keeps the recent head and drops the stale tail.
///
/// Coherence contract (B-Minor-1 / CF-3): open stops are read from the LIVE, authoritative
/// [`AgentLoopState::open_stops`], NEVER from `next.situation.open_stops` — that snapshot can
/// be stale on a non-escalate arm and empty-but-`Blocked` on auto-flow, so it is not trusted
/// here. No standalone `state` line is emitted because the report snapshot can describe the
/// current unsettled decision and contradict live open stops.
///
/// Only completed decisions are persisted, so the current in-flight decision is absent by
/// construction and every stored decision is prior context.
pub fn project_situation(next: &AgentLoopState) -> String {
    let mut lines: Vec<String> = Vec::new();

    // (1) The agent's own most recent intent — the freshest signal, so it heads the
    //     newest-first order and survives the clamp. (s_c1/s_c3 project last_plan.)
    if let Some(plan) = next
        .last_plan
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("- the agent's stated next step: {plan}"));
    }
    if let Some(status) = next
        .situation
        .as_ref()
        .and_then(|situation| situation.status.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("- the worker's latest reported result: {status}"));
    }

    // (2) An OBJECTIVE progress signal from D's counter — a fact, not precedent.
    if next.stale_plan_streak > 0 {
        lines.push(format!(
            "- objective signal: the agent has restated the same plan {}× without the marker \
             advancing",
            next.stale_plan_streak
        ));
    }

    // (3) Recent prior decisions, newest-first.
    for d in next.decisions.iter().rev() {
        if let Some(summary) = d
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            lines.push(format!(
                "- earlier this run pmd handled ({}): {summary}",
                d.kind.as_str()
            ));
        }
    }

    // (4) Stops the session is CURRENTLY blocking a human on — LIVE, authoritative.
    for s in &next.open_stops {
        lines.push(format!(
            "- still awaiting a human decision on stop {}",
            s.id
        ));
    }

    if lines.is_empty() {
        // C-5: a thin/fresh ledger projects NOTHING; the SITUATION fence is omitted and the
        // consult still runs on goal+question (never a forced static note).
        return String::new();
    }
    advise::clamp_situation(&lines.join("\n"))
}

/// Whether `name` resolves to an executable file on `PATH` — the supervisor's cheap
/// pre-flight (see [`JobScheduler::supervisor_binary_present`]).
///
/// Hand-rolled rather than shelling out to `which`/`command -v`: this runs inside the
/// daemon's 500ms sweep thread, and spawning a subprocess to ask whether we can spawn a
/// subprocess is the wrong trade. An absolute/relative `name` is probed as-is (mirroring
/// how a shell resolves anything containing a separator).
///
/// Returns TRUE when `PATH` itself is unavailable: "I could not tell" must behave exactly
/// as before this probe existed, never as "switch the feature off".
pub(super) fn binary_on_path(name: &str) -> bool {
    if name.contains(std::path::MAIN_SEPARATOR) {
        return is_executable_file(Path::new(name));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return true;
    };
    std::env::split_paths(&path).any(|dir| is_executable_file(&dir.join(name)))
}

/// Whether `p` is a file the current process could execute. The executable BIT is checked
/// on unix, because a same-named directory or a non-executable data file on `PATH` would
/// otherwise read as "the supervisor is installed".
pub(super) fn is_executable_file(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    executable_bit(&md)
}

/// The execute-permission half of [`is_executable_file`], split out so the `cfg` covers one
/// expression instead of a function body with two tails.
#[cfg(unix)]
fn executable_bit(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_bit(_md: &std::fs::Metadata) -> bool {
    true
}
