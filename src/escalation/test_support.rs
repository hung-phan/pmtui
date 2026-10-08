//! A `Notifier` that does nothing but remember what it was handed. It lives outside
//! `tests` because `daemon`'s tests need it too, and a capture transport is the only
//! way either of them can assert on an escalation without a notification server.

use super::*;
use std::sync::Mutex;

/// Records every escalation it receives, for assertions.
#[derive(Default)]
pub struct CaptureNotifier {
    pub seen: Mutex<Vec<Escalation>>,
}
impl CaptureNotifier {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}
impl Notifier for CaptureNotifier {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        self.seen.lock().unwrap().push(e.clone());
        Ok(())
    }
}
