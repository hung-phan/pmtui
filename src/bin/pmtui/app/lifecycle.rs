//! Starting, stopping and ending a session: `p` pauses it, Enter on a paused row
//! resumes it, `r` restarts its agent, `d` closes it. One module because they share
//! one rule — `registry.enabled` is what actually stops pmd, so its write ordering
//! against the tmux kill is what keeps a "paused" row from coming back to life.

use crate::*;

impl App {
    /// `p` — PAUSE the selected session: stop the driving and kill the agent.
    ///
    /// The user's spec: *"Pause can stop the current session or autopilot, then kill claude/codex so
    /// it is not running. When i press enter on pause session, it will resume for me."*
    ///
    /// Both halves are needed and neither is enough alone. Clearing `registry.enabled` stops pmd
    /// (its sweep returns before anything else on a disabled row) but leaves the agent's process
    /// alive and idling — "not running" has to mean the process is gone. Killing the pane alone
    /// would have pmd relaunch it on the very next sweep.
    ///
    /// It writes BOTH switches, and which one does what matters:
    ///   * `registry.enabled = false` is the pause itself — pmd's sweep returns there before
    ///     anything else, and it is what `Enter` lifts.
    ///   * `config.autonomy = Standard` turns AUTOPILOT off, at the user's direction: *"when i press
    ///     pause, i want to stop autopilot too, only when i press m again, then it start again."*
    ///
    /// The second one is the deliberate change of mind. This first shipped preserving the tier, so
    /// that `Enter` restored the session exactly as it was, autopilot and all. The user's model is
    /// the opposite and it is the better one: pause means STOPPED, and a stop you can walk away from
    /// must not restart itself the moment you press the key that brings the session back. So the two
    /// ways out say two different things — `Enter` brings the session back for YOU to drive, and `m`
    /// is the one key that starts the driving again. Nothing here is lost by not preserving the
    /// tier: `m` is a single keystroke, and the goal on disk (which is what autopilot actually needs)
    /// is untouched.
    ///
    /// There WAS a `p` key, removed in m15 because `enabled:false` had no undo from this screen.
    /// That objection is answered by `Enter` resuming, so the badge reads "paused" and the key is
    /// back.
    pub(crate) fn pause_session(&mut self) {
        let Some(v) = self.selected_view() else {
            self.status = "p pauses a session (nothing is selected)".into();
            return;
        };
        let id = v.id.clone();
        let mut reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter_mut().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        if !e.enabled {
            if !self.refuse_unstartable(e) {
                self.status = format!("{id} is already paused — Enter resumes it");
            }
            return;
        }
        let (root, session) = (e.root.clone(), session_name(&e.id, &e.root));
        // DISABLE FIRST, then kill. The other order races the daemon: pmd's sweep runs every 500ms,
        // so a pane killed while the row is still enabled can be relaunched before the registry
        // write lands, leaving a "paused" row with a live agent.
        e.enabled = false;
        if let Err(err) = reg.save(&self.registry_path) {
            self.status = format!("{id}: could not pause ({err})");
            return;
        }
        // AUTOPILOT OFF, through the same `config.json` the daemon reads and `m` writes — so `m` is
        // genuinely the thing that starts it again, rather than a dial that was never turned.
        //
        // AFTER the registry write, not before: `enabled:false` is what actually stops pmd, so it
        // goes first and a failure here can only leave a session that is stopped anyway. Reported,
        // though, and not swallowed — a paused row still reading Autopilot would restart the driving
        // the moment `Enter` resumed it, which is precisely what the user asked to stop.
        let cfg_path = ProjectPaths::for_session(&root, &id).config();
        let dial_off = match state::read_json::<Config>(&cfg_path) {
            Ok(mut c) if c.autonomy == Tier::Autopilot => {
                c.autonomy = Tier::Standard;
                state::write_json_atomic(&cfg_path, &c).map_err(|e| e.to_string())
            }
            // Already Standard: nothing to turn off, and no pointless write on the human's file.
            Ok(_) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
        let ended = self.agent_tmux.terminate(&session);
        // The row IS paused (the registry write landed) whatever else failed, so each of these says
        // what did and did not happen rather than implying nothing changed. Autopilot is named
        // first when it is the part that went wrong: it is the half that would come back by itself.
        self.status = match (dial_off, ended) {
            (Ok(()), Ok(())) => {
                format!("{id} paused — autopilot off, agent stopped; Enter resumes")
            }
            (Ok(()), Err(err)) => format!("{id} paused, but its pane may still be up ({err})"),
            (Err(e), _) => {
                format!("{id} paused, but autopilot could not be turned off ({e}) — m it off")
            }
        };
        self.refresh();
    }

    /// Flip one row's `registry.enabled`, leaving every other row untouched.
    ///
    /// Read-modify-write on the whole file because that is the registry's only shape; a missing row
    /// is an error rather than a silent no-op, since both callers are reporting an outcome to the
    /// human and "done" over a row that is gone is the wrong answer.
    pub(crate) fn set_row_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        // Through `Registry::update` (corrupt-safe): an unreadable registry now reports honestly
        // instead of reading as empty and claiming the row is "gone". Row-not-found is still an
        // error, checked after so a valid registry is never left half-updated.
        let mut found = false;
        Registry::update(&self.registry_path, |reg| {
            if let Some(e) = reg.projects.iter_mut().find(|p| p.id == id) {
                e.enabled = enabled;
                found = true;
            }
        })
        .map_err(|e| e.to_string())?;
        if found {
            Ok(())
        } else {
            Err(format!("{id} is gone from the list"))
        }
    }

    /// Enter on a paused row — bring the session back, for the human to drive.
    ///
    /// Re-enabling is the whole of it. Nothing here restores a tier, and since `pause_session` turns
    /// autopilot off, in practice this always lands on Standard: the session is back, attachable and
    /// unchanged, and NOTHING is driving it until `m`. That is the user's design — *"only when i
    /// press m again, then it start again"* — so the status names `m` rather than leaving the human
    /// to wonder why a resumed session is sitting still.
    ///
    /// The tier is still READ rather than assumed, because `enabled:false` is reachable without `p`
    /// (`d`'s confirm path, an out-of-band registry edit), and resuming one of those must honour
    /// whatever dial it actually carries. `ensure_daemon` runs only if that read says Autopilot:
    /// starting a daemon "for" a Standard row would drive every OTHER enabled project while doing
    /// nothing here.
    pub(crate) fn resume_session(&mut self, id: &str, root: &Path) {
        let mut reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter_mut().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        if self.refuse_unstartable(e) {
            return;
        }
        e.enabled = true;
        let (engine, mode, worker_model) = (
            e.engine.unwrap_or(Engine::Claude),
            e.mode,
            e.worker_model.clone(),
        );
        if let Err(err) = reg.save(&self.registry_path) {
            self.status = format!("{id}: could not resume ({err})");
            return;
        }
        // Read the tier back from the per-session config — the dial as it stood when the session was
        // paused, which pause deliberately did not touch.
        let tier = state::read_json::<Config>(&ProjectPaths::for_session(root, id).config())
            .map(|c| c.autonomy)
            .unwrap_or(Tier::Standard);
        // AND START IT. `pause_session` KILLS the agent (*"Pause can stop the current session or
        // autopilot, then kill claude/codex so it is not running"*), so re-enabling the row on its own
        // left a session with nothing running in it — and on Standard nothing ever would. User: *"enter
        // after press p to pause … it should start the session and render it immediately"*.
        self.status = if tier == Tier::Autopilot {
            format!("{id} resumed on Autopilot; {}", self.ensure_daemon())
        } else {
            let started = self
                .start_after_lifecycle_key(id, root, engine, mode, worker_model.as_deref())
                .unwrap_or_default();
            format!("{id} resumed{started} — you drive it; m starts autopilot")
        };
        self.restart_daemon_watch();
        self.refresh();
    }

    /// `r` — offer to restart the selected row's agent.
    ///
    /// Always a CONFIRM: a restart throws away whatever turn the agent is mid-way
    /// through, and `r` sits one key from `d`.
    pub(crate) fn begin_restart(&mut self) {
        let Some(v) = self.selected_view() else {
            self.status = "r restarts a session's agent (nothing is selected)".into();
            return;
        };
        let id = v.id.clone();
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        self.mode = UiMode::Confirming {
            session: session_name(&e.id, &e.root),
            id,
            what: Confirmable::Restart,
        };
    }

    fn refuse_codex_restart(&mut self, id: &str, reason: &str, cycle_daemon: bool) {
        let daemon = cycle_daemon.then(|| self.ensure_daemon());
        self.restart_daemon_watch();
        self.status = match daemon {
            Some(daemon) => {
                format!("{id}: {reason}; agent left unchanged; {daemon}")
            }
            None => format!("{id}: {reason}; agent left unchanged"),
        };
        self.refresh();
    }

    /// Restart the selected project terminal without changing its tier.
    ///
    /// For an Autopilot row, stop pmd first so it cannot nudge or relaunch during
    /// teardown, terminate the one unified terminal, then start pmd again. A Standard
    /// row restarts its terminal directly and leaves any shared daemon alone.
    ///
    /// The registry, tier, control file, and ledger remain intact, so the agent resumes
    /// its recorded conversation. A Standard Codex row may not have recorded its rollout
    /// yet; in that case pmtui captures the exact user rollout from the live process and
    /// persists it before teardown. If that identity cannot be proven, restart refuses
    /// without touching the pane. A restart may lose an in-flight reply and is therefore
    /// confirmation-gated.
    pub(crate) fn restart_agent(&mut self, id: &str) {
        self.mode = UiMode::Normal;
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list — nothing restarted");
            self.refresh();
            return;
        };
        if self.refuse_unstartable(e) {
            return;
        }
        // Everything owned UP FRONT, so the `reg` borrow is done before the `&mut self` calls.
        let session = session_name(&e.id, &e.root);
        let paths = entry_state_paths(e);
        let (root, engine, mode, worker_model, registry_cid) = (
            e.root.clone(),
            e.engine.unwrap_or(Engine::Claude),
            e.mode,
            e.worker_model.clone(),
            e.conversation_id.clone(),
        );
        let ledger_cid = job::load(&paths)
            .ok()
            .flatten()
            .and_then(|ledger| ledger.conversation_id);
        let tier = state::read_json::<Config>(&ProjectPaths::for_session(&root, id).config())
            .map(|c| c.autonomy)
            .unwrap_or(Tier::Standard);
        let cycle_daemon = agent_manager::daemon::pmd_drives_row(mode, Some(tier));
        self.restart_daemon_watch();
        let daemon_before = cycle_daemon.then(|| self.daemon_live());
        let stopped = cycle_daemon && self.stop_daemon();
        if cycle_daemon && !stopped && daemon_before != Some(DaemonLive::Down) {
            let daemon = self.ensure_daemon();
            self.restart_daemon_watch();
            self.status = format!("{id}: could not stop pmd; agent left unchanged; {daemon}");
            self.refresh();
            return;
        }
        if engine == Engine::Codex {
            let session_exists = match self.agent_tmux.is_alive(&session) {
                Ok(alive) => alive,
                Err(err) => {
                    self.refuse_codex_restart(
                        id,
                        &format!("could not determine whether the Codex pane is live ({err})"),
                        cycle_daemon,
                    );
                    return;
                }
            };
            let alive = if session_exists {
                match self.agent_tmux.pane_dead(&session) {
                    Ok(dead) => !dead,
                    Err(err) => {
                        self.refuse_codex_restart(
                            id,
                            &format!("could not determine whether the Codex pane exited ({err})"),
                            cycle_daemon,
                        );
                        return;
                    }
                }
            } else {
                false
            };
            if alive {
                let captured = match self.agent_tmux.codex_session_id(&session, &root) {
                    Ok(Some(id)) => id,
                    Ok(None) => {
                        self.refuse_codex_restart(
                            id,
                            "could not identify the live Codex conversation",
                            cycle_daemon,
                        );
                        return;
                    }
                    Err(err) => {
                        self.refuse_codex_restart(
                            id,
                            &format!("could not identify the live Codex conversation ({err})"),
                            cycle_daemon,
                        );
                        return;
                    }
                };
                if let Some(ledger_id) = ledger_cid.as_deref()
                    && ledger_id != captured
                {
                    self.refuse_codex_restart(
                        id,
                        &format!(
                            "live Codex conversation {captured} does not match ledger id {ledger_id}"
                        ),
                        cycle_daemon,
                    );
                    return;
                }
                if ledger_cid.is_none()
                    && registry_cid.as_deref() != Some(captured.as_str())
                    && let Err(err) = self.seed_registry_conversation_id(id, &captured)
                {
                    self.refuse_codex_restart(
                        id,
                        &format!("could not save the live Codex conversation ({err})"),
                        cycle_daemon,
                    );
                    return;
                }
            }
        }
        let driver = &*self.agent_tmux;
        // `terminate` is idempotent on a session that is already gone, so a row whose
        // agent had already died still gets its daemon back rather than a refusal.
        let ended = driver.terminate(&session);
        chat_lock::clear(&paths);
        // Cycle the daemon ONLY for a driven row (see above). `None` on a Standard row is
        // what the status reads as "pmd left running".
        let restarted = cycle_daemon.then(|| self.ensure_daemon());
        // AND BRING IT BACK, when nobody else will. `r` used to end here: it stopped the agent,
        // cycled the daemon and returned — which on a Standard row (`pmd_drives_row` is false for one
        // by design) left the agent dead and the pane empty until the human pressed something else.
        // User: *"when i press r to restart … it should start the session and render it
        // immediately"*. On a driven row this returns `None` and pmd does the launch.
        let started =
            self.start_after_lifecycle_key(id, &root, engine, mode, worker_model.as_deref());
        // Without this the cached sample would report the daemon we just cycled for up to
        // `DAEMON_PROBE_TTL`, so the header would lie about it for a few seconds.
        self.restart_daemon_watch();
        let started = started.unwrap_or_default();
        self.status = match ended {
            Err(err) => format!("{id}: could not stop the agent: {err}"),
            Ok(()) => match restarted {
                // DRIVEN row: pmd was cycled. `stop_daemon` reports whether a daemon
                // was actually there to stop — the difference between "restarted" and
                // "started for the first time".
                Some(restarted) if stopped => {
                    format!("{id} restarting — pmd stopped, pane ended; {restarted}{started}")
                }
                Some(restarted) => {
                    format!("{id}: agent stopped (pmd was not running); {restarted}{started}")
                }
                // STANDARD row: the shared pmd was left alone and this row was
                // relaunched directly for the human.
                None => format!("{id} restarted — pane ended, pmd left running{started}"),
            },
        };
        self.refresh();
    }

    /// `d` on the selected project. Closing an agent-loop row IS the only "done"
    /// (design: the human closes/deletes). Closing a running agent is significant, so
    /// it first asks for confirmation so a running agent isn't fat-fingered away.
    pub(crate) fn begin_delete(&mut self) {
        // Same silent dead end `request_attach` and `cycle_tier` had: `d` on an empty
        // list must say why nothing happened.
        let Some(v) = self.selected_view() else {
            self.status = "d closes a session (nothing is selected)".into();
            return;
        };
        let id = v.id.clone();
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            // The status was here already; the `refresh()` was not, which left the
            // stale row on screen for `d` to refuse again forever. Every other handler
            // that finds a row gone (`cycle_tier`, `request_attach`, `selected_brief`,
            // `remove_project`) drops it — this one now agrees.
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        // Route the unified project terminal through a confirm gate so a running
        // agent is not removed by an accidental keypress.
        self.mode = UiMode::Confirming {
            id,
            session: session_name(&e.id, &e.root),
            what: Confirmable::Remove,
        };
    }

    /// `a` — gate applying a finished job's commit behind a confirmation.
    ///
    /// Refuses out loud rather than silently: a chat row has no commit, a job that ran outside a
    /// repository has none either, and one that committed nothing has nothing to bring over. Each of
    /// those is a different sentence, because "nothing happened" is the one answer a key must never give.
    pub(crate) fn begin_apply(&mut self) {
        let Some(v) = self.selected_view() else {
            self.status = "a applies a finished job's commit (nothing is selected)".into();
            return;
        };
        if !v.job {
            self.status = format!("{} is not a job; a applies a job's commit", v.id);
            return;
        }
        let (id, commit, branch) = (v.id.clone(), v.job_commit.clone(), v.job_branch.clone());
        let Some(commit) = commit else {
            self.status = match branch {
                Some(branch) => format!("{id} committed nothing on {branch}"),
                None => format!("{id} has no worktree of its own, so there is nothing to apply"),
            };
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        self.status = format!(
            "apply {} from {} onto this checkout?",
            &commit[..commit.len().min(8)],
            branch.unwrap_or_default()
        );
        self.mode = UiMode::Confirming {
            id,
            session: session_name(&entry.id, &entry.root),
            what: Confirmable::Apply,
        };
    }

    /// Cherry-pick one job's commit onto its project's own checkout.
    ///
    /// The row is KEPT either way. A successful apply still leaves a job for a human to look at and clear
    /// with `d`, because they may want the log, and a conflict leaves the checkout exactly as it was with
    /// the conflict named — this is the human's working tree, so nothing here resolves anything for them.
    pub(crate) fn apply_job_commit(&mut self, id: &str) {
        self.mode = UiMode::Normal;
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(row) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        let Some(work) = job_worktree(row).map(|wt| agent_manager::worktree::collect(&wt)) else {
            self.status = format!("{id} has no worktree of its own, so there is nothing to apply");
            return;
        };
        let Some(commit) = work.commit.as_deref() else {
            self.status = format!("{id} committed nothing on {}", work.branch);
            return;
        };
        let short = &commit[..commit.len().min(8)];
        self.status = match agent_manager::worktree::integrate(&row.root, commit) {
            agent_manager::worktree::Integration::Applied(head) => {
                format!(
                    "applied {short} from {} \u{2014} this checkout is now {} ({} files)",
                    work.branch,
                    &head[..head.len().min(8)],
                    work.touched.len()
                )
            }
            agent_manager::worktree::Integration::Conflicted(why) => format!(
                "{short} conflicts with this checkout, which is unchanged \u{2014} {}",
                why.lines().next().unwrap_or_default().trim()
            ),
            agent_manager::worktree::Integration::Refused(why) => {
                format!(
                    "{short} was not applied \u{2014} {}",
                    why.lines().next().unwrap_or_default().trim()
                )
            }
        };
        self.record_status();
        self.refresh();
    }

    /// Kill the tmux session (best-effort — it may already be dead) and drop the
    /// registry entry. `.project-state/` is intentionally left on disk, and the
    /// project's own source files are never touched.
    pub(crate) fn remove_project(&mut self, id: &str, session: &str) {
        let _ = self.agent_tmux.terminate(session);
        let mut reg = Registry::load(&self.registry_path).unwrap_or_default();
        let before = reg.projects.len();
        // Capture the root + mode before dropping the entry so we can clear its
        // pmtui-owned markers below, and whether it was a JOB, whose state goes with it.
        let removed = reg
            .projects
            .iter()
            .find(|p| p.id == id)
            .map(|p| (p.root.clone(), p.mode, p.is_job(), job_worktree(p)));
        // A JOB's parent is waiting on a receipt. Removing the row by hand IS a cancellation, so say so
        // before the row is gone — otherwise an agent polls `ready` for a child that no longer exists.
        if let Some(row) = reg.projects.iter().find(|p| p.id == id).cloned() {
            self.cancel_job_receipt(&row);
        }
        reg.projects.retain(|p| p.id != id);
        if reg.projects.len() == before {
            self.message_drafts.remove(id);
            self.initial_message_retries.remove(id);
            self.status = format!("{id} is gone from the list");
            self.mode = UiMode::Normal;
            self.refresh();
            return;
        }
        // Delete removes the session for good, so clear the pmtui-owned resume
        // marker (`session.json`). Otherwise a NEW session created later at the
        // same path would find the stale marker and resume this deleted
        // conversation instead of starting fresh. The rest of `.project-state/`
        // and the source tree are left untouched.
        if let Some((root, mode, was_job, worktree)) = removed {
            let _ = std::fs::remove_file(ProjectPaths::new(&root).session());
            // Clear attach intent so a reused id at the same root never inherits it.
            if mode == Mode::AgentLoop {
                chat_lock::clear(&ProjectPaths::for_session(&root, id));
            }
            // A JOB'S STATE LIVES EXACTLY AS LONG AS ITS ROW. A job is one finished run, and what it
            // achieved is in the parent's receipt — summary, detail and error — so once the row is gone
            // its `.project-state/sessions/<seg>/` is a stale copy of a conversation nobody can resume.
            // Only the child's own subtree: the parent's state, the project ledger and the source tree
            // are untouched, and the guard for that lives in `purge_session_state` itself. A human's
            // session is NEVER purged this way — no machine decides a human's work is finished.
            // THE WORKTREE GOES THROUGH GIT, and only when it holds nothing but commits. `git worktree
            // remove` is what makes the repository forget it; a plain delete would leave admin files
            // behind. A worktree with uncommitted changes REFUSES, and then the state purge is skipped
            // too — those changes exist nowhere else, so the one thing this must never do is delete them
            // to tidy up. The branch stays either way: a commit's last home is not ours to drop.
            let mut keep_state = false;
            if let Some(worktree) = worktree.as_ref()
                && let Err(error) = agent_manager::worktree::prune(&root, worktree)
            {
                keep_state = true;
                self.log_line(&format!(
                    "spawn: {id}'s worktree is kept on {} ({error:#})",
                    worktree.branch
                ));
            }
            if was_job
                && !keep_state
                && let Err(error) =
                    agent_manager::state::purge_session_state(&ProjectPaths::for_session(&root, id))
            {
                self.log_line(&format!("spawn: could not clear {id}'s state ({error:#})"));
            }
        }
        match reg.save(&self.registry_path) {
            Ok(()) => {
                self.message_drafts.remove(id);
                self.initial_message_retries.remove(id);
                self.status = format!("removed {id}");
            }
            Err(e) => self.status = format!("save failed: {e}"),
        }
        self.mode = UiMode::Normal;
        self.refresh();
    }
}
