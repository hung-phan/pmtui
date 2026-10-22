//! The agent-to-agent message protocol: the bound, the envelope and the hop arithmetic.
//!
//! PURE, and in the library rather than beside the command, for one reason: the skill text that
//! DOCUMENTS the limit is embedded here too ([`crate::skills::MESSAGE_SKILL_MD`]), so a test can
//! hold the prose and the enforced number against each other. A documented bound that drifts from
//! the enforced one is worse than no documentation.
//!
//! `pmtui message` in the dashboard binary owns everything with an effect: the registry, the
//! pane, the lock and the files.

use serde::{Deserialize, Serialize};

use crate::clock::Epoch;

/// Sends to ONE peer allowed inside [`HOP_WINDOW_S`] before the sender is refused.
///
/// Two agents being polite to each other is an unattended token fire: nobody is watching, both
/// are driven on a cadence, and each reply is a turn that costs money. The bound is deliberately
/// on the SENDER and enforced by this command rather than carried in the envelope, because a hop
/// count an agent has to echo back is reset by an agent that simply omits the flag — which is no
/// bound at all.
pub const HOP_MAX: u32 = 6;
/// Quiet time after which a peer's hop count starts over.
pub const HOP_WINDOW_S: i64 = 600;

/// The envelope the recipient sees.
///
/// A message from another agent arrives as an ordinary user turn, which the recipient cannot
/// otherwise tell from its own human's. The frame is what makes it distinguishable, and it names
/// the command that answers it so a reply needs no documentation. It is advisory and not a
/// security control — the recipient can still be talked into things — but "labelled" beats
/// "indistinguishable".
pub fn envelope(from: &str, hop: u32, text: &str) -> String {
    format!(
        "[message from session {from} · hop {hop}/{HOP_MAX} · \
         reply with: pmtui message --to {from} --message \"...\"]\n\n{text}"
    )
}

/// The hop ledger for one peer: how many sends inside the current window, and when it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hops {
    pub count: u32,
    pub opened_at: Epoch,
}

/// The hop this send would be, given the ledger — `None` once the limit is reached.
///
/// PURE, so every arm is testable without a clock or a file.
pub fn next_hop(previous: Option<Hops>, now: Epoch) -> Option<Hops> {
    match previous {
        // A quiet window has closed: this is a new conversation, not hop 7 of the old one.
        Some(h) if now.saturating_sub(h.opened_at) >= HOP_WINDOW_S => Some(Hops {
            count: 1,
            opened_at: now,
        }),
        Some(h) if h.count >= HOP_MAX => None,
        Some(h) => Some(Hops {
            count: h.count + 1,
            opened_at: h.opened_at,
        }),
        None => Some(Hops {
            count: 1,
            opened_at: now,
        }),
    }
}

#[cfg(test)]
#[path = "message/tests.rs"]
mod tests;
