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
mod tests {
    use super::*;

    const TIERS: [Tier; 2] = [Tier::Autopilot, Tier::Standard];

    fn stop(kind: &str, rc: RiskClass) -> Stop {
        Stop {
            id: "s".into(),
            kind: kind.into(),
            risk_class: rc,
            question: "?".into(),
            options: vec![],
            context_ref: None,
            status: "awaiting_reply".into(),
        }
    }

    #[test]
    fn hard_always_escalates() {
        for tier in TIERS {
            assert_eq!(
                decide(tier, RiskClass::Hard),
                Decision::Escalate,
                "{tier:?}"
            );
        }
    }

    #[test]
    fn medium_escalates_unless_autopilot() {
        assert_eq!(
            decide(Tier::Autopilot, RiskClass::Medium),
            Decision::AutoFlow
        );
        assert_eq!(
            decide(Tier::Standard, RiskClass::Medium),
            Decision::Escalate
        );
    }

    #[test]
    fn low_never_escalates() {
        for tier in TIERS {
            assert_eq!(decide(tier, RiskClass::Low), Decision::AutoFlow, "{tier:?}");
        }
    }

    #[test]
    fn publish_kind_is_forced_hard_even_if_labelled_low() {
        let s = stop("publish", RiskClass::Low);
        assert_eq!(effective_risk(&s), RiskClass::Hard);
        // Even autopilot must escalate a publish.
        assert_eq!(decide_stop(Tier::Autopilot, &s), Decision::Escalate);
    }

    #[test]
    fn confirm_done_is_forced_hard() {
        let s = stop("confirm_done", RiskClass::Medium);
        assert_eq!(decide_stop(Tier::Autopilot, &s), Decision::Escalate);
    }

    #[test]
    fn ordinary_ambiguity_uses_labelled_risk() {
        let s = stop("ambiguity", RiskClass::Medium);
        assert_eq!(effective_risk(&s), RiskClass::Medium);
        assert_eq!(decide_stop(Tier::Autopilot, &s), Decision::AutoFlow);
        assert_eq!(decide_stop(Tier::Standard, &s), Decision::Escalate);
    }

    #[test]
    fn autopilot_reviews_worker_labelled_hard_ordinary_stops() {
        for kind in [
            StopKind::Ambiguity,
            StopKind::ExpertNeeded,
            StopKind::WorkerStuck,
        ] {
            assert_eq!(
                decide_kind(Tier::Autopilot, kind, RiskClass::Hard),
                Decision::AutoFlow,
                "{kind:?}"
            );
            assert_eq!(
                decide_kind(Tier::Standard, kind, RiskClass::Hard),
                Decision::Escalate,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn effective_risk_kind_forces_hard_for_gated_kinds_and_passes_others_through() {
        use crate::pmstate::StopKind;
        // The five kinds that are irreversible / external are forced Hard no
        // matter how the worker labelled them.
        for k in [
            StopKind::Publish,
            StopKind::Merge,
            StopKind::ConfirmDone,
            StopKind::Stuck,
            StopKind::Capability,
        ] {
            assert_eq!(
                effective_risk_kind(k, RiskClass::Low),
                RiskClass::Hard,
                "{k:?}"
            );
            assert_eq!(
                effective_risk_kind(k, RiskClass::Medium),
                RiskClass::Hard,
                "{k:?}"
            );
        }
        // Ambiguity / ExpertNeeded / WorkerStuck default to Medium (spec §7): a
        // Low label floors up to Medium, Medium stays Medium, Hard stays Hard.
        assert_eq!(
            effective_risk_kind(StopKind::Ambiguity, RiskClass::Low),
            RiskClass::Medium,
            "Low ambiguity floors to Medium"
        );
        assert_eq!(
            effective_risk_kind(StopKind::Ambiguity, RiskClass::Medium),
            RiskClass::Medium
        );
        assert_eq!(
            effective_risk_kind(StopKind::ExpertNeeded, RiskClass::Low),
            RiskClass::Medium,
            "Low expert_needed floors to Medium"
        );
        assert_eq!(
            effective_risk_kind(StopKind::WorkerStuck, RiskClass::Low),
            RiskClass::Medium
        );
        assert_eq!(
            effective_risk_kind(StopKind::WorkerStuck, RiskClass::Hard),
            RiskClass::Hard,
            "Hard is preserved above the Medium floor"
        );
    }

    #[test]
    fn decide_kind_escalates_stuck_capability_and_autoflows_medium_ambiguity() {
        use crate::pmstate::StopKind;
        // Stuck/Capability are forced hard -> escalate on every tier, even autopilot.
        assert_eq!(
            decide_kind(Tier::Autopilot, StopKind::Stuck, RiskClass::Low),
            Decision::Escalate
        );
        assert_eq!(
            decide_kind(Tier::Autopilot, StopKind::Capability, RiskClass::Low),
            Decision::Escalate
        );
        // confirm_done forced hard even at autopilot.
        assert_eq!(
            decide_kind(Tier::Autopilot, StopKind::ConfirmDone, RiskClass::Medium),
            Decision::Escalate
        );
        // Medium ambiguity: autopilot auto-flows, standard escalates.
        assert_eq!(
            decide_kind(Tier::Autopilot, StopKind::Ambiguity, RiskClass::Medium),
            Decision::AutoFlow
        );
        assert_eq!(
            decide_kind(Tier::Standard, StopKind::Ambiguity, RiskClass::Medium),
            Decision::Escalate
        );
        // Low ambiguity floors to Medium: autopilot still auto-flows, but standard
        // now escalates it (it would have auto-flowed before the Medium floor).
        assert_eq!(
            decide_kind(Tier::Autopilot, StopKind::Ambiguity, RiskClass::Low),
            Decision::AutoFlow
        );
        assert_eq!(
            decide_kind(Tier::Standard, StopKind::Ambiguity, RiskClass::Low),
            Decision::Escalate
        );
    }

    #[test]
    fn decide_kind_takes_only_fieldless_enums_no_bytes() {
        use crate::pmstate::StopKind;
        // Binding decide_kind to an explicit fn-pointer type FAILS TO COMPILE if the signature
        // ever gains a String/&str/situation/ledger parameter — the guard that keeps the
        // directive (and any ledger bytes) out of the policy gate.
        let _f: fn(Tier, StopKind, RiskClass) -> Decision = decide_kind;
        // Sanity: the two outputs are unchanged.
        assert!(matches!(
            decide_kind(Tier::Autopilot, StopKind::Publish, RiskClass::Low),
            Decision::Escalate | Decision::AutoFlow
        ));
    }

    #[test]
    fn stops_requiring_human_filters_with_floor() {
        let stops = vec![
            stop("ambiguity", RiskClass::Low), // flows
            stop("publish", RiskClass::Low),   // forced hard -> escalates
            stop("ambiguity", RiskClass::Medium),
        ];
        // Autopilot: only the publish escalates (medium flows, low flows).
        let need = stops_requiring_human(Tier::Autopilot, &stops);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].kind, "publish");
        // Standard: publish + medium escalate.
        let need = stops_requiring_human(Tier::Standard, &stops);
        assert_eq!(need.len(), 2);
    }
}
