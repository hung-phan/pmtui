use serde::{Deserialize, Serialize};

use super::{AgentLoopState, WakeReport, WakeState};
use crate::clock::Epoch;

pub const TURN_TRACE_MAX: usize = 64;
const TURN_TEXT_MAX: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnTrigger {
    Heartbeat {
        pending_context: bool,
        marker_recovery: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnDisposition {
    Working,
    Monitoring,
    Reviewing,
    AutoFlow,
    Escalated,
    Interrupted,
    Stalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnReviewOutcome {
    AutoFlow,
    Escalated,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnNoReportReason {
    Superseded,
    Relaunched,
    TerminalUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    AwaitingReport,
    Reported {
        at: Epoch,
        #[serde(default)]
        report_generation: u64,
        marker_seq: u64,
        state: WakeState,
        disposition: TurnDisposition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_step: Option<String>,
    },
    NoReport {
        at: Epoch,
        reason: TurnNoReportReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnTrace {
    pub id: u64,
    pub started_at: Epoch,
    /// Worker audit stamp visible when the turn began.
    pub marker_baseline: u64,
    /// Pmd-owned report generation at turn start. `None` marks a pre-generation trace; its next
    /// accepted report may close it regardless of a poisoned worker marker baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_generation_baseline: Option<u64>,
    pub trigger: TurnTrigger,
    pub outcome: TurnOutcome,
}

fn cap_text(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= TURN_TEXT_MAX {
        return value;
    }
    let mut capped = value.chars().take(TURN_TEXT_MAX - 1).collect::<String>();
    capped.push('…');
    capped
}

pub(super) fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Vec<TurnTrace>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Array(values) = value else {
        return Ok(Vec::new());
    };
    let mut traces = values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect::<Vec<_>>();
    if traces.len() > TURN_TRACE_MAX {
        traces.drain(0..traces.len() - TURN_TRACE_MAX);
    }
    Ok(traces)
}

impl AgentLoopState {
    pub fn start_turn(&mut self, now: Epoch, trigger: TurnTrigger) -> u64 {
        if let Some(pending) = self
            .turn_trace
            .iter_mut()
            .rev()
            .find(|turn| matches!(turn.outcome, TurnOutcome::AwaitingReport))
        {
            pending.outcome = TurnOutcome::NoReport {
                at: now,
                reason: TurnNoReportReason::Superseded,
            };
        }

        self.turn_seq = self.turn_seq.saturating_add(1);
        self.turn_trace.push(TurnTrace {
            id: self.turn_seq,
            started_at: now,
            marker_baseline: self.last_marker_seq,
            report_generation_baseline: Some(self.report_generation),
            trigger,
            outcome: TurnOutcome::AwaitingReport,
        });
        if self.turn_trace.len() > TURN_TRACE_MAX {
            self.turn_trace
                .drain(0..self.turn_trace.len() - TURN_TRACE_MAX);
        }
        self.turn_seq
    }

    pub fn finish_turn_from_report(
        &mut self,
        now: Epoch,
        report: &WakeReport,
        disposition: TurnDisposition,
    ) -> bool {
        let Some(turn) = self
            .turn_trace
            .iter_mut()
            .rev()
            .find(|turn| matches!(turn.outcome, TurnOutcome::AwaitingReport))
        else {
            return false;
        };
        if turn
            .report_generation_baseline
            .is_some_and(|baseline| self.report_generation <= baseline)
        {
            return false;
        }
        turn.outcome = TurnOutcome::Reported {
            at: now,
            report_generation: self.report_generation,
            marker_seq: report.seq,
            state: report.state,
            disposition,
            status: report.status.as_deref().map(cap_text),
            next_step: report.next_step.as_deref().map(cap_text),
        };
        true
    }

    pub fn finish_turn_review(
        &mut self,
        report_generation: u64,
        outcome: TurnReviewOutcome,
    ) -> bool {
        let Some(disposition) = self.turn_trace.iter_mut().rev().find_map(|turn| {
            let TurnOutcome::Reported {
                report_generation: turn_generation,
                marker_seq,
                disposition,
                ..
            } = &mut turn.outcome
            else {
                return None;
            };
            ((*turn_generation == report_generation
                || (*turn_generation == 0 && *marker_seq == report_generation))
                && *disposition == TurnDisposition::Reviewing)
                .then_some(disposition)
        }) else {
            return false;
        };
        *disposition = match outcome {
            TurnReviewOutcome::AutoFlow => TurnDisposition::AutoFlow,
            TurnReviewOutcome::Escalated => TurnDisposition::Escalated,
            TurnReviewOutcome::Interrupted => TurnDisposition::Interrupted,
        };
        true
    }

    pub fn finish_turn_without_report(&mut self, now: Epoch, reason: TurnNoReportReason) -> bool {
        let Some(turn) = self
            .turn_trace
            .iter_mut()
            .rev()
            .find(|turn| matches!(turn.outcome, TurnOutcome::AwaitingReport))
        else {
            return false;
        };
        turn.outcome = TurnOutcome::NoReport { at: now, reason };
        true
    }
}

#[cfg(test)]
mod tests;
