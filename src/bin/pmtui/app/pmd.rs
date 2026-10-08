//! The `pmd` process, from pmtui's side: probe whether one is up (cached, because the
//! render path must not syscall per frame), start one when a session needs it, stop one
//! for a restart, and reap the children we spawned. `Tier::Autopilot` is the "let pmd
//! drive this" switch, so every path that turns it on ends up here.

use crate::*;

impl App {
    pub(crate) fn pmd_path_for_executable(executable: &Path) -> Option<PathBuf> {
        executable.parent().map(|parent| parent.join("pmd"))
    }

    /// The daemon-liveness answer for this frame: a cached observation, re-probed at
    /// most once per [`DAEMON_PROBE_TTL`]. `&self` because the render path only has
    /// `&App` (interior mutability, like `scroll_max`).
    ///
    /// ONE probe path for the whole program: the status-bar chip and the armed-drain
    /// disarm (see [`armed_wait_decision`]) read this same cached value, so the screen
    /// and the decision can never disagree about whether pmd is up.
    pub(crate) fn daemon_live(&self) -> DaemonLive {
        let now = std::time::Instant::now();
        if let Some((at, seen)) = self.daemon_live.get()
            && now.duration_since(at) < DAEMON_PROBE_TTL
        {
            return seen; // cache hit — no syscall, and the streak does NOT advance
        }
        let seen = probe_daemon_live(&self.registry_path, &self.socket);
        self.daemon_live.set(Some((now, seen)));
        self.daemon_down_streak.set(match seen {
            DaemonLive::Down => self.daemon_down_streak.get().saturating_add(1),
            // Up or Unknown breaks the streak: only an unbroken run of confirmed-DOWN
            // samples may be acted on.
            _ => 0,
        });
        seen
    }

    /// Reap any `pmd` WE spawned that has since exited, so a restart cannot leave a
    /// `<defunct>` process behind (see [`App::spawned_pmd`]).
    ///
    /// Never blocks and never signals: `try_wait` only collects an exit status that is
    /// already there. A daemon still running is retained, so this is safe to call as often
    /// as it is cheap to.
    pub(crate) fn reap_spawned_pmd(&self) {
        if let Ok(mut kids) = self.spawned_pmd.try_borrow_mut() {
            kids.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
        }
    }

    /// Start a FRESH liveness observation window: forget the cached sample and zero the
    /// consecutive-DOWN streak.
    ///
    /// Called when an Autopilot arm is set, right after `ensure_daemon`. Without it the
    /// streak would carry over from BEFORE the ensure — a pmtui that had been sitting in
    /// front of a dead daemon for a minute would arm with a streak of 20 and disarm on
    /// the very next drain, in front of a `pmd` that is booting perfectly normally.
    pub(crate) fn restart_daemon_watch(&self) {
        self.daemon_live.set(None);
        self.daemon_down_streak.set(0);
    }

    /// STARTUP recovery for "autopilot says ON but nothing is driving it".
    ///
    /// `m` can only start the daemon on the OFF→ON edge, which leaves a real hole:
    /// pmtui opens, a session is already on Autopilot, and pmd is dead (crash, reboot,
    /// `pkill`). Nothing runs, and because the tier flip is a pure 2-value toggle the
    /// human has to press `m` TWICE (off, then on) to get a daemon back. Close that
    /// once, at open: if ANY enabled session pmd drives already reads Autopilot, ensure
    /// the daemon.
    ///
    /// Best-effort by construction, so it can never keep the dashboard from opening: a
    /// missing or unparseable registry/config is skipped rather than raised,
    /// `ensure_daemon` runs AT MOST ONCE however many sessions are on Autopilot, and
    /// with nothing on Autopilot this is a silent no-op that leaves `status` alone and
    /// does not even create the daemon lock file.
    pub(crate) fn ensure_daemon_for_enabled_autopilot(&mut self) {
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        // The SAME config-path resolution `cycle_tier` writes through (per-session for
        // AgentLoop, the shared root config otherwise) and the same `pmd_drives` gate,
        // so what we read here is exactly the dial `m` sets and the daemon obeys. Already
        // filtered on Autopilot, which is exactly the m15 rule (`daemon::pmd_drives_row`):
        // a Standard agent-loop row is undriven by design, so it must not start a daemon
        // — verified, no change needed here.
        let on_autopilot: Vec<&str> = reg
            .enabled()
            .filter(|p| pmd_drives(p.mode))
            .filter(|p| {
                state::read_json::<Config>(&entry_state_paths(p).config())
                    .is_ok_and(|c| c.autonomy == Tier::Autopilot)
            })
            .map(|p| p.id.as_str())
            .collect();
        let label = match on_autopilot.as_slice() {
            [] => return,
            [one] => format!("{one} is on Autopilot"),
            many => format!("{} sessions on Autopilot", many.len()),
        };
        self.status = format!("{label}; {}", self.ensure_daemon());
    }

    /// Start `pmd` (detached, same socket+registry) unless it is already running.
    /// Returns the outcome (see [`DaemonEnsure`], whose `Display` is the status
    /// fragment every print-only caller shows). Liveness = the daemon singleton flock:
    /// if we can take it, no daemon is up (drop it, then spawn); if not, one already
    /// holds it.
    pub(crate) fn ensure_daemon(&self) -> DaemonEnsure {
        let owner = lease::socket_owner_lock_path(&self.socket);
        self.ensure_daemon_with(&owner, || self.spawn_daemon())
    }

    pub(crate) fn ensure_daemon_with(
        &self,
        owner: &Path,
        spawn: impl FnOnce() -> DaemonEnsure,
    ) -> DaemonEnsure {
        let lock = lease::daemon_lock_path(&self.registry_path, &self.socket);
        if let Some(parent) = lock.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match lease::try_acquire(&lock) {
            // Was free => no daemon is running => drop the probe lease so pmd can
            // take it, then spawn.
            Ok(Some(l)) => {
                drop(l);
                match lease::try_acquire(owner) {
                    Ok(Some(owner_probe)) => {
                        drop(owner_probe);
                        spawn()
                    }
                    Ok(None) => DaemonEnsure::Failed(format!(
                        "socket {} belongs to another registry",
                        self.socket
                    )),
                    Err(e) => DaemonEnsure::Failed(format!("socket ownership check failed: {e}")),
                }
            }
            Ok(None) => DaemonEnsure::AlreadyRunning,
            Err(e) => DaemonEnsure::Failed(format!("daemon check failed: {e}")),
        }
    }

    /// Spawn `pmd` (the sibling binary next to pmtui's own exe) detached, same
    /// socket+registry, logging to `<registry-dir>/pmd.log`. Detached: its own
    /// process group + no controlling tty (null stdin + redirected stdout/stderr),
    /// so it doesn't take pmtui's Ctrl-C/signals and keeps running after pmtui exits
    /// (it's then reparented to init). Note: without `setsid` (which needs `libc`, a
    /// dep we don't take) pmd stays in pmtui's session, so closing the terminal
    /// window while pmtui is still up could SIGHUP it — acceptable for now.
    pub(crate) fn spawn_daemon(&self) -> DaemonEnsure {
        self.spawn_daemon_for_executable(std::env::current_exe())
    }

    pub(crate) fn spawn_daemon_for_executable(
        &self,
        executable: std::io::Result<PathBuf>,
    ) -> DaemonEnsure {
        let Ok(exe) = executable else {
            return DaemonEnsure::Failed("could not locate pmtui exe".into());
        };
        let pmd = match Self::pmd_path_for_executable(&exe) {
            Some(pmd) => pmd,
            None => return DaemonEnsure::Failed("could not locate pmd".into()),
        };
        self.spawn_daemon_from(&pmd)
    }

    pub(crate) fn spawn_daemon_from(&self, pmd: &Path) -> DaemonEnsure {
        use std::os::unix::process::CommandExt;

        if !pmd.exists() {
            return DaemonEnsure::Failed(format!("pmd binary not found at {}", pmd.display()));
        }
        let log_dir = match self.registry_path.parent() {
            Some(parent) => parent,
            None => Path::new("."),
        };
        let log_path = log_dir.join("pmd.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path);
        let mut cmd = std::process::Command::new(pmd);
        cmd.arg("--socket")
            .arg(&self.socket)
            .arg("--registry")
            .arg(&self.registry_path)
            .stdin(std::process::Stdio::null())
            .process_group(0);
        match log {
            Ok(f) => {
                let f2 = f.try_clone().ok();
                cmd.stdout(std::process::Stdio::from(f));
                // stderr must NEVER fall back to inherit: an inherited fd would let
                // pmd's `eprintln!` corrupt pmtui's live ratatui dashboard. If the
                // clone failed, send stderr to /dev/null instead.
                match f2 {
                    Some(f2) => cmd.stderr(std::process::Stdio::from(f2)),
                    None => cmd.stderr(std::process::Stdio::null()),
                };
            }
            Err(_) => {
                cmd.stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
            }
        }
        // Reap BEFORE recording the new child: a pmtui that restarts repeatedly would
        // otherwise accumulate one handle per press even though each is already dead.
        self.reap_spawned_pmd();
        match cmd.spawn() {
            Ok(child) => {
                if let Ok(mut kids) = self.spawned_pmd.try_borrow_mut() {
                    kids.push(child);
                }
                DaemonEnsure::Started
            }
            Err(e) => DaemonEnsure::Failed(format!("daemon start failed: {e}")),
        }
    }

    /// Stop the daemon for this `(registry, socket)`. Returns whether a live owner
    /// consumed the stop request and released its lock.
    ///
    /// Liveness is the singleton flock. The request is an atomic file write consumed
    /// by that owner, so pmtui never signals an unverified or recycled PID.
    pub(crate) fn stop_daemon(&self) -> bool {
        self.stop_daemon_with_budget(DAEMON_STOP_TRIES, DAEMON_STOP_WAIT)
    }

    pub(crate) fn stop_daemon_with_budget(&self, tries: u32, wait: std::time::Duration) -> bool {
        let lock = lease::daemon_lock_path(&self.registry_path, &self.socket);
        match lease::try_acquire(&lock) {
            Ok(Some(_)) => return false,
            Ok(None) => {}
            Err(_) => return false,
        }
        let stop = lease::daemon_stop_path(&self.registry_path, &self.socket);
        if state::write_text_atomic(&stop, &format!("{}\n", std::process::id())).is_err() {
            return false;
        }
        // Wait for the lock to come free — the only proof the daemon is really gone — so
        // the `ensure_daemon` that follows does not read a still-held lock and decline to
        // spawn. Bounded, because a dashboard must never hang on a process that refuses
        // to die: if it outlasts us, `ensure_daemon` reports `AlreadyRunning` and the
        // human sees that rather than a frozen screen.
        for _ in 0..tries {
            if matches!(lease::try_acquire(&lock), Ok(Some(_))) {
                // The lock coming free IS the moment the daemon died, so this is exactly
                // when there is an exit status to collect. Reaping here (rather than only at
                // the next spawn) is what makes a single `r` leave nothing behind.
                self.reap_spawned_pmd();
                return true;
            }
            std::thread::sleep(wait);
        }
        false
    }
}
