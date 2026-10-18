//! The SITUATION a decider is shown: the ledger projected down to what one consult needs.
//!
//! Split out of `job.rs` with the decider records and the event log. Its own file because it is a
//! PROJECTION and nothing else — a read of state that crosses the seam into a prompt, which is the
//! one place where what the ledger holds and what a model sees have to be told apart.
//!
//! Keeps its path (`job::LedgerSituation`) through a re-export in the parent.

use serde::{Deserialize, Serialize};

use super::{WakeReport, WakeState};
use crate::clock::Epoch;

/// A snapshot of the last report `pmd` disposed — the current "situation" the supervisor
/// projection (Milestone C) reads. Projected fresh at spawn, but persisted here so it
/// never goes stale across a park (set by the single dispose seam on EVERY accepted bump).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSituation {
    pub state: WakeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_stops: Vec<String>,
    pub seq: u64,
    pub at: Epoch,
}

impl LedgerSituation {
    /// Snapshot the just-disposed `report` (its state/status/seq) plus the stop ids the
    /// session is open on at disposition time.
    pub fn from_report(report: &WakeReport, open_stops: &[String], now: Epoch) -> Self {
        Self {
            state: report.state,
            status: report.status.clone(),
            open_stops: open_stops.to_vec(),
            seq: report.seq,
            at: now,
        }
    }
}
