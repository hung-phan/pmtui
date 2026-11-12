//! How an escalation leaves the process: the `Notifier` seam, the always-on stderr
//! line, the best-effort notifications with the health bookkeeping that latches a
//! broken transport off, and the fan-out that drives several at once. One unit
//! because a transport's give-up rule only reads as safe next to the log line that
//! keeps recording what it stopped delivering.
//!
//! A NOTIFICATION HAS TWO FAMILIES, and which one a new transport joins is decided
//! by its mechanism rather than by its vendor:
//!
//! - It RUNS A PROGRAM: [`ProgramNotifier`], where the transport is one [`Wire`]
//!   arm naming an argv. Everything hard about it is shared, because it is the same
//!   problem every time — any notifier program can block or hang, so each delivery
//!   is a detached thread whose outcome latches the transport off instead of being
//!   dropped. Adding one is an enum arm and a match arm.
//! - It WRITES BYTES TO A TERMINAL: [`OscNotifier`]. No subprocess, so none of that
//!   bookkeeping applies — a write either lands or it does not.
//!
//! [`Composite`] is the seam above both, so a future family that is neither (an
//! HTTP webhook, say) is a third `Notifier` impl and changes nothing here.

use std::io::{IsTerminal, Write};
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

/// Consecutive non-zero exits after which a program transport is declared
/// unavailable. One failure could be a restarting notification server; three in
/// a row (with no success in between) is a broken transport, not a blip.
pub(super) const DEAD_AFTER: usize = 3;

/// Delivery health of one `ProgramNotifier`, shared with its detached delivery
/// threads. Lives as long as the notifier — `pmd` builds exactly one for the
/// whole process, so "once per notifier" is "once per process" in practice,
/// while staying per-instance keeps tests independent of each other.
#[derive(Debug, Default)]
pub(super) struct DeliveryHealth {
    /// Delivery failures observed (spawn error or non-zero exit).
    failures: AtomicUsize,
    /// Warning lines actually printed; the once-guard keeps this at 0 or 1.
    warnings: AtomicUsize,
    /// Consecutive non-zero exits; any success resets it to 0.
    consecutive: AtomicUsize,
    /// Latched: transport declared unavailable, stop spawning the subprocess.
    dead: AtomicBool,
}

/// The one line a reader gets when a transport cannot deliver. Pure, because it is
/// USER-VISIBLE TEXT and `eprintln!` cannot be asserted — this is the project's own
/// lesson from an installer whose reworded error told every machine its network was
/// down.
///
/// It names the PROGRAM rather than a family: the same bookkeeping now serves several
/// transports, and "desktop notification via `cmux`" sent a reader looking for a
/// D-Bus problem that was never there. It also has to say that the escalation itself
/// survived, since a daemon sweeping every 500ms prints this once and then goes quiet.
pub(super) fn broken_transport_warning(bin: &str, detail: &str) -> String {
    format!(
        "pmd: notification via `{bin}` failed: {detail} — escalations are still \
         recorded in the session ledger and shown in pmtui; suppressing further \
         `{bin}` notification warnings for this process"
    )
}

impl DeliveryHealth {
    /// Print [`broken_transport_warning`] at most once. A daemon sweeping every
    /// 500ms would otherwise repeat it forever.
    fn warn_once(&self, bin: &str, detail: &str) {
        if self
            .warnings
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            eprintln!("{}", broken_transport_warning(bin, detail));
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
                if n >= DEAD_AFTER {
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

/// Which notifier program a [`ProgramNotifier`] speaks to. THE ONLY THING A NEW
/// PROGRAM-BASED TRANSPORT ADDS: one arm here, one arm in [`argv`], one constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wire {
    /// `notify-send -u <urgency> <summary> <body>` — freedesktop, over D-Bus.
    #[default]
    NotifySend,
    /// `cmux notify --title T --subtitle S --body B`.
    Cmux,
}

impl Wire {
    /// The program this wire speaks to, which is also the default `bin`. Separate
    /// from `bin` so a caller can still point a wire at a wrapper or an absolute
    /// path without the wire having to know about it.
    pub fn program(self) -> &'static str {
        match self {
            Wire::NotifySend => "notify-send",
            Wire::Cmux => "cmux",
        }
    }
}

/// One transport a caller can ask for by name, which is how `pmd` learns its set.
/// Exhaustive over both families, so adding a transport adds exactly one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// [`LogNotifier`] — always available, and the reason a failed toast is never
    /// a lost escalation.
    Log,
    /// A [`ProgramNotifier`] on this wire.
    Program(Wire),
    /// [`OscNotifier`].
    Osc,
}

/// The set `pmd` runs with when nothing is asked for.
///
/// IT IS DELIBERATELY NOT EVERY TRANSPORT. The two that reach a specific product —
/// `cmux` and the terminal sequence — have EFFECTS OUTSIDE THIS PROCESS on whatever
/// machine the daemon happens to run on, and the integration suite launches the real
/// `pmd` binary. On by default, every test that drove a session to an escalation
/// fired a real notification into the developer's own terminal and spawned a `cmux`
/// that could hang for 15s on its relay — the same shape as the fixture that once
/// spawned real sessions in a live dashboard. Opt-in costs one flag; the alternative
/// costs a flag on every pmd launch site in the suite, and missing one is silent.
pub const DEFAULT_TRANSPORTS: &[Transport] =
    &[Transport::Log, Transport::Program(Wire::NotifySend)];

impl Transport {
    /// The name this transport is asked for by. Round-trips with [`Transport::parse`].
    pub fn name(self) -> &'static str {
        match self {
            Transport::Log => "log",
            Transport::Program(Wire::NotifySend) => "notify-send",
            Transport::Program(Wire::Cmux) => "cmux",
            Transport::Osc => "osc",
        }
    }

    /// Every transport that can be named, for help text and for the tests that hold
    /// the two in step.
    pub const ALL: &'static [Transport] = &[
        Transport::Log,
        Transport::Program(Wire::NotifySend),
        Transport::Program(Wire::Cmux),
        Transport::Osc,
    ];

    /// One name to a transport. An UNKNOWN NAME IS AN ERROR rather than a skip: a
    /// caller who asked for a transport and silently did not get it would read every
    /// missing notification as the transport failing.
    pub fn parse(name: &str) -> Option<Transport> {
        Self::ALL.iter().copied().find(|t| t.name() == name)
    }

    /// A comma-separated list to a set, in the order given. Pure, and the one place
    /// that turns a string into transports — `pmd` only builds the boxes.
    pub fn parse_list(list: &str) -> Result<Vec<Transport>, String> {
        list.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Transport::parse(s).ok_or_else(|| s.to_string()))
            .collect()
    }

    /// The notifier this transport names.
    pub fn notifier(self, min_severity: Severity) -> Box<dyn Notifier> {
        match self {
            Transport::Log => Box::new(LogNotifier),
            Transport::Program(wire) => Box::new(ProgramNotifier::wired(wire, min_severity)),
            Transport::Osc => Box::new(OscNotifier::new(min_severity)),
        }
    }
}

/// The argv for one escalation, less the program itself. Pure, so every wire's
/// mapping is checked without a notification server, a relay, or a terminal.
///
/// [`Escalation`] has three pieces of text and the wires disagree about how many
/// slots they have, which is the whole reason this is a function: `notify-send`
/// takes a summary and a body, so title and body fold together; cmux takes three,
/// so they stay apart.
pub(super) fn argv(wire: Wire, e: &Escalation) -> Vec<String> {
    match wire {
        Wire::NotifySend => vec![
            "-u".into(),
            urgency_for(e.severity).into(),
            format!("pmd · {}", e.project),
            format!("{}\n{}", e.title, e.body),
        ],
        Wire::Cmux => vec![
            "notify".into(),
            "--title".into(),
            format!("pmtui · {}", e.project),
            "--subtitle".into(),
            e.title.clone(),
            "--body".into(),
            e.body.clone(),
        ],
    }
}

/// A notification delivered by RUNNING A PROGRAM, named by its [`Wire`]; best-effort
/// (skipped when the severity is below `min_severity`, and — after a hard failure —
/// when the transport has been latched off). Failures are non-fatal but never silent:
/// the first one is reported on stderr like the rest of the daemon's logging.
pub struct ProgramNotifier {
    pub bin: String,
    pub min_severity: Severity,
    pub wire: Wire,
    pub(super) health: Arc<DeliveryHealth>,
}

impl Default for ProgramNotifier {
    fn default() -> Self {
        Self::wired(Wire::NotifySend, Severity::Warn)
    }
}

impl ProgramNotifier {
    /// The ordinary constructor: a wire, delivered by the program that wire names.
    ///
    /// Registered unconditionally by callers, because a missing program is a spawn
    /// error — not transient, so it latches the transport off after one attempt and
    /// names itself once on stderr. A PATH probe would only duplicate a decision
    /// the health bookkeeping already makes correctly.
    pub fn wired(wire: Wire, min_severity: Severity) -> Self {
        Self::with_bin(wire.program(), min_severity, wire)
    }

    /// A wire pointed at a DIFFERENT program than the one it names — a wrapper, an
    /// absolute path, or a fake in a test. The wire still decides the argv, which is
    /// why there is no constructor taking a binary without one: handing `cmux` to the
    /// `notify-send` argv would speak the wrong protocol to a real program, silently.
    pub fn with_bin(bin: impl Into<String>, min_severity: Severity, wire: Wire) -> Self {
        Self {
            bin: bin.into(),
            min_severity,
            wire,
            health: Arc::new(DeliveryHealth::default()),
        }
    }

    /// Whether this escalation clears the min-severity gate (pure, testable).
    pub(super) fn will_send(&self, e: &Escalation) -> bool {
        e.severity >= self.min_severity
    }

    /// True once the transport has been declared unavailable: a spawn error, or
    /// `DEAD_AFTER` consecutive non-zero exits with no success between.
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
        cmd.args(argv(self.wire, e));
        // Fire-and-forget on a detached thread, which is why every program-based wire
        // shares this code: each one can hang, for its own reason. `notify-send` makes
        // a synchronous D-Bus call that can block for ~25s if the notification server
        // is unresponsive; `cmux` talks to a per-attach TCP relay that keeps accepting
        // connections but stops answering its auth challenge while the attach is down,
        // measured timing out for 15s at a stretch. The daemon loop is single-threaded,
        // so a blocking wait here would freeze scheduling for every project. The thread waits
        // on (and thus reaps) the child so it never becomes a zombie, and — since
        // a dropped result is how this transport used to fail silently — hands
        // the outcome to `DeliveryHealth` instead of discarding it. `output()`
        // captures stderr so the real cause can be named, not guessed.
        let health = Arc::clone(&self.health);
        let bin = self.bin.clone();
        Some(std::thread::spawn(move || {
            health.record(&bin, cmd.output());
        }))
    }
}

impl Notifier for ProgramNotifier {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        // Detach: a failed toast must never fail the caller's sweep.
        let _detached = self.spawn_delivery(e);
        Ok(())
    }
}

/// One OSC 777 field, made safe to put inside the sequence.
///
/// A control byte would END the sequence early — `0x07` is its own terminator and
/// `0x1b` starts another escape — and a `;` would shift every later field, so a
/// title carrying one would push the body out of its slot. Both matter because this
/// text is partly AGENT-AUTHORED: an escalation body is built from stop questions
/// the worker wrote.
///
/// Deliberately NOT `advise::sanitize_control_bytes`, whose doc restricts it to
/// harness-quoted worker text for a different reason. Two small rules that happen to
/// strip overlapping bytes, each stated where it is enforced, beats one shared
/// function whose contract has to cover both.
fn osc_field(s: &str) -> String {
    let swapped: String = s
        .chars()
        .map(|c| {
            let u = c as u32;
            if u < 0x20 || u == 0x7f || (0x80..=0x9f).contains(&u) {
                ' '
            } else if c == ';' {
                ','
            } else {
                c
            }
        })
        .collect();
    swapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The OSC 777 byte sequence for one escalation. Pure, so the escaping is checked
/// without a terminal.
///
/// Two text slots, so project and title fold together the way they do for
/// `notify-send`. OSC 777 carries no urgency field, so severity survives only as
/// the transport's own min-severity gate.
///
/// TERMINATED BY ST (`ESC \`), not BEL. Both end an OSC and most terminals take either,
/// but the only sequence MEASURED to raise a notification in the surface this exists for
/// was terminated by ST, typed by hand into a cmux tab. BEL has no such reading behind it.
pub(super) fn osc777(e: &Escalation) -> String {
    format!(
        "\x1b]777;notify;{};{}\x1b\\",
        osc_field(&format!("pmd · {} — {}", e.project, e.title)),
        osc_field(&e.body),
    )
}

/// A notification delivered by WRITING BYTES TO A TERMINAL, as OSC 777.
///
/// The vendor-neutral transport: it names no product and needs no program on PATH,
/// so any terminal that understands the sequence shows the notification and any
/// terminal that does not discards it. That is also why it carries none of
/// [`ProgramNotifier`]'s health bookkeeping — there is no subprocess to hang and no
/// exit code to interpret, so a write either lands or returns an error at once.
///
/// It writes to STDERR, the same place [`LogNotifier`] writes, and only when that is
/// a terminal. A redirected stderr is a log file, and OSC bytes in a log file are
/// noise no one will ever see as a notification.
pub struct OscNotifier {
    pub min_severity: Severity,
}

impl Default for OscNotifier {
    fn default() -> Self {
        Self {
            min_severity: Severity::Warn,
        }
    }
}

impl OscNotifier {
    pub fn new(min_severity: Severity) -> Self {
        Self { min_severity }
    }

    /// Whether this escalation clears the min-severity gate (pure, testable).
    pub(super) fn will_send(&self, e: &Escalation) -> bool {
        e.severity >= self.min_severity
    }
}

impl Notifier for OscNotifier {
    fn notify(&self, e: &Escalation) -> anyhow::Result<()> {
        if !self.will_send(e) {
            return Ok(());
        }
        let mut err = std::io::stderr();
        if !err.is_terminal() {
            return Ok(());
        }
        // Swallowed like the other transports: a notification that did not land must
        // never fail the caller's sweep, and `LogNotifier` recorded it regardless.
        let _ = err.write_all(osc777(e).as_bytes());
        let _ = err.flush();
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
