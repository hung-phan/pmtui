use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// A clock the test controls; cloneable so the scheduler and the test share
/// one underlying value.
#[derive(Clone, Default)]
pub struct FakeClock(Arc<AtomicI64>);

impl FakeClock {
    pub fn new(start: Epoch) -> Self {
        FakeClock(Arc::new(AtomicI64::new(start)))
    }
    pub fn set(&self, t: Epoch) {
        self.0.store(t, Ordering::SeqCst);
    }
    pub fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Epoch {
        self.0.load(Ordering::SeqCst)
    }
}
