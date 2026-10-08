//! The daemon's per-sweep orchestration, extracted from the `pmd` binary so it
//! is unit-testable over fake `Driver`/`Clock`/`Notifier` (the binary is then
//! just arg-parsing + the sleep loop).
//!
//! Each `sweep` reloads nothing itself — the caller passes a freshly-loaded
//! [`Registry`] — and then, for the live project set:
//!   - **prunes** runners whose id left the registry (no unbounded growth);
//!   - **rebuilds** a runner whose `root` changed, or that was just re-enabled
//!     (so a paused project re-activates without a daemon restart);
//!   - **drives** each enabled, non-poisoned project one tick and routes its
//!     escalations — **unless the row's autonomy tier says not to**
//!     ([`pmd_drives_row`]: anything but autopilot ON means pmd does not touch
//!     that session at all);
//!   - **pauses** a project that errors on `POISON_THRESHOLD` consecutive ticks
//!     (corrupt state), surfacing it once instead of a tight retry loop (§11).
//!
//! The sweep itself stays HERE, because [`Daemon`] is the only thing that holds
//! state across sweeps (the runner table, each runner's lease, the notify-once
//! dedup); the two files beside it are what a sweep asks about ONE row. `row` reads
//! that row — its paths, its autonomy tier — and answers the drive-or-skip question
//! ([`pmd_drives_row`], re-exported here because pmtui and the job engine read the
//! same predicate as `daemon::pmd_drives_row`). `stops` turns a parked ledger's
//! `open_stops` into the `Stop`s an escalation carries.

mod reap;
mod row;
mod stops;

use std::collections::HashMap;
use std::path::PathBuf;

use crate::clock::{Clock, Epoch};
use crate::escalation::{Escalation, Notifier};
use crate::job_engine::{JobScheduler, JobTick};
use crate::lease::{self, ProjectLease};
use crate::registry::{Engine, ProjectEntry, Registry};
use crate::state::Tier;
use crate::tmux::Driver;

pub use reap::{reap_owned_sessions, sweep_orphan_loops};
pub use row::pmd_drives_row;
use row::{entry_paths, entry_tier};
use stops::{job_open_stop_ids, job_stops};

/// Consecutive tick errors after which a project is paused ("poisoned") and
/// surfaced, rather than retried in a tight loop (design §11).
const POISON_THRESHOLD: u32 = 3;

/// Which driver a runner holds. Every project is a `Mode::AgentLoop` session driven
/// by [`JobScheduler`] (the per-session heartbeat loop).
enum Driven {
    Job(JobScheduler),
}

/// One managed project's live driver plus the bookkeeping the sweep needs.
struct Runner {
    driven: Driven,
    /// The registry root this runner was built from (rebuild on change).
    root: PathBuf,
    /// Last-seen enabled flag, to detect the disabled→enabled ("unpause") edge.
    enabled: bool,
    boot_failures: u32,
    poisoned: bool,
    /// Cross-process driver lease; `None` until acquired. Held for as long as this
    /// process drives the project (released when the runner is dropped/rebuilt).
    lease: Option<ProjectLease>,
    /// Whether we've already logged that another process holds the lease.
    lease_warned: bool,
    /// Notify-once dedup, keyed on OpenStop ids. `JobScheduler` re-emits
    /// `Escalated`/`Stuck` on every tick while parked (it has no `Still*` variant),
    /// and a `Stuck` park re-surfaces on the next sweep as `Escalated([stuck_id])` —
    /// so BOTH arms dedup on the same stop ids: a surfaced id is inserted once, and a
    /// later tick that only re-emits known ids stays silent. Any progress outcome
    /// clears the set so a later park re-arms (a genuinely new stop id re-notifies).
    /// In-memory only — across a daemon restart the set starts empty, so one
    /// re-notify per parked project after a restart is accepted for v1.
    notified: std::collections::HashSet<String>,
    /// The last successful Autopilot tick parked on a human decision. Used only to keep pmd
    /// resident through a later unreadable tier; it never grants permission to drive.
    waiting_for_human: bool,
}

impl Runner {
    fn build(
        p: &ProjectEntry,
        supervisor_enabled: Option<bool>,
        pmtui_bin: Option<&std::path::Path>,
    ) -> Runner {
        let mut js = JobScheduler::new(
            p.id.clone(),
            p.root.clone(),
            &p.id,
            p.engine.unwrap_or(Engine::Claude),
            p.worker_model.clone(),
        );
        if let Some(enabled) = supervisor_enabled {
            js.set_supervisor_enabled(enabled);
        }
        js.set_pmtui_bin(pmtui_bin.map(std::path::Path::to_path_buf));
        // Seed the adopt arm with the registry's `conversation_id` (the id the
        // human's first chat created). `reconcile_one` re-sets this each sweep so
        // a seed written AFTER the runner was built still reaches the scheduler.
        js.set_registry_seed(p.conversation_id.as_deref());
        Runner {
            driven: Driven::Job(js),
            root: p.root.clone(),
            enabled: p.enabled,
            boot_failures: 0,
            poisoned: false,
            lease: None,
            lease_warned: false,
            notified: std::collections::HashSet::new(),
            waiting_for_human: false,
        }
    }
}

fn interrupt_runner_advice(
    runner: &mut Runner,
    driver: &dyn Driver,
    now: Epoch,
    reason: &str,
) -> bool {
    let result = match &mut runner.driven {
        Driven::Job(scheduler) => scheduler.interrupt_advice(driver, now, reason),
    };
    if let Err(error) = result {
        eprintln!("pmd: could not interrupt decider: {error:#}");
        return false;
    }
    true
}

/// How long pmd may sit with NOTHING to drive before it exits.
///
/// pmd is spawned on demand (pmtui ensures one when you turn autopilot on and at startup),
/// so keeping it spinning at the tick rate when no enabled row needs it — all paused,
/// removed, Standard, or interactive — is pure waste. It shuts itself down instead, and the
/// next thing that needs driving brings it back.
///
/// A GRACE window, not an immediate exit, and that is the point: toggling autopilot off then
/// on again must keep the SAME daemon (no thrash, no "pmd DOWN" flicker, no lost in-memory
/// runner state) — a property `autopilot_off_stops_the_nudges_without_killing_the_session`
/// pins. Five minutes is far longer than any such toggle and short enough to reclaim a truly
/// idle daemon. An Autopilot row parked on a long cadence is NOT idle — it still needs pmd to
/// fire the next nudge — so this only ever triggers when nothing is drivable at all.
pub const IDLE_EXIT_S: i64 = 300;

/// What a sweep concluded, for the caller's loop.
pub struct SweepReport {
    /// Every enabled project has reached `done` (the caller may exit).
    pub all_enabled_done: bool,
    /// Nothing has needed pmd for at least [`IDLE_EXIT_S`] — the caller should exit and let
    /// the next autopilot flip respawn it.
    pub idle_expired: bool,
}

/// Holds the persistent per-project run state across sweeps.
#[derive(Default)]
pub struct Daemon {
    runners: HashMap<String, Runner>,
    /// Last successfully read autonomy tier for each enabled row at its current root.
    ///
    /// This is a residency hint only. A failed current read is still passed to the drive gate as
    /// `None`, so cached Autopilot can keep pmd available without granting permission to type.
    last_known_tiers: HashMap<String, Tier>,
    /// Duplicate registry ids already warned about (warn once, not every sweep).
    warned_dupes: std::collections::HashSet<String>,
    /// Ids already warned about for an UNREADABLE autonomy tier (warn once, not every sweep
    /// — this one would otherwise fire twice a second).
    ///
    /// The price of making [`pmd_drives_row`] opt-in: a row whose config cannot be read is
    /// now skipped rather than driven, and a skipped row is silent. Silence is how a
    /// half-created or corrupted session becomes invisible, so it gets named once.
    warned_no_tier: std::collections::HashSet<String>,
    /// When pmd first had NOTHING to drive, or `None` while something needs it. Drives the
    /// idle self-exit (see [`IDLE_EXIT_S`]); set on the transition into idle and left to
    /// accumulate, cleared the moment a drivable row reappears.
    idle_since: Option<Epoch>,
    /// Test/one-shot override for detached decider consults. `None` keeps the
    /// environment-controlled default; `Some(false)` guarantees one sweep never
    /// leaves asynchronous work behind.
    supervisor_enabled: Option<bool>,
    /// The `pmtui` every terminal this daemon launches names as `PMTUI_BIN`, resolved once by
    /// pmd at startup; `None` omits the variable.
    pmtui_bin: Option<PathBuf>,
}

impl Daemon {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_supervisor_enabled(enabled: bool) -> Self {
        Self {
            supervisor_enabled: Some(enabled),
            ..Self::default()
        }
    }

    /// Name `bin` as `PMTUI_BIN` in every terminal this daemon launches (`None` omits it).
    /// pmd passes [`crate::tmux::pmtui_bin_for_current_process`], the `pmtui` beside it.
    pub fn with_pmtui_bin(mut self, bin: Option<PathBuf>) -> Self {
        self.pmtui_bin = bin;
        self
    }

    /// Run one sweep over `reg`.
    pub fn sweep(
        &mut self,
        reg: &Registry,
        driver: &dyn Driver,
        clock: &dyn Clock,
        notifier: &dyn Notifier,
    ) -> SweepReport {
        // Prune runners for ids no longer present at all (frees dead state and
        // lets a reused id start fresh). Dropping a `Runner` does NOT terminate its
        // tmux worker (there is no Drop-abort), so before removing each pruned
        // runner we abort its in-flight worker — otherwise a human who closes an
        // agent-loop session mid-wake (its id leaves the registry) would orphan the
        // `pmj-…` pane, which would keep editing the tree. `abort` is a no-op when
        // nothing is running, so this is safe/universal for every mode: it only
        // terminates the pane and never touches the per-session on-disk state under
        // `sessions/<id>/`. Mirrors the interactive-switch abort below.
        let live: std::collections::HashSet<&str> =
            reg.projects.iter().map(|p| p.id.as_str()).collect();
        let removed: Vec<String> = self
            .runners
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for id in removed {
            // The registry no longer authorizes this id, so its residency hint is invalid even if
            // aborting an in-flight worker fails and the runner must remain for a retry.
            self.last_known_tiers.remove(&id);
            let aborted = self
                .runners
                .get_mut(&id)
                .is_none_or(|r| match &mut r.driven {
                    Driven::Job(s) => s.abort(driver, clock).is_ok(),
                });
            if aborted {
                self.runners.remove(&id);
            }
        }

        // Reconcile each id at most once per sweep. A duplicate id in the registry
        // is a misconfiguration (two entries would fight over one runner); skip the
        // repeat and warn once.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut needs_pmd = false;
        for p in &reg.projects {
            // Dedup so a duplicate id is warned-once and only the first entry governs
            // this id's runner (two entries would otherwise fight over one runner).
            if !seen.insert(p.id.as_str()) {
                if self.warned_dupes.insert(p.id.clone()) {
                    eprintln!(
                        "pmd: duplicate project id {:?} in registry — ignoring the extra entry",
                        p.id
                    );
                }
                continue;
            }
            // A JOB IS NOT pmd'S ROW. Its whole life belongs to the dashboard's spawn broker: one
            // headless run that reports a result and exits, then retirement. pmd has nothing to drive
            // (a job is seeded Standard, so the Autopilot gate already refused it) but it was still
            // building a runner and OBSERVING the pane, which wrote a death ledger — `driver.json`
            // with `exit_reason: failed` — into a directory the broker had just deleted, leaving a
            // retired child's folder behind holding that one file. Skipped HERE, before any per-row
            // work, so pmd neither reads a job's config nor writes anything under it, and a finished
            // one-shot run can never reach the relaunch path.
            if p.is_job() {
                continue;
            }
            let root_changed = self
                .runners
                .get(&p.id)
                .is_some_and(|runner| runner.root != p.root);
            if root_changed || !p.enabled {
                self.last_known_tiers.remove(&p.id);
            }
            let tier = entry_tier(p);
            if p.enabled
                && let Some(observed) = tier
            {
                self.last_known_tiers.insert(p.id.clone(), observed);
            }
            self.reconcile_one(p, tier, driver, clock, notifier);
            let waiting_for_human = self
                .runners
                .get(&p.id)
                .is_some_and(|runner| runner.waiting_for_human);
            needs_pmd |= row::entry_needs_pmd(
                p,
                tier,
                self.last_known_tiers.get(&p.id).copied(),
                waiting_for_human,
            );
        }

        // An agent-loop session never reaches a machine "done" (the human closes it),
        // so pmd never exits on this signal — only on idle self-exit or a stop signal.
        let all_enabled_done = false;

        // DOES ANY ENABLED ROW NEED PMD? The loop above uses one tier read for both this residency
        // decision and reconciliation. A temporary read failure may preserve a previously observed
        // Autopilot responsibility, but `reconcile_one` receives `None` and still fails closed.
        let now = clock.now();
        // Start the idle clock on the transition into idle and let it accumulate; clear it the
        // instant a drivable row reappears (a quick autopilot off→on never accrues the grace).
        self.idle_since = match (needs_pmd, self.idle_since) {
            (true, _) => None,
            (false, Some(t)) => Some(t),
            (false, None) => Some(now),
        };
        let idle_expired = self
            .idle_since
            .is_some_and(|t| now.saturating_sub(t) >= IDLE_EXIT_S);
        SweepReport {
            all_enabled_done,
            idle_expired,
        }
    }

    fn reconcile_one(
        &mut self,
        p: &ProjectEntry,
        tier: Option<Tier>,
        driver: &dyn Driver,
        clock: &dyn Clock,
        notifier: &dyn Notifier,
    ) {
        // Decide whether to (re)build this runner before driving it.
        let existing = self.runners.get(&p.id);
        let was_enabled = existing.map(|r| r.enabled).unwrap_or(false);
        let config_changed = existing.map(|r| r.root != p.root).unwrap_or(false);
        // Re-enabling a paused project rebuilds it (fresh read of disk, clears poison).
        let reenabled = existing.is_some() && !was_enabled && p.enabled;

        if existing.is_none() || config_changed || reenabled {
            if config_changed {
                eprintln!("pmd: {} config changed — rebuilding its driver", p.id);
                if let Some(runner) = self.runners.get_mut(&p.id) {
                    let result = match &mut runner.driven {
                        Driven::Job(scheduler) => scheduler.abort(driver, clock),
                    };
                    if let Err(error) = result {
                        eprintln!("pmd: could not stop old root for {}: {error:#}", p.id);
                        return;
                    }
                }
            }
            self.runners.insert(
                p.id.clone(),
                Runner::build(p, self.supervisor_enabled, self.pmtui_bin.as_deref()),
            );
        }

        let r = self.runners.get_mut(&p.id).expect("just ensured present");
        r.enabled = p.enabled;
        if !p.enabled || r.poisoned {
            if !interrupt_runner_advice(
                r,
                driver,
                clock.now(),
                "the managed row stopped being drivable",
            ) {
                return;
            }
            // RELEASE the per-session `driver.lock` for a row we are no longer driving. pmd
            // acquires that flock while driving and holds it across sweeps; a plain `return`
            // here kept holding it for a PAUSED row until a later sweep rebuilt the runner —
            // so pmtui's Enter-to-resume raced the stale lock and lost ("pmd is already
            // starting this session"), needing a second Enter once the rebuild finally freed
            // it. Dropping the lease now frees the lock on the sweep that observes the pause,
            // so a single resume wins. Safe: an unheld `Option` drops to `None` (no-op); a
            // re-enable to Autopilot re-acquires via the `reenabled` rebuild above.
            r.lease = None;
            return; // disabled or surfaced-and-paused
        }
        // AUTOPILOT OFF MEANS OFF. `m` in pmtui flips the per-session `config.autonomy`,
        // and until now the whole engine read that tier at exactly ONE site
        // (`policy::decide_kind`) — so it only decided whether a stop the agent REPORTED
        // was auto-approved or escalated. The heartbeat itself was tier-blind, which
        // meant `m` could START the driving (`cycle_tier` → `ensure_daemon`) and could
        // NEVER stop it.
        //
        // Gate it HERE, in the reconcile path, rather than at the nudge: skipping the
        // whole `JobScheduler::tick` is what makes "off" mean off for all THREE things
        // that type into a session — the cadence nudge, `ensure_session`'s
        // launch/relaunch, and answer injection — since all three live inside that tick.
        //
        // Deliberately NOT inside `nudge` / at the `idle_observed` call site: an answer a
        // human writes while the pane is not idle is parked in `pending_context`, whose
        // ONLY reader is `nudge`. Answering also flips `run` out of `Blocked`, so
        // `on_blocked` never re-enters — a gate at the nudge would therefore ORPHAN that
        // parked answer forever ("answer accepted", agent never hears it). Skipping the
        // tick instead leaves the ledger byte-identical, so nothing is consumed and
        // nothing is stranded.
        //
        // A live project terminal is NOT killed: this is a plain `return`, and nothing on
        // this path calls `terminate`/`abort`. The human keeps their agent and drives it by
        // hand (Enter attaches it in pmtui) — that IS what "you drive it" means.
        if !pmd_drives_row(p.mode, tier) {
            if !interrupt_runner_advice(r, driver, clock.now(), "the session left Autopilot") {
                return;
            }
            // Same lease release as the disabled arm above: an autopilot→Standard flip (`m`)
            // leaves this row ENABLED but no longer pmd-driven, so drop the `driver.lock` pmd
            // held while driving — otherwise a later Enter-to-drive would race the stale lock.
            r.lease = None;
            // An unreadable tier is a DIFFERENT situation from a deliberate Standard, and
            // only one of the two is the human's to fix. Say so, once.
            // No mode check: `pmd_drives_row` only ever returns false for `Mode::AgentLoop`,
            // so reaching here already proves the mode, and `Mode::Interactive` never gets
            // this far (the sweep skips it before `reconcile_one`).
            if tier.is_none() && self.warned_no_tier.insert(p.id.clone()) {
                eprintln!(
                    "pmd: {} not driven — cannot read its autonomy from {} (autopilot is \
                     opt-in, so an unreadable tier counts as OFF)",
                    p.id,
                    entry_paths(p).config().display()
                );
            }
            return;
        }

        // One driver per project across processes: hold the project's lease
        // before driving. A second daemon (or a stray manual run) that can't take
        // it must not double-drive — skip and surface it once. For AgentLoop the
        // lease is PER-SESSION (`entry_paths` re-bases under `sessions/<id>/`), so
        // two sessions sharing one folder never share a lock.
        if r.lease.is_none() {
            let lock = entry_paths(p).daemon_dir().join("driver.lock");
            // RETRIED, not single-shot — the same reason pmd's startup singleton is (see
            // `bin/pmd.rs`): `flock` has no peek, so a contended attempt is not proof of a live
            // owner. A lock this daemon just RELEASED (the disabled / Standard-flip arms above set
            // `r.lease = None`) stays briefly held if a child pmd forked — an agent launch, a
            // notifier subprocess — inherited the lease fd, because a forked fd keeps the flock
            // alive until the child `exec`s (CLOEXEC only fires at exec). A single `try_acquire`
            // landing in that fork→exec window would falsely read "another process holds it" and
            // skip driving the row for a whole sweep (self-correcting next cadence, but a spurious
            // "already driven" log and a lost tick). A short retry rides the window out; a REAL
            // second daemon holds the lock for life and still loses every attempt, so the
            // one-driver-per-project guarantee is intact.
            const DRIVE_TRIES: u32 = 3;
            const DRIVE_RETRY: std::time::Duration = std::time::Duration::from_millis(50);
            match lease::acquire_with_retry(&lock, DRIVE_TRIES, DRIVE_RETRY) {
                Ok(Some(l)) => {
                    r.lease = Some(l);
                    r.lease_warned = false;
                }
                Ok(None) => {
                    if !r.lease_warned {
                        eprintln!(
                            "pmd: {} is already driven by another process (lease held) — skipping",
                            p.id
                        );
                        r.lease_warned = true;
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("pmd: {} lease error: {e}", p.id);
                    return;
                }
            }
        }

        // Drive this runner's session for a tick. Capture the tick result into an
        // owned value first so the mutable borrow of `r.driven` ends before we touch
        // `r`'s other fields (boot_failures, notified) below.
        let tick = {
            let Driven::Job(s) = &mut r.driven;
            // Refresh the adopt-arm seed each sweep BEFORE the tick: pmtui writes
            // `registry.conversation_id` AFTER this runner was built, so the seed
            // reaching the LIVE scheduler on every sweep is what lets the first
            // wake ADOPT the human-created conversation instead of minting.
            s.set_registry_seed(p.conversation_id.as_deref());
            s.tick(driver, clock)
        };
        match tick {
            // Agent-loop path — `JobScheduler` re-emits Escalated/Stuck on every
            // parked tick (no `Still*`/`Done` variants), so it uses a notify-once
            // dedup keyed on the per-session ledger's open-stop ids. A `Stuck` park
            // leaves a `Stuck` OpenStop that later ticks re-emit as
            // `Escalated([stuck_id])`, so both arms dedup on that id.
            Ok(tick) => {
                r.boot_failures = 0;
                let session_paths = entry_paths(p);
                match tick {
                    JobTick::Escalated(ids) => {
                        r.waiting_for_human = true;
                        let fresh: Vec<String> = ids
                            .iter()
                            .filter(|id| !r.notified.contains(*id))
                            .cloned()
                            .collect();
                        if !fresh.is_empty() {
                            let stops = job_stops(&session_paths, &fresh);
                            let _ = notifier.notify(&Escalation::for_stops(&p.id, &stops));
                            r.notified.extend(fresh);
                        }
                    }
                    JobTick::Stuck(reason) => {
                        r.waiting_for_human = true;
                        let ids = job_open_stop_ids(&session_paths);
                        let already_surfaced =
                            !ids.is_empty() && ids.iter().all(|id| r.notified.contains(id));
                        if !already_surfaced {
                            let _ = notifier.notify(&Escalation::stuck(&p.id, &reason));
                            r.notified.extend(ids);
                        }
                    }
                    // Any progress / idle outcome clears the dedup set so a future
                    // escalation/stuck re-arms (there is no machine `Done`).
                    JobTick::WaitingForIntake | JobTick::Monitoring { .. } => {
                        r.waiting_for_human = false;
                        r.notified.clear();
                    }
                }
            }
            // Poison path: consecutive tick errors (corrupt state) pause the project
            // after the threshold, surfaced once, rather than a tight loop.
            Err(e) => {
                r.boot_failures += 1;
                if r.boot_failures >= POISON_THRESHOLD && !r.poisoned {
                    r.poisoned = true;
                    let _ = notifier.notify(&Escalation::stuck(
                        &p.id,
                        &format!(
                            "paused after {} consecutive tick errors (corrupt state?): {e:#}",
                            r.boot_failures
                        ),
                    ));
                } else {
                    eprintln!("pmd: {} tick error: {e:#}", p.id);
                }
            }
        }
    }

    #[cfg(test)]
    fn tracks(&self, id: &str) -> bool {
        self.runners.contains_key(id)
    }
    #[cfg(test)]
    fn is_poisoned(&self, id: &str) -> bool {
        self.runners.get(id).map(|r| r.poisoned).unwrap_or(false)
    }

    pub fn shutdown_advice(
        &mut self,
        driver: &dyn Driver,
        clock: &dyn Clock,
    ) -> anyhow::Result<()> {
        for runner in self.runners.values_mut() {
            if !interrupt_runner_advice(runner, driver, clock.now(), "the daemon is shutting down")
            {
                anyhow::bail!("could not interrupt all deciders during daemon shutdown");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
