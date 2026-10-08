//! What the human is told and how loud: the severity ladder, the severity a whole
//! report of stops adds up to, and the title and body one escalation carries. One
//! unit because the loudness and the words are read off the same stops, and nothing
//! here knows or cares which transport will carry them.

use crate::policy;
use crate::state::{RiskClass, Stop};

/// Loudness of an escalation. Ordered `Info < Warn < Urgent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Urgent,
}

/// Severity from the loudest stop's *effective* risk (kind-based hard floor
/// applied), so a mislabelled publish still reads as urgent.
pub fn severity_for_stops(stops: &[Stop]) -> Severity {
    let mut sev = Severity::Info;
    for s in stops {
        let r = match policy::effective_risk(s) {
            RiskClass::Hard => Severity::Urgent,
            RiskClass::Medium => Severity::Warn,
            RiskClass::Low => Severity::Info,
        };
        if r > sev {
            sev = r;
        }
    }
    sev
}

/// A human-facing escalation message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    pub project: String,
    pub title: String,
    pub body: String,
    pub severity: Severity,
}

impl Escalation {
    /// Build from the stops that require the human.
    pub fn for_stops(project: &str, stops: &[Stop]) -> Escalation {
        let severity = severity_for_stops(stops);
        let title = if stops.len() == 1 {
            "1 decision needs you".to_string()
        } else {
            format!("{} decisions need you", stops.len())
        };
        let body = stops
            .iter()
            .map(|s| {
                // ONE mapping shared with pmtui's Stops block and answer overlay (see
                // `state::stop_product_text`). This used to fall back to the bare `&s.kind`,
                // so a stop the HARNESS synthesized — which by definition carries no draft
                // text — notified the human with the body `[stop-…] confirm_done` and
                // nothing else. This is the message that leaves the machine, so it is the
                // worst of the three surfaces to say nothing on.
                let product = crate::state::stop_product_text(s);
                // A kind the harness never synthesizes still has no product string (a
                // text-less `publish` means the AGENT dropped its question, and no fixed
                // sentence could say WHAT would be published) — keep the old bare-kind body
                // for those rather than shipping an empty one.
                let q = if product.is_empty() {
                    s.kind.as_str()
                } else {
                    product
                };
                let line = format!("[{}] {}", s.id, q);
                // The choices the agent offered, numbered onto ONE continuation line.
                // This is the message a human actually receives, so an answerable
                // question ships with its answers; no options ⇒ byte-identical body.
                if s.options.is_empty() {
                    line
                } else {
                    let opts = s
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{}) {}", i + 1, o.trim()))
                        .collect::<Vec<_>>()
                        .join("  ");
                    format!("{line}\n    {opts}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        Escalation {
            project: project.to_string(),
            title,
            body,
            severity,
        }
    }

    /// A stuck project (timeout / repeated failure) is always urgent.
    pub fn stuck(project: &str, reason: &str) -> Escalation {
        Escalation {
            project: project.to_string(),
            title: "project stuck".to_string(),
            body: reason.to_string(),
            severity: Severity::Urgent,
        }
    }
}
