//! How an escalation leaves the process: the `Notifier` seam, the always-on stderr
//! line, the best-effort `notify-send` toast with the health bookkeeping that latches
//! a broken transport off, and the fan-out that drives several at once. One unit
//! because the desktop transport's give-up rule only reads as safe next to the log
//! line that keeps recording what it stopped delivering.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::message::{Escalation, Severity};

/// A destination an escalation can be delivered to.
pub trait Notifier: Send + Sync {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()>;
}

/// Always-on transport: one line to stderr (captured to the daemon log).
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        eprintln!(
            "[escalation:{:?}] {}: {} — {}",
            e.severity,
            e.project,
            e.title,
            e.body.replace('\n', " | ")
        );
        Ok(())
    }
}

/// Consecutive non-zero exits after which the desktop transport is declared
/// unavailable. One failure could be a restarting notification server; three in
/// a row (with no success in between) is a broken transport, not a blip.
pub(super) const DESKTOP_DEAD_AFTER: usize = 3;

/// Delivery health of one `DesktopNotifier`, shared with its detached delivery
/// threads. Lives as long as the notifier — `pmd` builds exactly one for the
/// whole process, so "once per notifier" is "once per process" in practice,
/// while staying per-instance keeps tests independent of each other.
#[derive(Debug, Default)]
pub(super) struct DesktopHealth {
    /// Delivery failures observed (spawn error or non-zero exit).
    failures: AtomicUsize,
    /// Warning lines actually printed; the once-guard keeps this at 0 or 1.
    warnings: AtomicUsize,
    /// Consecutive non-zero exits; any success resets it to 0.
    consecutive: AtomicUsize,
    /// Latched: transport declared unavailable, stop spawning the subprocess.
    dead: AtomicBool,
}

impl DesktopHealth {
    /// Print the "desktop notifications are broken" warning at most once. A
    /// daemon sweeping every 500ms would otherwise repeat it forever, so the
    /// single line has to carry everything a reader needs — including the fact
    /// that the escalation itself was *not* lost.
    fn warn_once(&self, bin: &str, detail: &str) {
        if self
            .warnings
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            eprintln!(
                "pmd: desktop notification via `{bin}` failed: {detail} — escalations are \
                 still recorded in the session ledger and shown in pmtui; suppressing \
                 further desktop-notification warnings for this process"
            );
        }
    }

    /// Consume one delivery attempt's outcome. Called on the delivery thread.
    pub(super) fn record(&self, bin: &str, out: std::io::Result<std::process::Output>) {
        match out {
            // Delivered. Clear the streak so a past blip can't accumulate into
            // a latch-off of a transport that demonstrably works.
            Ok(o) if o.status.success() => {
                self.consecutive.store(0, Ordering::SeqCst);
            }
            // Ran but refused (e.g. `notify-send` returning the D-Bus
            // `ServiceUnknown` error when no notification daemon is running).
            Ok(o) => {
                self.failures.fetch_add(1, Ordering::SeqCst);
                let n = self.consecutive.fetch_add(1, Ordering::SeqCst) + 1;
                self.warn_once(bin, &format!("{} ({})", o.status, first_line(&o.stderr)));
                if n >= DESKTOP_DEAD_AFTER {
                    self.dead.store(true, Ordering::SeqCst);
                }
            }
            // Could not even start it (missing binary, not executable): nothing
            // transient about that, so latch off immediately.
            Err(e) => {
                self.failures.fetch_add(1, Ordering::SeqCst);
                self.consecutive.fetch_add(1, Ordering::SeqCst);
                self.warn_once(bin, &format!("cannot run it ({e})"));
                self.dead.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// First non-empty line of a child's stderr, trimmed and length-capped, so the
/// real cause lands in the warning instead of being guessed at.
pub(super) fn first_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no stderr");
    match line.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// `notify-send` urgency corresponding to the escalation's severity.
pub(super) fn urgency_for(severity: Severity) -> &'static str {
    match severity {
        Severity::Urgent => "critical",
        Severity::Warn => "normal",
        Severity::Info => "low",
    }
}

/// Desktop notification via `notify-send`; best-effort (skipped when the
/// severity is below `min_severity`, and — after a hard failure — when the
/// transport has been latched off). Failures are non-fatal but never silent:
/// the first one is reported on stderr like the rest of the daemon's logging.
pub struct DesktopNotifier {
    pub bin: String,
    pub min_severity: Severity,
    pub(super) health: Arc<DesktopHealth>,
}

impl Default for DesktopNotifier {
    fn default() -> Self {
        Self::new("notify-send", Severity::Warn)
    }
}

impl DesktopNotifier {
    pub fn new(bin: impl Into<String>, min_severity: Severity) -> Self {
        Self {
            bin: bin.into(),
            min_severity,
            health: Arc::new(DesktopHealth::default()),
        }
    }

    /// Whether this escalation clears the min-severity gate (pure, testable).
    pub(super) fn will_send(&self, e: &Escalation) -> bool {
        e.severity >= self.min_severity
    }

    /// True once the transport has been declared unavailable: a spawn error, or
    /// `DESKTOP_DEAD_AFTER` consecutive non-zero exits with no success between.
    /// Further escalations then skip the subprocess entirely (they are still
    /// logged by `LogNotifier` and recorded in the ledger).
    pub fn is_unavailable(&self) -> bool {
        self.health.dead.load(Ordering::SeqCst)
    }

    /// Delivery failures observed so far (test/diagnostic observability).
    pub fn failure_count(&self) -> usize {
        self.health.failures.load(Ordering::SeqCst)
    }

    /// Warning lines printed so far; the once-guard caps this at 1.
    pub fn warning_count(&self) -> usize {
        self.health.warnings.load(Ordering::SeqCst)
    }

    /// Spawn the delivery thread, or `None` when nothing should be sent
    /// (severity-gated, or the transport is latched off). `notify` drops the
    /// handle — fire-and-forget; tests join it to observe the outcome
    /// deterministically instead of racing a detached thread.
    pub(super) fn spawn_delivery(&self, e: &Escalation) -> Option<std::thread::JoinHandle<()>> {
        if !self.will_send(e) || self.is_unavailable() {
            return None;
        }
        let mut cmd = Command::new(&self.bin);
        cmd.arg("-u")
            .arg(urgency_for(e.severity))
            .arg(format!("pmd · {}", e.project))
            .arg(format!("{}\n{}", e.title, e.body));
        // Fire-and-forget on a detached thread: `notify-send` makes a synchronous
        // D-Bus call that can block for ~25s (or hang) if the notification server
        // is unresponsive, and the daemon loop is single-threaded — a blocking
        // wait here would freeze scheduling for every project. The thread waits
        // on (and thus reaps) the child so it never becomes a zombie, and — since
        // a dropped result is how this transport used to fail silently — hands
        // the outcome to `DesktopHealth` instead of discarding it. `output()`
        // captures stderr so the real cause can be named, not guessed.
        let health = Arc::clone(&self.health);
        let bin = self.bin.clone();
        Some(std::thread::spawn(move || {
            health.record(&bin, cmd.output());
        }))
    }
}

impl Notifier for DesktopNotifier {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        // Detach: a failed toast must never fail the caller's sweep.
        let _detached = self.spawn_delivery(e);
        Ok(())
    }
}

/// Fan an escalation out to several transports (a failing one never blocks the rest).
pub struct Composite {
    pub notifiers: Vec<Box<dyn Notifier>>,
}

impl Notifier for Composite {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        for n in &self.notifiers {
            let _ = n.notify(e);
        }
        Ok(())
    }
}
