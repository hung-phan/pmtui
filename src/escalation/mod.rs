//! Routing escalations to transports (design §9). The scheduler decides *when*
//! to escalate — exactly once per stop, via its `RunState` — so this module has
//! no dedup logic; it decides *how loud* (severity from the loudest stop) and
//! delivers: an always-on log line plus an optional desktop notification.
//!
//! Split along that "how loud" / "how delivered" seam: `message` is what the human is
//! told (the severity ladder, the severity a report of stops adds up to, and the words
//! one [`Escalation`] carries), `transport` is how it leaves the process (the
//! [`Notifier`] seam, the always-on log line, the best-effort `notify-send` toast with
//! the health bookkeeping that latches a broken transport off, and the fan-out), and
//! `test_support` is the capture notifier `daemon`'s tests share with ours. Each item is
//! re-exported by NAME below, so nothing joins this crate's public API without an edit
//! visible right here.

mod message;
mod transport;

#[cfg(test)]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use message::{Escalation, Severity, severity_for_stops};
pub use transport::{Composite, DesktopNotifier, LogNotifier, Notifier};
