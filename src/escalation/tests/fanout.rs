use std::sync::Arc;

use super::*;
use test_support::CaptureNotifier;

struct Shared(Arc<CaptureNotifier>);

impl Notifier for Shared {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        self.0.notify(e)
    }
}

#[test]
fn composite_still_delivers_when_the_desktop_transport_is_broken() {
    // A latched-off desktop transport: no fork, and Ok() to its caller.
    let dead = desktop("pmd-no-such-notifier-binary");
    record(&dead, unspawnable());
    let cap = Arc::new(CaptureNotifier::new());
    let composite = Composite {
        notifiers: vec![
            Box::new(dead) as Box<dyn Notifier>,
            Box::new(LogNotifier),
            Box::new(Shared(Arc::clone(&cap))),
        ],
    };
    composite.notify(&Escalation::stuck("p", "boom")).unwrap();
    assert_eq!(cap.count(), 1, "later transports still get the escalation");
}

#[test]
fn composite_tolerates_a_failing_notifier() {
    struct Failing;

    impl Notifier for Failing {
        fn notify(&self, _: &Escalation) -> anyhow::Result<()> {
            anyhow::bail!("transport down")
        }
    }

    let cap = Arc::new(CaptureNotifier::new());
    let composite = Composite {
        notifiers: vec![Box::new(Failing), Box::new(Shared(Arc::clone(&cap)))],
    };
    // The failing notifier must not block the rest, and notify returns Ok.
    composite.notify(&Escalation::stuck("p", "x")).unwrap();
    assert_eq!(cap.count(), 1, "capture still received despite the failure");
}

#[test]
fn capture_notifier_records() {
    let n = CaptureNotifier::new();
    n.notify(&Escalation::stuck("p", "timed out")).unwrap();
    assert_eq!(n.count(), 1);
    assert_eq!(n.seen.lock().unwrap()[0].severity, Severity::Urgent);
}
