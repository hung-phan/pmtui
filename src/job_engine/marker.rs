//! The marker DISPOSER: read the `needs-you.json` [`WakeReport`] the agent wrote and
//! turn it into ledger transitions. This is the one place where the agent's own
//! written self-assessment — working / monitoring / blocked, its stops, its proposed
//! cadence — outranks anything the harness infers from pixels, so what a report is
//! ALLOWED to do is decided here and nowhere else. The harness only ever STATS and
//! READS the marker; the agent stays its sole writer.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Result;

use crate::clock::Epoch;
use crate::job::{
    AgentLoopState, AutopilotEventKind, DecisionKind, DecisionRecord, JobRun, LedgerSituation,
    MarkerRevision, TurnDisposition, WakeReport, WakeState,
};
use crate::pmstate::{OpenStop, StopKind};
use crate::policy::{self, Decision};
use crate::state::{self, Config};
use crate::tmux::Driver;

use super::drive::BUSY_RECHECK_S;
use super::nudge::{CADENCE_MAX_S, CADENCE_MIN_S, DEFAULT_CADENCE_S};
use super::stops::open_stop;
use super::supervisor::DeciderSkip;
use super::{JobScheduler, JobTick, RAW_JSONL_MAX_BYTES};

/// One full-fidelity `raw.jsonl` line: the dispose metadata plus the whole [`WakeReport`]
/// (B-4: full-fidelity machine JSONL). Serialized compactly to a single line.
#[derive(serde::Serialize)]
struct RawLine<'a> {
    at: Epoch,
    disposed: u64,
    report: &'a WakeReport,
}

/// OQ3 write/read-race guard: a marker that fails to parse but whose file was last
/// modified within this window of REAL wall-clock time is treated as "not ready"
/// (a plausibly mid-write read, even though the agent is told to write atomically),
/// re-checked at [`BUSY_RECHECK_S`] WITHOUT counting a stall. Only once it stays
/// unparseable past this window is it counted as malformed (`lenient_working`).
const MARKER_MIDWRITE_GRACE: Duration = Duration::from_secs(1);

/// FO-1 (Milestone D) escalation threshold (T2): once `stale_plan_streak` reaches this
/// many consecutive byte-identical (status AND next_step) reports, the disposer escalates
/// a `WorkerStuck` — the "alive but not advancing" twin of `DEFAULT_STALL_BUSY_S`. The
/// lower SIGNAL threshold (T1 = 3), at which Milestone E's nudge surfaces "you've restated
/// this plan N×", is E's own const; D only exposes the counter. Could become a per-session
/// `Config` field later (mirrors `DEFAULT_STALL_BUSY_S`).
pub(super) const DEFAULT_STALE_PLAN_STALL: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerStamp {
    pub(super) modified: SystemTime,
    pub(super) len: u64,
    pub(super) inode: u64,
}

impl MarkerStamp {
    fn from_file(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            modified: metadata.modified()?,
            len: metadata.len(),
            inode: metadata.ino(),
        })
    }

    fn with_fingerprint(self, fingerprint: String) -> MarkerRevision {
        let modified_ns = self
            .modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        MarkerRevision {
            modified_ns,
            len: self.len,
            inode: self.inode,
            fingerprint,
        }
    }
}

pub(super) struct UnresolvedAdvice<'a> {
    pub report_seq: u64,
    pub stops: &'a [(String, crate::worker::StopDraft)],
    pub reason: &'a str,
    pub preserve_queued_siblings: bool,
}

fn marker_already_disposed(
    ledger: &AgentLoopState,
    report: &WakeReport,
    revision: &MarkerRevision,
) -> bool {
    match ledger.last_marker_revision.as_ref() {
        Some(last) => last == revision,
        // Compatibility for ledgers written before semantic fingerprints: when the current
        // marker is exactly the report represented by the persisted situation, seed no duplicate
        // effects. Sequence equality alone is insufficient: an older worker may reuse a timestamp
        // while changing status or plan, and that substantive revision must be accepted.
        None => {
            let next_step_matches = match report.next_step.as_deref().map(str::trim) {
                None | Some("") => true,
                Some(step) => ledger.last_plan.as_deref().map(str::trim) == Some(step),
            };
            ledger.situation.as_ref().is_some_and(|situation| {
                situation.seq == report.seq
                    && situation.state == report.state
                    && situation.status.as_deref().map(str::trim)
                        == report.status.as_deref().map(str::trim)
                    && next_step_matches
            }) || (ledger.situation.is_none()
                && ledger.last_marker_seq != 0
                && ledger.last_marker_seq == report.seq)
        }
    }
}

impl JobScheduler {
    /// The current atomic marker-file revision, or `None` when absent/unreadable.
    fn marker_revision(&self) -> Option<MarkerStamp> {
        File::open(self.paths.needs_you())
            .ok()
            .and_then(|file| MarkerStamp::from_file(&file).ok())
    }

    /// Whether the atomic file revision changed since the last marker observation. Inode and
    /// length close the same-timestamp gap left by an mtime-only probe.
    pub(super) fn marker_revision_advanced(&self) -> bool {
        match self.marker_revision() {
            Some(revision) => self.last_marker_stamp != Some(revision),
            None => false,
        }
    }

    /// Read `needs-you.json` and dispose the agent's [`WakeReport`] into ledger
    /// transitions (design §2). Returns `Some(tick)` when a bump produced a
    /// terminal-for-this-tick decision (park/escalate) that WINS the tick, or `None`
    /// when the caller should fall through to the normal classify+nudge heartbeat
    /// (no marker, an unchanged revision, duplicate content, or a `Working`/auto-flow
    /// disposition that keeps nudging).
    ///
    /// The harness only STATS + READS the marker — the agent is its SOLE writer, so the
    /// single-writer-ledger invariant is preserved (the disposer's own writes go to the
    /// ledger via `job::save`, never to the marker).
    pub(super) fn observe_marker(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        ledger: &AgentLoopState,
        config: &Config,
    ) -> Result<Option<JobTick>> {
        // Read metadata and bytes from one descriptor so an atomic rename cannot combine two
        // marker revisions. The cheap revision comparison avoids parsing unchanged content.
        let Ok(mut file) = File::open(self.paths.needs_you()) else {
            return Ok(None);
        };
        let revision = MarkerStamp::from_file(&file)?;
        if self.last_marker_stamp == Some(revision) {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        match serde_json::from_slice::<WakeReport>(&bytes) {
            // Mode B (malformed) — with the OQ3 write/read-race hardening.
            Err(_) => {
                if marker_is_mid_write(revision.modified) {
                    // VERY FRESH mtime: plausibly a mid-write read (defensive — the agent
                    // is told to write atomically). Treat as "not ready": recheck soon
                    // WITHOUT counting a stall, and do NOT record the revision as seen, so
                    // the same file is re-read next tick and only counts once it stays
                    // unparseable past the grace window.
                    let until = now + BUSY_RECHECK_S;
                    self.persist_run(ledger, now, JobRun::Monitoring { until }, None)?;
                    Ok(Some(JobTick::Monitoring { until }))
                } else {
                    // Stays unparseable past the grace window. Before downgrading to a lenient
                    // `working` continuation, try a RECOVERY parse that IGNORES unknown fields:
                    // the strict `parse_report` is `deny_unknown_fields`, so ONE typo'd/extra
                    // field rejects the whole report — and a `blocked` escalation must not be
                    // silently swallowed into a low-priority working bump because of it. A
                    // genuinely malformed marker (bad JSON / invalid `state`) still fails the
                    // recovery and downgrades, so drift still surfaces.
                    if let Ok(report) = parse_report_lenient_bytes(&bytes) {
                        let fingerprint = crate::job::report_fingerprint(&report)?;
                        let identity = revision.with_fingerprint(fingerprint);
                        if marker_already_disposed(ledger, &report, &identity) {
                            self.last_marker_stamp = Some(revision);
                            return Ok(None);
                        }
                        self.busy_since = None;
                        let outcome =
                            self.dispose_report(driver, now, ledger, config, report, identity)?;
                        self.last_marker_stamp = Some(revision);
                        return Ok(outcome);
                    }
                    let outcome = self.lenient_working(now, ledger, config)?;
                    self.last_marker_stamp = Some(revision);
                    Ok(Some(outcome))
                }
            }
            Ok(report) => {
                let fingerprint = crate::job::report_fingerprint(&report)?;
                let identity = revision.with_fingerprint(fingerprint);
                if marker_already_disposed(ledger, &report, &identity) {
                    self.last_marker_stamp = Some(revision);
                    return Ok(None);
                }
                // Progress: a fresh, valid marker bump (any state) proves the agent is
                // alive and advancing — clear the stall backstop timer here, once, before
                // branching on state, so a diligent agent that signals progress (as the
                // nudge prompt instructs) never trips `DEFAULT_STALL_BUSY_S`.
                self.busy_since = None;
                let outcome = self.dispose_report(driver, now, ledger, config, report, identity)?;
                self.last_marker_stamp = Some(revision);
                Ok(outcome)
            }
        }
    }

    /// Dispose one semantically new marker revision. pmd assigns its operational generation;
    /// worker `seq` is retained only as audit data.
    fn dispose_report(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        ledger: &AgentLoopState,
        config: &Config,
        report: WakeReport,
        revision: MarkerRevision,
    ) -> Result<Option<JobTick>> {
        // A fresh, valid marker bump means the agent has moved on from whatever an
        // in-flight consult was about, so that consult is moot: abandon it (killing its
        // session so no `pmsup-` is leaked) before disposing. Done here, once, for EVERY
        // report state — a `Working`/`Monitoring`/escalating bump invalidates a pending
        // opinion just as surely as a new auto-flow one does, and the auto-flow branch
        // below then starts a fresh consult against the CURRENT question.
        let abandoned = self.abandon_advice(driver, ledger)?;
        let mut next = ledger.clone();
        // A fresh marker supersedes every queued decision from the prior report. A blocked report
        // below repopulates the queue from its own stops after deterministic policy partitions it.
        next.advice_queue.clear();
        if let Some(abandoned) = abandoned {
            next.finish_decider_run(
                abandoned.decider_seq,
                now,
                crate::job::DeciderOutcome::Interrupted {
                    reason: "the agent reported a newer state before the decider returned".into(),
                },
            );
            if let Some(report_seq) = abandoned.report_seq {
                next.finish_turn_review(report_seq, crate::job::TurnReviewOutcome::Interrupted);
            }
        }
        next.last_marker_seq = report.seq;
        next.report_generation = next
            .report_generation
            .max(next.digest.disposed)
            .saturating_add(1);
        next.last_marker_revision = Some(revision);
        if next.nudged_at_report_generation.is_none() && next.nudged_at_seq.is_some() {
            next.nudged_at_report_generation = Some(next.report_generation.saturating_sub(1));
        }
        // CONFIRM POINT of the resume→create fallback. An accepted bump proves the agent
        // actually ran a turn ON THIS conversation, so its id is now real in the engine —
        // clear the fallback gate so every future relaunch RESUMES it
        // (`resolve_conversation_id` branch 1) rather than re-creating. `dispose_report`
        // is only reached on an accepted semantic marker revision, and this is BEFORE the state
        // match / any early
        // return, so it fires exactly once per accepted bump whichever arm wins the tick.
        next.resume_unconfirmed = false;
        // Keep-prior for status too (symmetric with the plan below): a TERSE bump that omits
        // `status` (or sends a present-but-empty one) must NOT wipe `last_status` to None — that
        // blanks the dashboard chip AND the nudge echo, even though the agent DID report (a fresh
        // `seq`), just without new prose. Only a non-empty status overwrites; else the prior line
        // stands. (The FO-1 streak below still compares the RAW `report.status` against the prior,
        // so keep-prior here does not mask a genuinely-unchanged status.)
        if let Some(status) = report.status.clone()
            && !status.trim().is_empty()
        {
            next.last_status = Some(status);
        }
        // Keep-prior: only overwrite the plan when THIS report restated it with a
        // NON-EMPTY step, so an agent that reports progress without repeating its plan
        // — whether by omitting `next_step` or sending a present-but-empty "" — does not
        // lose its orientation.
        if let Some(step) = report.next_step.clone()
            && !step.trim().is_empty()
        {
            next.last_plan = Some(step);
        }
        // === FO-1 (Milestone D): plan-staleness streak accrual ===
        // Compare THIS report against the PRIOR stored values (`ledger`, before the
        // keep-prior overwrite above) — the streak counts CONSECUTIVE reports whose BOTH
        // human-facing fields (status AND next_step) are trim-equal to the prior. Trim both
        // sides HERE so trailing-whitespace drift cannot spuriously reset it — the keep-prior
        // block above stores the plan UNtrimmed (`report.next_step` verbatim, only filtered on
        // trimmed-emptiness), so it is this comparison, not storage, that normalizes whitespace.
        // Pure equality — no prose interpretation.
        match report
            .next_step
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // A terse bump (no/empty next_step) preserves the prior plan (keep-prior above)
            // and leaves the streak UNTOUCHED — we cannot judge staleness without a
            // restated plan, and a diligent progress bump must not reset it.
            None => {}
            Some(step) => {
                let same_plan = ledger.last_plan.as_deref().map(str::trim) == Some(step);
                let same_status = report.status.as_deref().map(str::trim)
                    == ledger.last_status.as_deref().map(str::trim);
                if same_plan && same_status {
                    next.stale_plan_streak = next.stale_plan_streak.saturating_add(1);
                } else {
                    next.stale_plan_streak = 0;
                }
            }
        }
        // FO-2 (Milestone D): any accepted marker bump means the agent reported — a
        // marker-less finish is over, so the recheck counter resets on every valid bump.
        next.marker_less_rechecks = 0;
        // OQ6: capture the agent-reported conversation id when the ledger has none.
        if next.conversation_id.is_none()
            && let Some(cid) = report.conversation_id.as_ref()
        {
            next.conversation_id = Some(cid.clone());
        }
        next.updated_at = now;
        // ADAPTIVE CADENCE. Applied HERE, before the state match, so every arm below
        // persists it with the same `save_ledger` call and no arm can forget to — and so a
        // `monitoring` report that proposes a new rhythm without a `next_check_s` parks on
        // the NEW cadence rather than one last tick of the old one.
        let cadence_note = Self::adopt_cadence(&mut next, report.cadence_s);

        // The change must be VISIBLE. `last_status` is the one line the dashboard shows for a
        // session, so the note rides along with it: a session that re-times itself behind the
        // human's back is the invisible behaviour change this harness exists to stop. The
        // harness speaks in its own voice rather than editing the agent's words — and it
        // reports the CLAMPED value, never the requested one.
        if let Some(note) = cadence_note {
            // Surface the re-time on the autopilot feed too, not only inline in last_status.
            next.record_event(now, AutopilotEventKind::CadenceChanged(note.clone()));
            next.last_status = Some(match next.last_status.take() {
                Some(s) if !s.trim().is_empty() => format!("{s} · {note}"),
                _ => note,
            });
        }

        // === Milestone B: the decider-lane ledger — the single accepted-bump seam ===
        // Unconditional, ONCE per accepted marker bump, BEFORE any WakeState branch — so the
        // two paths that skip the four arms (auto-flow→spawn_advice's `parked = next.clone()`,
        // and blocked-no-stop→park_stuck's clone) still carry them. This structurally
        // guarantees `digest.disposed == number of accepted bumps` and keeps `situation` from
        // ever going stale on a park (Missing-3).
        next.digest.disposed = next.digest.disposed.saturating_add(1);
        let mut open_ids = Vec::with_capacity(next.open_stops.len());
        for stop in &next.open_stops {
            open_ids.push(stop.id.clone());
        }
        next.situation = Some(LedgerSituation::from_report(&report, &open_ids, now));
        // `raw.jsonl` is full-fidelity and a pure function of the report, so emit it here.
        // ERROR-ISOLATED: a side-file failure is logged and swallowed — it must never fail
        // the tick (raw.jsonl is machine-only, lenient-read, torn-tolerant).
        if let Ok(line) = serde_json::to_string(&RawLine {
            at: now,
            disposed: next.digest.disposed,
            report: &report,
        }) && let Err(e) = self.append_raw(&line, RAW_JSONL_MAX_BYTES)
        {
            eprintln!("pmd: {}: raw.jsonl append failed: {e}", self.project_id);
        }

        // === FO-1 T2 (Milestone D): escalate a plan that never advances ===
        // Placed in the SHARED pre-match location so a Working AND a Monitoring bump both
        // escalate (Important-1). This is a BACKSTOP, not a policy decision: it goes through
        // `park_stuck_kind` (which parks `Blocked` directly and NEVER consults
        // `decide_kind`), exactly as the busy-stall / dead-pane / malformed-marker
        // escalations do — so `decide_kind` stays byte-for-byte pure. The B seam above has
        // already counted this accepted bump (`digest.disposed`) and emitted its full
        // `raw.jsonl` line, so the escalation loses no fidelity; `park_stuck_kind` clones
        // `next` (streak + disposed + situation) and persists it.
        if next.stale_plan_streak >= DEFAULT_STALE_PLAN_STALL {
            next.finish_turn_from_report(now, &report, TurnDisposition::Stalled);
            let reason = format!(
                "agent has restated the same plan {}× without the work advancing — it may be \
                 stuck in a loop; close it, or answer to keep it going",
                next.stale_plan_streak
            );
            return Ok(Some(self.park_stuck_kind(
                now,
                &next,
                StopKind::WorkerStuck,
                reason,
            )?));
        }

        match report.state {
            // Progress: keep nudging on cadence. Persist the watermark + status +
            // continuations reset, then fall through so `drive` delivers the heartbeat
            // nudge. (A marker bump is not a human touch — `wakes`/`window_start`
            // are deliberately NOT reset.)
            WakeState::Working => {
                next.continuations = 0;
                next.finish_turn_from_report(now, &report, TurnDisposition::Working);
                next.record_event(now, AutopilotEventKind::Reported(report.status.clone()));
                next.record_decision(DecisionRecord::at(
                    now,
                    Some(report.seq),
                    DecisionKind::Working,
                    report.status.clone(),
                    Vec::new(),
                ));
                self.save_ledger(&mut next)?;
                Ok(None)
            }
            // Self-scheduled nap: the agent is waiting on something it polls itself.
            // Park until `next_check_s` (or the base cadence) and SKIP the nudge.
            WakeState::Monitoring => {
                // `next`, not `ledger`: a report may have just proposed a new base cadence
                // (see `adopt_cadence`), and parking on the OLD one would spend one more tick
                // at a rhythm the agent has already told us is wrong. `next_check_s` still
                // wins when present — a one-off nap is more specific than a base rhythm.
                let cadence = next.cadence_s.unwrap_or(DEFAULT_CADENCE_S);
                // CLAMP the agent's self-proposed nap to the SAME bounds `cadence_s` is held to
                // (`adopt_cadence`): `next_check_s` is worker-supplied and was unbounded, so a
                // report of `next_check_s: 999999999` parked the session ~31 years out (a silent
                // self-eviction from the fleet), and a `next_check_s: 0`/`1` parked it sub-second —
                // it expires immediately, re-nudges, and re-reports the same nap: the sub-minute
                // nudge storm `CADENCE_MIN_S` exists to forbid. Both ends matter, so this is a
                // `clamp`, not a bare ceiling. `saturating_add` keeps the `Epoch` (i64) arithmetic
                // from overflowing on a pathological value even after the clamp.
                let nap = report
                    .next_check_s
                    .unwrap_or(cadence)
                    .clamp(CADENCE_MIN_S, CADENCE_MAX_S);
                let until = now.saturating_add(nap as i64);
                next.continuations = 0;
                next.finish_turn_from_report(now, &report, TurnDisposition::Monitoring);
                next.record_event(now, AutopilotEventKind::Reported(report.status.clone()));
                next.record_decision(DecisionRecord::at(
                    now,
                    Some(report.seq),
                    DecisionKind::Monitoring,
                    report.status.clone(),
                    Vec::new(),
                ));
                // The accepted report is stronger evidence than the optional engine hook: this
                // wake has yielded. Codex can miss its notify callback, and retaining the old
                // equality baseline then makes `turn_in_progress()` hold an idle pane every 5s
                // until the 1800s busy-stall backstop fires. Retire both copies together so a
                // daemon restart and the dashboard agree; the next delivered nudge creates a new
                // baseline in the normal path.
                next.turn_count_at_nudge = None;
                self.turns_at_nudge = None;
                next.run = JobRun::Monitoring { until };
                self.save_ledger(&mut next)?;
                self.run = JobRun::Monitoring { until };
                Ok(Some(JobTick::Monitoring { until }))
            }
            // Needs a human decision: route each stop through the tier oracle, reusing
            // the SURVIVING escalation machinery (ported from `phase_engine::dispose`).
            WakeState::Blocked => {
                if report.stops.is_empty() {
                    next.finish_turn_from_report(now, &report, TurnDisposition::Stalled);
                    // No routable stop → treat as a stall escalation.
                    let reason = "agent reported blocked without a stop".to_string();
                    next.record_decision(DecisionRecord::at(
                        now,
                        Some(report.seq),
                        DecisionKind::Stalled,
                        Some(reason.clone()),
                        Vec::new(),
                    ));
                    let tick = self.park_stuck(now, &next, reason.clone())?;
                    if let Err(e) =
                        self.append_decision_md(now, DecisionKind::Stalled, Some(&reason))
                    {
                        eprintln!("pmd: {}: decisions.md append failed: {e}", self.project_id);
                    }
                    return Ok(Some(tick));
                }
                let mut escalating: Vec<OpenStop> = Vec::new();
                let mut auto: Vec<(String, crate::worker::StopDraft)> = Vec::new();
                for (i, draft) in report.stops.iter().enumerate() {
                    let id = format!(
                        "stop-{}-{}-{}-{now}",
                        self.project_id, i, next.report_generation
                    );
                    let labelled_effective =
                        policy::effective_risk_kind(draft.kind, draft.risk_class);
                    let kind = if draft.effect.requires_human()
                        && labelled_effective != crate::state::RiskClass::Hard
                    {
                        StopKind::Capability
                    } else {
                        draft.kind
                    };
                    let effective_risk = policy::effective_risk_kind(kind, draft.risk_class);
                    // The draft's question/options ride into the ledger: `kind` alone
                    // tells the human a decision is needed, not what is being asked.
                    let open = open_stop(
                        id,
                        kind,
                        draft.context_ref.clone(),
                        &draft.question,
                        &draft.options,
                        now,
                    );
                    let decision = policy::decide_kind(config.autonomy, kind, draft.risk_class);
                    if decision == Decision::Escalate {
                        let reason = if draft.effect.requires_human() {
                            format!(
                                "typed effect `{}` requires human ownership; no decider was called",
                                draft.effect.summary()
                            )
                        } else {
                            format!(
                                "deterministic policy escalated stop kind `{kind:?}` at \
                                 labelled={:?}, effective={effective_risk:?}, tier={:?}; no decider \
                                 was called",
                                draft.risk_class, config.autonomy
                            )
                        };
                        self.record_decider_skip(
                            &mut next,
                            now,
                            DeciderSkip {
                                engine: config.decider_engine,
                                model: config.decider_model.as_deref(),
                                target: crate::job::DeciderTarget::Marker,
                                question: &draft.question,
                                options: &draft.options,
                                policy: crate::job::DeciderPolicy {
                                    kind,
                                    labelled_risk: draft.risk_class,
                                    effective_risk,
                                },
                                reason,
                                reported_kind: Some(draft.kind),
                                effect: Some(draft.effect),
                            },
                        );
                    }
                    match decision {
                        Decision::Escalate => escalating.push(open),
                        // Co-drafted auto-flow stops are recorded (for the note / the
                        // supervisor consult) but not persisted as open — matches
                        // `phase_engine`'s partition.
                        Decision::AutoFlow => auto.push((open.id, draft.clone())),
                    }
                }
                if auto.len() > crate::job::ADVICE_QUEUE_MAX {
                    let overflow = auto.split_off(crate::job::ADVICE_QUEUE_MAX);
                    for (id, draft) in overflow {
                        let effective_risk =
                            policy::effective_risk_kind(draft.kind, draft.risk_class);
                        self.record_decider_skip(
                            &mut next,
                            now,
                            DeciderSkip {
                                engine: config.decider_engine,
                                model: config.decider_model.as_deref(),
                                target: crate::job::DeciderTarget::Marker,
                                question: &draft.question,
                                options: &draft.options,
                                policy: crate::job::DeciderPolicy {
                                    kind: draft.kind,
                                    labelled_risk: draft.risk_class,
                                    effective_risk,
                                },
                                reason: format!(
                                    "the marker exceeded the bounded {}-decision review queue; \
                                     this overflow decision needs human review",
                                    crate::job::ADVICE_QUEUE_MAX
                                ),
                                reported_kind: Some(draft.kind),
                                effect: Some(draft.effect),
                            },
                        );
                        escalating.push(open_stop(
                            id,
                            StopKind::Capability,
                            draft.context_ref.clone(),
                            &draft.question,
                            &draft.options,
                            now,
                        ));
                    }
                }
                next.advice_queue = auto
                    .iter()
                    .map(|(stop_id, draft)| crate::job::QueuedAdvice {
                        stop_id: stop_id.clone(),
                        report_seq: next.report_generation,
                        draft: draft.clone(),
                    })
                    .collect();
                if !escalating.is_empty() {
                    next.finish_turn_from_report(now, &report, TurnDisposition::Escalated);
                    // NEEDING A HUMAN PAUSES THE HEARTBEAT; IT DOES NOT TOUCH THE DIAL. User:
                    // *"When its mention need me, may be it should not switch to Standard and still
                    // keep autopilot. But it will pause the heartbeat. Press a will answer and
                    // re-arm the heartbeat again on next tick."*
                    //
                    // That is what `JobRun::Blocked` already does for every other stop kind, and it
                    // needs no code here: `on_blocked` re-emits `Escalated` without nudging while a
                    // stop is unanswered, then resumes on the answer. The park IS the pause.
                    //
                    // m36 ALSO wrote `autonomy = Standard` here, for a `confirm_done`, reasoning
                    // that the dial was already the stop condition. It was the wrong lever, and the
                    // user found the dead end it made: `a` writes `answers.json`, which only pmd
                    // reads, and `pmd_drives_row` is false for a Standard row — so the harness
                    // raised a question and disabled the key that answers it in the same tick, then
                    // told the human to press Enter, which reaches the agent but never clears the
                    // stop. The dial and the park are two independent facts and conflating them is
                    // what made the row and the `a` key contradict each other.
                    //
                    // So the tier is left alone. `m`/`p`/`d` remain the human's levers, and a
                    // confirmed-done session that should stop being nudged is stopped by one of
                    // them rather than by the harness guessing.
                    //
                    // Any escalating stop parks the session for the human (mirrors
                    // `park_stuck_kind`'s persistence, but with the real drafted stops).
                    let ids: Vec<String> = escalating.iter().map(|s| s.id.clone()).collect();
                    let asked = escalating.first().and_then(|s| s.question.clone());
                    next.open_stops = escalating;
                    next.continuations = 0;
                    next.run = JobRun::Blocked {
                        stop_ids: ids.clone(),
                        since: now,
                    };
                    next.record_event(now, AutopilotEventKind::Escalated(asked.clone()));
                    next.record_decision(DecisionRecord::at(
                        now,
                        Some(report.seq),
                        DecisionKind::Escalated,
                        asked.clone(),
                        ids.clone(),
                    ));
                    // The seam captured the PRIOR open stops; refresh the snapshot with the
                    // stops the session now blocks on.
                    if let Some(sit) = next.situation.as_mut() {
                        sit.open_stops = ids.clone();
                    }
                    self.save_ledger(&mut next)?;
                    if let Err(e) =
                        self.append_decision_md(now, DecisionKind::Escalated, asked.as_deref())
                    {
                        eprintln!("pmd: {}: decisions.md append failed: {e}", self.project_id);
                    }
                    self.run = JobRun::Blocked {
                        stop_ids: ids.clone(),
                        since: now,
                    };
                    Ok(Some(JobTick::Escalated(ids)))
                } else {
                    next.finish_turn_from_report(now, &report, TurnDisposition::Reviewing);
                    // Typed policy found these stops eligible for independent review; it has not
                    // approved them. The first reaches the decider now and siblings remain queued
                    // so every decision receives its own validated verdict.
                    next.continuations = 0;
                    match self.spawn_advice(
                        driver,
                        now,
                        &next,
                        &auto,
                        config.decider_engine,
                        config.decider_model.as_deref(),
                    )? {
                        super::supervisor::AdviceSpawn::Spawned(tick) => return Ok(Some(tick)),
                        super::supervisor::AdviceSpawn::Unconsultable => {
                            return Ok(Some(self.park_unresolved_auto_stops(
                                now,
                                &next,
                                config,
                                UnresolvedAdvice {
                                    report_seq: next.report_generation,
                                    stops: &auto[..1],
                                    reason: "the current decision is malformed, oversized, or \
                                             otherwise not safely consultable; it needs human \
                                             review",
                                    preserve_queued_siblings: true,
                                },
                            )?));
                        }
                        super::supervisor::AdviceSpawn::Empty
                        | super::supervisor::AdviceSpawn::Unavailable => {}
                    }
                    let unavailable_reason = self
                        .advise_health
                        .reason
                        .as_deref()
                        .map(|reason| {
                            format!(
                                "decider is latched off ({reason}); no decision was auto-approved"
                            )
                        })
                        .unwrap_or_else(|| {
                            "decider unavailable; queued decisions need human review".into()
                        });
                    Ok(Some(self.park_unresolved_auto_stops(
                        now,
                        &next,
                        config,
                        UnresolvedAdvice {
                            report_seq: next.report_generation,
                            stops: &auto,
                            reason: &unavailable_reason,
                            preserve_queued_siblings: false,
                        },
                    )?))
                }
            }
        }
    }

    pub(super) fn park_unresolved_auto_stops(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
        config: &Config,
        unresolved: UnresolvedAdvice<'_>,
    ) -> Result<JobTick> {
        let mut next = base.clone();
        if unresolved.preserve_queued_siblings {
            let unresolved: std::collections::HashSet<&str> =
                unresolved.stops.iter().map(|(id, _)| id.as_str()).collect();
            next.advice_queue
                .retain(|queued| !unresolved.contains(queued.stop_id.as_str()));
        } else {
            next.advice_queue.clear();
        }
        next.finish_turn_review(
            unresolved.report_seq,
            crate::job::TurnReviewOutcome::Escalated,
        );
        let mut open = Vec::with_capacity(unresolved.stops.len());
        for (id, draft) in unresolved.stops {
            self.record_decider_skip(
                &mut next,
                now,
                DeciderSkip {
                    engine: config.decider_engine,
                    model: config.decider_model.as_deref(),
                    target: crate::job::DeciderTarget::Marker,
                    question: &draft.question,
                    options: &draft.options,
                    policy: crate::job::DeciderPolicy {
                        kind: draft.kind,
                        labelled_risk: draft.risk_class,
                        effective_risk: policy::effective_risk_kind(draft.kind, draft.risk_class),
                    },
                    reason: unresolved.reason.into(),
                    reported_kind: Some(draft.kind),
                    effect: Some(draft.effect),
                },
            );
            open.push(open_stop(
                id.clone(),
                StopKind::Capability,
                draft.context_ref.clone(),
                &draft.question,
                &draft.options,
                now,
            ));
        }
        let ids: Vec<String> = open.iter().map(|stop| stop.id.clone()).collect();
        let asked = open.first().and_then(|stop| stop.question.clone());
        next.open_stops = open;
        next.continuations = 0;
        next.run = JobRun::Blocked {
            stop_ids: ids.clone(),
            since: now,
        };
        next.record_event(now, AutopilotEventKind::Escalated(asked.clone()));
        next.record_decision(DecisionRecord::at(
            now,
            Some(unresolved.report_seq),
            DecisionKind::Escalated,
            asked.clone(),
            ids.clone(),
        ));
        if let Some(situation) = next.situation.as_mut() {
            situation.open_stops = ids.clone();
        }
        self.save_ledger(&mut next)?;
        if let Err(error) = self.append_decision_md(now, DecisionKind::Escalated, asked.as_deref())
        {
            eprintln!(
                "pmd: {}: decisions.md append failed: {error}",
                self.project_id
            );
        }
        self.run = JobRun::Blocked {
            stop_ids: ids.clone(),
            since: now,
        };
        Ok(JobTick::Escalated(ids))
    }

    /// Missing-parse fallback (mode B, past the OQ3 grace window): bump the stall
    /// counter ONCE for this bump; at `config.stuck_threshold` raise a `Stuck`
    /// escalation (never a silent stop — honors `parse_report`'s contract), else park a
    /// short recheck so the malformed file is re-observed promptly. The caller has
    /// already recorded the mtime as seen, so the same bad file is not re-counted every
    /// sweep.
    fn lenient_working(
        &mut self,
        now: Epoch,
        ledger: &AgentLoopState,
        config: &Config,
    ) -> Result<JobTick> {
        let mut next = ledger.clone();
        next.continuations = ledger.continuations.saturating_add(1);
        if next.continuations >= config.stuck_threshold {
            // Pass the bumped clone so the parked ledger carries the final count.
            return self.park_stuck(
                now,
                &next,
                "agent's decision marker stayed unparseable — no parseable progress".into(),
            );
        }
        let until = now + BUSY_RECHECK_S;
        next.run = JobRun::Monitoring { until };
        next.updated_at = now;
        self.save_ledger(&mut next)?;
        self.run = JobRun::Monitoring { until };
        Ok(JobTick::Monitoring { until })
    }
}

/// Parse a [`WakeReport`] the agent wrote as its final act. A missing/unparseable
/// report is an `Err` the caller turns into a lenient `working` bump (never a
/// silent stop). Mirrors [`crate::worker::parse_result`] for the phase worker.
pub fn parse_report(path: &Path) -> Result<WakeReport> {
    state::read_json(path)
}

/// A RECOVERY parse for a marker that FAILED the strict [`parse_report`]. `WakeReport` is
/// `#[serde(deny_unknown_fields)]`, so a single typo'd or extra key rejects the ENTIRE report
/// — which would silently downgrade a `blocked` escalation to a lenient `working` bump. This
/// strips any key the schema does not know, then parses strictly: an escalation carrying one
/// stray field is recovered (and still escalates), while genuinely malformed JSON or an
/// invalid `state` still fails here and falls through to `lenient_working` (drift still
/// surfaces). `KNOWN` mirrors `WakeReport`'s fields — kept honest by
/// `lenient_parse_key_list_matches_wakereport` in the tests.
fn parse_report_lenient_bytes(raw: &[u8]) -> Result<WakeReport> {
    let mut val: serde_json::Value = serde_json::from_slice(raw)?;
    if let Some(obj) = val.as_object_mut() {
        obj.retain(|k, _| WAKEREPORT_FIELDS.contains(&k.as_str()));
    }
    Ok(serde_json::from_value(val)?)
}

/// The top-level keys the [`WakeReport`] schema accepts, used by [`parse_report_lenient`] to
/// strip unknowns. MUST stay in sync with the struct (a test enforces it).
const WAKEREPORT_FIELDS: &[&str] = &[
    "state",
    "seq",
    "stops",
    "next_check_s",
    "cadence_s",
    "status",
    "next_step",
    "conversation_id",
];

/// OQ3 write/read-race guard (design §7.3): whether the marker file was last modified
/// within [`MARKER_MIDWRITE_GRACE`] of REAL wall-clock now — i.e. so recently that our
/// read may have raced the agent's write. Measured against the wall clock (not the
/// scheduler's logical `Epoch`, which can be a fake in tests) because it is a physical
/// write-race question. A modified-time in the FUTURE (clock skew) is treated as
/// mid-write (conservative — recheck rather than count a stall).
fn marker_is_mid_write(mtime: SystemTime) -> bool {
    match mtime.elapsed() {
        Ok(elapsed) => elapsed < MARKER_MIDWRITE_GRACE,
        Err(_) => true,
    }
}

#[cfg(test)]
mod field_sync_tests {
    use super::*;
    use crate::job::{WakeReport, WakeState};

    #[test]
    fn wakereport_fields_list_matches_the_struct() {
        // Compile-forced awareness: adding a WakeReport field breaks THIS construction, and the
        // key-set assertion then forces updating WAKEREPORT_FIELDS — else the new field would be
        // silently stripped on the lenient recovery path.
        let full = WakeReport {
            state: WakeState::Working,
            seq: 1,
            stops: vec![],
            next_check_s: Some(1),
            cadence_s: Some(1),
            status: Some("s".into()),
            next_step: Some("n".into()),
            conversation_id: Some("c".into()),
        };
        let val = serde_json::to_value(&full).unwrap();
        let mut keys: Vec<String> = val
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_string())
            .collect();
        keys.sort();
        let mut known: Vec<String> = WAKEREPORT_FIELDS.iter().map(|s| s.to_string()).collect();
        known.sort();
        assert_eq!(
            keys, known,
            "WAKEREPORT_FIELDS drifted from WakeReport's fields"
        );
    }
}
