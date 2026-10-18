//! The autonomy policy: pure functions deciding whether a stop must escalate to
//! the human or may auto-flow, given the project tier and the stop's risk class
//! (design §8). This is the daemon's deterministic safety-net; the coordinator
//! applies the same rules to self-suppress low-stakes stops.

use crate::pmstate::StopKind;
use crate::state::{RiskClass, Stop, Tier};

/// What the daemon should do with a stop the coordinator raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Wake the human; the step stays parked until an answer arrives.
    Escalate,
    /// Safe to proceed without the human. (The coordinator should normally have
    /// resolved this already; this is the belt-and-suspenders path.)
    AutoFlow,
}

/// Stop kinds that are always `Hard` (irreversible / external), regardless of
/// what the coordinator wrote in `risk_class`. No tier can auto-flow these —
/// "safe by construction" (design §8; hardening R19).
pub const ALWAYS_HARD_KINDS: &[&str] = &[
    "publish",
    "deploy",
    "merge",
    "land",
    "confirm_done",
    "credentials",
    "payment",
    "destructive",
];

/// The effective risk the daemon gates on: a `kind` in [`ALWAYS_HARD_KINDS`] is
/// forced to `Hard` even if the coordinator labelled it lower.
pub fn effective_risk(stop: &Stop) -> RiskClass {
    if ALWAYS_HARD_KINDS.contains(&stop.kind.as_str()) {
        RiskClass::Hard
    } else {
        stop.risk_class
    }
}

/// The effective risk gated on for a typed [`StopKind`] (the harness ledger's
/// stop kind, distinct from the reference model's free-string `Stop.kind`). The
/// irreversible / external kinds — `Publish`, `Merge`, `ConfirmDone`, `Stuck`,
/// `Capability` — are forced to `Hard` regardless of the worker's `labelled`
/// risk. `Ambiguity`, `ExpertNeeded`, and `WorkerStuck` "default to Medium"
/// (spec §7): they are floored to at least `Medium` so a worker-labelled `Low`
/// still escalates under Standard (only Autopilot auto-flows Medium).
/// This is the typed twin of [`effective_risk`], used by the phase engine.
pub fn effective_risk_kind(kind: StopKind, labelled: RiskClass) -> RiskClass {
    match kind {
        StopKind::Publish
        | StopKind::Merge
        | StopKind::ConfirmDone
        | StopKind::Stuck
        | StopKind::Capability => RiskClass::Hard,
        StopKind::Ambiguity | StopKind::ExpertNeeded | StopKind::WorkerStuck => {
            labelled.max(RiskClass::Medium)
        }
    }
}

/// Decide for a typed `(tier, kind, labelled-risk)` triple, applying the
/// kind-based hard floor. `= decide(tier, effective_risk_kind(kind, labelled))`.
pub fn decide_kind(tier: Tier, kind: StopKind, labelled: RiskClass) -> Decision {
    // In Autopilot, ordinary worker uncertainty is reviewed by the goal-aware decider even when
    // the worker labelled it Hard. The label remains in the audit; it is evidence, not authority
    // to force a human interruption. Human-owned kinds still take the hard floor below.
    if tier == Tier::Autopilot
        && matches!(
            kind,
            StopKind::Ambiguity | StopKind::ExpertNeeded | StopKind::WorkerStuck
        )
    {
        return Decision::AutoFlow;
    }
    decide(tier, effective_risk_kind(kind, labelled))
}

/// Decide for a `(tier, risk)` pair. `Hard` always escalates, every tier.
pub fn decide(tier: Tier, risk: RiskClass) -> Decision {
    match (tier, risk) {
        (_, RiskClass::Hard) => Decision::Escalate,
        (Tier::Autopilot, RiskClass::Medium) => Decision::AutoFlow,
        (_, RiskClass::Medium) => Decision::Escalate,
        (_, RiskClass::Low) => Decision::AutoFlow,
    }
}

/// Decide for a concrete stop, applying the kind-based hard floor.
pub fn decide_stop(tier: Tier, stop: &Stop) -> Decision {
    decide(tier, effective_risk(stop))
}

/// The stops that require the human. Empty => the project may auto-flow.
pub fn stops_requiring_human(tier: Tier, stops: &[Stop]) -> Vec<&Stop> {
    stops
        .iter()
        .filter(|s| decide_stop(tier, s) == Decision::Escalate)
        .collect()
}

#[cfg(test)]
mod tests;
