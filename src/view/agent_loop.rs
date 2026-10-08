//! The `Mode::AgentLoop` half of the view: derive a row from a session's PER-SESSION
//! ledger (`sessions/<id>/state.json`) instead of the phase/coordinator files the rest
//! of this module reads. Separate because an agent-loop session shares nothing with the
//! phase path but the [`ProjectView`] shape — its posture, stops and hint all come from
//! [`AgentLoopState`], and it must agree with the daemon's escalations rather than with
//! `derive_posture`.

use crate::clock::Epoch;
use crate::job::{self, AgentLoopState, JobRun};
use crate::pmstate::{OpenStop, StopKind, StopStatus};
use crate::policy;
use crate::registry::Mode;
use crate::state::{self, Config, ProjectPaths, RiskClass, Tier};

use super::{Posture, ProjectView};

impl ProjectView {
    /// Read a `Mode::AgentLoop` session into a view from its PER-SESSION ledger
    /// (`sessions/<id>/`), NOT the phase/coordinator files [`ProjectView::read`]
    /// consumes. An agent-loop session's truth is its [`AgentLoopState`]: `stops`
    /// come from the ledger's `open_stops`, `posture` from its `run`, `tier` from
    /// the per-session `config.json`, and `next_action` is a short human hint.
    /// Tolerates a missing/malformed ledger (renders Fresh with no stops) so a
    /// half-seeded session still shows rather than crashing the TUI. Mirrors the
    /// daemon's `open_stops_for_display`/`job_stops` pattern (`src/daemon/stops.rs`) so
    /// the dashboard and the escalations agree on severity.
    pub fn read_agent_loop(
        id: &str,
        session_paths: &ProjectPaths,
        enabled: bool,
        now: Epoch,
    ) -> ProjectView {
        let config = state::read_json_opt::<Config>(&session_paths.config())
            .ok()
            .flatten();
        let ledger = job::load(session_paths).ok().flatten();
        let tier = config.as_ref().map(|c| c.autonomy);

        // Working (mid-turn) vs waiting (idle at prompt) is a pmd-DRIVEN notion, read LIVE from
        // the turn-end signal file — the daemon can't surface this (it is parked `Monitoring`
        // and blind between nudges), but the hook updates the file in real time.
        //
        // It is meaningful ONLY for a session pmd actually nudges (Autopilot). A STANDARD
        // session is human-driven and pmd never nudges it (`daemon::pmd_drives_row`), so there
        // is no "next nudge" to be mid-turn toward: force `None` so `status_category` buckets it
        // on plain REPL liveness (running/idle), not on a working/waiting sub-state read off a
        // frozen ledger.
        let working = if tier == Some(Tier::Standard) {
            None
        } else {
            ledger
                .as_ref()
                .and_then(|l| agent_loop_working(l, session_paths))
        };
        let (mut posture, stops, next_action) = match ledger.as_ref() {
            Some(l) => (
                agent_loop_posture(l),
                agent_loop_stops(&l.open_stops),
                agent_loop_next_action(l, now, working, tier),
            ),
            None => (Posture::Fresh, Vec::new(), String::new()),
        };

        // AUTOPILOT-OFF HONESTY. With the tier at `Standard` the daemon skips this
        // session's WHOLE tick by design (`daemon::pmd_drives_row`), so a marker the
        // agent wrote while off is never folded into the ledger — the ledger stays `Idle`
        // with no open stops, and the row drew an idle glyph (and the header tallied an
        // idle session) for an agent that was actually blocked and waiting on a human.
        //
        // So consult the agent's own machine report DIRECTLY. A pure file read, which is
        // what keeps this module READ-ONLY over state: only `pmd` writes `state.json`,
        // and nothing here writes anything. The preview does exactly this read one
        // function away (`pmtui::render_preview` via `job_engine::parse_report`).
        //
        // Deliberately NOT a re-opening of the daemon's tick for an OFF row: that gate is
        // a whole-tick `return`, and the ruling is that autopilot off means the harness
        // does not touch the session. This only changes what the DASHBOARD says.
        //
        // Gated on the ledger having NO open stops: a real open stop already drives the
        // posture through `agent_loop_posture` (including the stuck-kind burn), and the
        // marker must never soften or override it.
        //
        // `stops` is left EMPTY on purpose, and that is the point of the whole gate.
        // Nothing here becomes ANSWERABLE — folding a marker into open stops is `pmd`'s
        // job, and with autopilot off `pmd` would never deliver the answer. Only the
        // glyph, the header tally, and the message pmtui's answer key (`s`) can give become
        // honest; no answer is offered that would evaporate.
        if tier == Some(Tier::Standard)
            && stops.is_empty()
            && let Ok(Some(report)) =
                state::read_json_opt::<job::WakeReport>(&session_paths.needs_you())
            && report.state == job::WakeState::Blocked
        {
            posture = Posture::NeedsYou;
        }

        let (
            autopilot_events,
            turn_trace,
            decision_digest,
            advice_inflight,
            advice_queue,
            decider_runs,
        ) = ledger
            .as_ref()
            .map(|ledger| {
                (
                    ledger.events.clone(),
                    ledger.turn_trace.clone(),
                    ledger.digest,
                    ledger.advice_inflight.clone(),
                    ledger.advice_queue.clone(),
                    ledger.decider_runs.clone(),
                )
            })
            .unwrap_or_default();

        ProjectView {
            id: id.to_string(),
            display_name: None,
            work_summary: None,
            project_name: None,
            forked_from: None,
            incomplete_fork: false,
            spawned_by: None,
            spawned_by_label: None,
            spawn_staged: false,
            job: false,
            job_commit: None,
            job_branch: None,
            enabled,
            tier,
            posture,
            next_action,
            // An agent-loop session isn't phase-stepped; step_id is unused for it.
            step_id: 0,
            last_activity: ledger.as_ref().map(|l| l.updated_at),
            // `stops` may have been left empty by the autopilot-off gate above, so read
            // the age from the LEDGER rather than from `stops` — the two can disagree,
            // and an age with nothing to attach it to would be a lie about what is open.
            oldest_stop_since: ledger
                .as_ref()
                .and_then(|l| l.open_stops.iter().map(|s| s.first_posted).min())
                .filter(|_| !stops.is_empty()),
            stops,
            // Registry-sourced fields; the caller (pmtui) fills these in.
            mode: Mode::AgentLoop,
            engine: None,
            session_live: false,
            human_attached: false,
            agent_working: working,
            // pmd's decision feed and structured audit, verbatim off the ledger.
            // Empty when there is no ledger yet; the preview shows nothing until pmd acts.
            autopilot_events,
            turn_trace,
            decision_digest,
            advice_inflight,
            decider_live: false,
            advice_queue,
            decider_runs,
            decider_engine: config.as_ref().map(|config| config.decider_engine),
            decider_model: config.and_then(|config| config.decider_model),
        }
    }

    /// Apply a live pane observation to an already-read agent-loop view and keep its human-facing
    /// next action consistent with the reconciled activity.
    pub fn apply_agent_working(
        &mut self,
        ledger: &AgentLoopState,
        now: Epoch,
        working: Option<bool>,
    ) {
        self.agent_working = working;
        self.next_action = agent_loop_next_action(ledger, now, working, self.tier);
    }
}

/// Working (mid-turn) vs waiting (idle at prompt) for a DRIVEN agent-loop session, or `None`
/// when it can't be told. Compares the LIVE turn-end signal file (`turn_signal`, which the
/// engine's hook appends one byte to per completed turn) against the ledger's
/// `turn_count_at_nudge` baseline (the count at the last nudge): size == baseline ⇒ the
/// nudged turn has not completed ⇒ WORKING (`Some(true)`); size > baseline ⇒ a turn completed
/// since the nudge ⇒ idle at prompt, WAITING (`Some(false)`).
///
/// `None` when this isn't a driven `Monitoring` row, or the hook isn't wired (no signal file
/// / a pre-M72 ledger with no baseline) — in which case the display keeps the M72 default of
/// showing a driven session as running. A pure file stat, no subprocess, so an agent-loop row
/// still costs no `tmux` fork per refresh.
fn agent_loop_working(l: &AgentLoopState, paths: &ProjectPaths) -> Option<bool> {
    if !matches!(l.run, JobRun::Monitoring { .. }) {
        return None;
    }
    // A disposed Monitoring report is pmd-owned proof that the worker yielded this wake. Codex's
    // optional notify hook can miss a completed turn, leaving the signal size equal to the old
    // nudge baseline forever. Do not let that stale byte count hide the scheduled countdown. A
    // newer outstanding nudge still wins, and pmtui's live pane classifier can independently
    // restore `working` when the terminal is actually busy.
    if !l.awaiting_report()
        && l.situation.as_ref().is_some_and(|situation| {
            situation.seq == l.last_marker_seq
                && situation.state == crate::job::WakeState::Monitoring
        })
    {
        return Some(false);
    }
    let baseline = l.turn_count_at_nudge?;
    let size = std::fs::metadata(paths.turn_signal())
        .map(|m| m.len())
        .ok()?;
    Some(size <= baseline)
}

/// Glanceable posture for an agent-loop session from its ledger `run`. A `Blocked`
/// park is `NeedsYou`, unless one of its open stops is a stuck-kind dead-end
/// (`Stuck`/`WorkerStuck`) — then it burns as the alarming `Stuck` posture.
fn agent_loop_posture(l: &AgentLoopState) -> Posture {
    match &l.run {
        JobRun::Idle => Posture::Fresh,
        JobRun::Running { .. } => Posture::Running,
        JobRun::Monitoring { .. } => Posture::Monitoring,
        JobRun::Blocked { .. } => {
            if l.open_stops
                .iter()
                .any(|s| matches!(s.kind, StopKind::Stuck | StopKind::WorkerStuck))
            {
                Posture::Stuck
            } else {
                Posture::NeedsYou
            }
        }
    }
}

/// Convert the ledger's `open_stops` into the display [`state::Stop`] shape the TUI
/// renders. Mirrors the daemon's `open_stops_for_display`: `risk_class` is derived
/// from the typed [`StopKind`] (floored to at least `Medium`, forced `Hard` for the
/// irreversible/dead-end kinds) so a `stuck`/`capability` stop — absent from the
/// reference `ALWAYS_HARD_KINDS` string set — still renders Hard. The agent's own
/// `question`/`options` (when the ledger carries them — older ledgers and
/// synthesized stops don't) come through verbatim; an absent question stays empty
/// and the renderer falls back to the kind name.
fn agent_loop_stops(open_stops: &[OpenStop]) -> Vec<state::Stop> {
    open_stops
        .iter()
        .map(|s| state::Stop {
            id: s.id.clone(),
            kind: stop_kind_name(s.kind),
            risk_class: policy::effective_risk_kind(s.kind, RiskClass::Medium),
            question: s.question.clone().unwrap_or_default(),
            options: s.options.clone(),
            context_ref: s.context_ref.clone(),
            status: match s.status {
                StopStatus::AwaitingReply => "awaiting_reply".to_string(),
                StopStatus::Held => "held".to_string(),
            },
        })
        .collect()
}

/// A short human hint for the preview/row `next:` field, per ledger `run`. `working` is the
/// live turn-end sub-state ([`agent_loop_working`]): `Some(true)` = mid-turn, `Some(false)` =
/// idle at prompt between nudges, `None` = unknown (no hook / not driven). `tier` gates the
/// pmd-cadence wording: for a Standard (human-driven) row there is no pmd nudge to count down
/// to, so it must never print one.
fn agent_loop_next_action(
    l: &AgentLoopState,
    now: Epoch,
    working: Option<bool>,
    tier: Option<Tier>,
) -> String {
    // A STANDARD session is HUMAN-driven — pmd never nudges it (`daemon::pmd_drives_row`), so a
    // cadence countdown or a "monitoring · check in HH:MM:SS"/"waiting" hint would promise pmd
    // activity that never comes (the same class of lie the autopilot countdown fixes guard
    // against). Its ledger `run` is also a frozen snapshot (often a leftover `Monitoring` after
    // an Autopilot→Standard flip), so it is not the truth either. Say who holds the wheel instead;
    // a genuine blocked ask still surfaces via the posture/glyph and the answer panel.
    if tier == Some(Tier::Standard) {
        return match &l.run {
            JobRun::Blocked { .. } => {
                let label = l
                    .open_stops
                    .first()
                    .map(|s| stop_kind_name(s.kind))
                    .unwrap_or_else(|| "decision".to_string());
                format!("blocked · {label}")
            }
            _ => "you drive it".to_string(),
        };
    }
    match &l.run {
        JobRun::Idle => "idle".to_string(),
        JobRun::Running { .. } => "working".to_string(),
        JobRun::Monitoring { until } => {
            // MID-TURN per the turn-end signal ⇒ the agent is actively WORKING. Say so
            // plainly, above every countdown: the next nudge is DEFERRED until the turn ends
            // (`drive`'s turn-in-progress guard), so a countdown here would promise a send
            // that will not fire — the same lie the `confirming…` floor below fixes.
            if working == Some(true) {
                return "working".to_string();
            }
            // A COUNTDOWN ONLY WHEN IT MEANS SOMETHING. While a nudge is outstanding
            // (`awaiting_report`), `JobScheduler::drive` sends nothing whatever this timer says — it
            // re-parks a few seconds at a time until the agent reports or the stall backstop
            // escalates. The old text counted down to 0 anyway, which is what the user saw: *"The
            // check in go to 0 but no message is sent"*. The timer was true about the ledger and
            // silent about the brake.
            if l.awaiting_report() {
                return "waiting for the agent to report".to_string();
            }
            let secs = (until - now).max(0);
            // A COUNTDOWN THAT NEVER RESETS THROUGH ZERO. The last `BUSY_RECHECK_S` of any
            // monitoring park is `drive`'s confirmation/recheck brake: when it expires the
            // daemon re-reads the pane and, if it is still confirming — the first of two
            // consecutive Idle observations, a transcript still changing between captures,
            // or a Busy/awaiting-report pane — re-parks ANOTHER short recheck and sends
            // NOTHING. The old text counted this window down to `0s` and then visibly reset
            // to 5s, which the user read as a broken timer: *"when the timer countdown to 0s
            // on autopilot, it doesn't send immediately. it resets to 5 or 10s then it sends
            // another one. This sets false expectation."* So the tail window shows the
            // honest state instead of promising a send at a zero the drive path structurally
            // withholds. `awaiting_report` (masked above) is the one recheck that has its own
            // clearer wording; every other one lands here.
            if secs <= crate::job_engine::BUSY_RECHECK_S {
                return "monitoring · confirming…".to_string();
            }
            // Between nudges: `Some(false)` = the turn finished and the agent is idle at its
            // prompt, so it reads "waiting"; an unknown sub-state (`None`, no hook) keeps the
            // neutral "monitoring". Either way the countdown is honest — the agent is idle and
            // the nudge fires when it comes due.
            let label = if working == Some(false) {
                "waiting"
            } else {
                "monitoring"
            };
            format!("{label} · check in {}", format_duration_hms(secs))
        }
        JobRun::Blocked { .. } => {
            let label = l
                .open_stops
                .first()
                .map(|s| stop_kind_name(s.kind))
                .unwrap_or_else(|| "decision".to_string());
            format!("blocked · {label}")
        }
    }
}

fn format_duration_hms(total_seconds: i64) -> String {
    let total_seconds = total_seconds.max(0);
    let hours = total_seconds / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// The snake_case wire name of a [`StopKind`] (deferring to serde rather than
/// duplicating the mapping), so the converted `Stop.kind` matches the strings
/// `policy::effective_risk`/`escalation` gate on. Mirrors
/// `daemon::stops::stop_kind_name`.
fn stop_kind_name(kind: StopKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
