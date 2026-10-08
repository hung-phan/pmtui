//! A minimal clock abstraction so the scheduler is deterministically testable.
//! Real code uses [`SystemClock`]; tests use `test_support::FakeClock`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock time in whole seconds since the Unix epoch.
pub type Epoch = i64;

/// Source of "now".
pub trait Clock: Send + Sync {
    fn now(&self) -> Epoch;
}

/// Real system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Epoch {
        // A clock before 1970 is nonsensical here; clamp to 0 rather than panic.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as Epoch)
            .unwrap_or(0)
    }
}

#[cfg(test)]
pub mod test_support {
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
}
