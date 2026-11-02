//! Is the engine's turn-end hook actually firing?
//!
//! The hook appends one byte per completed turn to
//! [`ProjectPaths::turn_signal`](super::ProjectPaths::turn_signal), and pmd compares that count
//! against the count it recorded when it last nudged. That comparison is the only thing that can
//! tell claude's bare composer mid-tool-call apart from claude's bare composer at an ended turn,
//! so a hook that stops firing does not break loudly — it makes every awaiting-report hold run to
//! the report-debt ceiling instead.
//!
//! ONE RULE, TWO READERS, so they can never disagree about what "the hook is dead" means: `pmd
//! doctor` reports it when asked, and the report-debt backstop names it as the cause of a hold it
//! would otherwise have blamed on the agent. Two copies of this comparison is how a diagnostic
//! ends up describing a different fault from the one the scheduler acted on.

use std::path::Path;

/// How far `report_generation` may legitimately run ahead of the turn-end signal before the hook
/// is judged to be missing turns. Small but non-zero: a turn ends before its notification lands,
/// and the marker disposer bumps its own generation on paths that never involved a turn at all.
pub const TURN_SIGNAL_LAG_MAX: u64 = 5;

/// What the turn-end signal says about the hook that writes it, read against how many reports the
/// worker has had accepted. The two unhealthy variants carry their counts because the numbers are
/// the evidence — a human told "the hook is dead" deserves to see what it was measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnSignalHealth {
    /// No signal file, and too few accepted reports for its absence to mean anything yet. A fresh
    /// session legitimately has neither, so this is never a fault.
    Fresh,
    /// No signal file at all, though the worker has reported repeatedly: the hook has never once
    /// fired in this session.
    NeverFired { reports: u64 },
    /// Firing, but for far fewer turns than the worker has reported — intermittent rather than
    /// absent, which is how a live codex session held for 3033 seconds.
    Partial { turns: u64, reports: u64 },
    /// Firing in step with the reports.
    Healthy { turns: u64 },
}

impl TurnSignalHealth {
    /// Whether pmd is BLIND to turn ends through no fault of the agent. True only for the two
    /// diagnosable faults: [`Self::Fresh`] is indistinguishable from a new session, so it is not a
    /// finding, and claiming it as one would blame the harness for the ordinary first few minutes.
    pub fn is_blind(self) -> bool {
        matches!(self, Self::NeverFired { .. } | Self::Partial { .. })
    }
}

/// Classify the turn-end signal at `path` against `report_generation` accepted reports.
///
/// Read-only and panic-free: a missing file, an unreadable one and a permission error all read as
/// "no signal", because none of them is a reason to fail a tick.
pub fn turn_signal_health(path: &Path, report_generation: u64) -> TurnSignalHealth {
    match std::fs::metadata(path).map(|m| m.len()).ok() {
        None if report_generation <= TURN_SIGNAL_LAG_MAX => TurnSignalHealth::Fresh,
        None => TurnSignalHealth::NeverFired {
            reports: report_generation,
        },
        Some(turns) if report_generation.saturating_sub(turns) > TURN_SIGNAL_LAG_MAX => {
            TurnSignalHealth::Partial {
                turns,
                reports: report_generation,
            }
        }
        Some(turns) => TurnSignalHealth::Healthy { turns },
    }
}
