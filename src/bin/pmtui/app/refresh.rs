//! Re-reading the world each idle tick, and where the cursor sits in it. The two
//! belong together: a refresh RE-SORTS, so it has to follow the selected session by
//! id rather than by index, and the armed auto-open is drained off the same tick.

use crate::*;

/// How many lines of the live pane to capture when classifying a managed row's activity —
/// matches the daemon drive loop's capture depth (`src/job_engine/drive.rs`).
const ACTIVITY_CAPTURE_LINES: usize = 40;
/// Consecutive byte-stable Idle observations required before a managed row flips to idle (`○`) —
/// mirrors the daemon's `IDLE_CONFIRMATIONS_REQUIRED`. Two ~500ms ticks of a STABLE transcript.
const ACTIVITY_IDLE_CONFIRMATIONS: u32 = 2;

/// One step of the working-vs-idle CONFIRMATION GATE: given the PREVIOUS `(fingerprint, count)` for
/// a row and the CURRENT idle fingerprint, return the new pair. A stable fingerprint bumps the
/// count; a changed one — a streaming transcript growing between captures — RESETS it to 1, so a
/// mid-stream pane never reaches the confirmation threshold and never falsely reads idle. Pure, so
/// the streaming-vs-stable logic is unit-tested without tmux.
fn idle_gate_step(prev: Option<(u64, u32)>, fp: u64) -> (u64, u32) {
    match prev {
        Some((f, c)) if f == fp => (fp, c.saturating_add(1)),
        _ => (fp, 1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityObservation {
    Missing,
    Unreadable,
    Busy,
    IdleUnconfirmed,
    IdleConfirmed,
}

fn merge_activity(
    engine: Option<Engine>,
    hook_working: Option<bool>,
    observation: ActivityObservation,
) -> Option<bool> {
    match observation {
        ActivityObservation::Missing => Some(false),
        ActivityObservation::Unreadable
        | ActivityObservation::Busy
        | ActivityObservation::IdleUnconfirmed => Some(true),
        // Claude can leave its bare prompt visible through a silent tool call, so its
        // hook-backed in-flight signal remains authoritative. Codex paints an explicit busy
        // banner while a turn runs; after two byte-stable idle-composer captures, an unchanged
        // optional notify byte is stale rather than stronger evidence of work.
        ActivityObservation::IdleConfirmed
            if hook_working == Some(true) && engine != Some(Engine::Codex) =>
        {
            Some(true)
        }
        ActivityObservation::IdleConfirmed => Some(false),
    }
}

/// [`merge_activity`] for a caller that ran its own two-capture idle confirmation, so the fork's
/// source recheck weighs the turn hook exactly as the dashboard row does.
pub(super) fn confirmed_idle_activity(
    engine: Option<Engine>,
    hook_working: Option<bool>,
) -> Option<bool> {
    merge_activity(engine, hook_working, ActivityObservation::IdleConfirmed)
}

impl App {
    /// Everything the dashboard does AFTER one frame's input, in one place: the idle-tick refresh,
    /// the armed first-chat, and the status log.
    ///
    /// A named step rather than three statements at the bottom of `run`'s loop, because the loop body
    /// is the one part of this binary a test cannot reach — it needs a terminal and an event source.
    /// Feeding the log was written into that loop first and NOTHING failed when it was left out: every
    /// test called `record_status` itself, so the log worked in the tests and stayed empty in the app.
    /// That is the same shape as the m41 half-fix (a test proving a mechanism works is not a test
    /// proving the symptom is gone), so the bookkeeping moved to where it can be asserted.
    ///
    /// `handled_key` = this frame consumed input, which SUPPRESSES the refresh: `refresh` shells
    /// `tmux has-session` per project, and doing that per keystroke is the subprocess storm the loop
    /// has always avoided. State-changing actions refresh themselves.
    pub(crate) fn after_input(&mut self, handled_key: bool) {
        if !handled_key
            && self.dashboard_owner_nonce.as_deref().is_some_and(|nonce| {
                takeover::requested_for(&self.registry_path, &self.socket, nonce)
            })
        {
            self.status = "another pmtui was confirmed; handing over the dashboard".into();
            self.should_quit = true;
            return;
        }
        // Skip the refresh entirely in the wake-follow view — it does not show the project list, and
        // `render_wake_view` re-reads the log itself each frame, so following is unaffected.
        if !handled_key && !matches!(self.mode, UiMode::WakeView { .. }) {
            self.refresh();
            // Auto-open an armed first-chat ONLY on an idle tick and ONLY in Normal mode — never as a
            // side effect of an action refresh (tier/answer/create) and never while a modal
            // (Answering/Creating/Confirming) is up. `drain_armed_first_chat` re-reads the ledger
            // itself, so it does not depend on the `refresh()` above having just run.
            if matches!(self.mode, UiMode::Normal) {
                self.drain_armed_first_chat();
            }
        }
        // FEED THE STATUS LOG, from ONE place: after every path above has had its chance to set
        // `status` — a key, an attach/chat return, a send, a refresh. One call site rather than one
        // per writer, because ~50 sites assign `status` directly and any of them could be the one
        // that forgets. `record_status` is idempotent (it drops empties and collapses a repeat of the
        // newest entry), so running it every frame — including idle ticks where nothing happened —
        // costs nothing and cannot double-log.
        self.record_status();
    }

    pub(crate) fn refresh(&mut self) {
        // WHICH SESSION is selected, not which INDEX. A refresh re-sorts, and an action that
        // changes a row's section moves it: pausing the top row used to leave the cursor on
        // whatever slid up into its place, so a reflex second `p` would have paused a DIFFERENT
        // session. Seen live, one keystroke after the sections landed.
        //
        // The same hazard was always there under the posture sort (a row that starts working
        // moves), just rarer — the sections made it happen on every pause.
        let keep = self.selected_view().map(|v| v.id.clone());
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        if self.is_spawn_broker() {
            self.discover_spawn_requests(&reg);
            // AFTER the request scan: a job's receipt goes final at `ready` (launched and running),
            // so the scan has already settled it while the work is still going. This is the pass that
            // notices the work ended and writes what became of it.
            self.retire_finished_jobs(&reg);
        }
        let driver = TmuxDriver::with_socket(&self.socket);
        let now = SystemClock.now();
        // The working-vs-idle gate carries state across ticks. TAKE it out (leaving self.idle_gate
        // empty) so the row-building closure can read `prev_gate` and fill a fresh `new_gate` with
        // no borrow of `self`; the fresh map is written back after, which auto-evicts any id not
        // seen this tick.
        let prev_gate = std::mem::take(&mut self.idle_gate);
        let mut new_gate: std::collections::HashMap<String, (u64, u32)> =
            std::collections::HashMap::new();
        let mut views: Vec<ProjectView> = reg
            .projects
            .iter()
            .map(|p| {
                // An AgentLoop session's truth is its per-session ledger
                // (`sessions/<id>/`), read via `entry_state_paths`.
                let mut v =
                    ProjectView::read_agent_loop(&p.id, &entry_state_paths(p), p.enabled, now);
                let paths = entry_state_paths(p);
                v.display_name = p
                    .display_name
                    .as_deref()
                    .and_then(|name| agent_manager::registry::normalize_display_name(name).ok())
                    .flatten();
                v.work_summary = session_work_summary(p, &paths);
                v.project_name = p
                    .root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                v.forked_from = p.forked_from.clone();
                // A staged spawn is waiting on its broker, never an incomplete fork, whatever
                // lineage a hand-edited row carries (`start_refusal` names it the same way).
                v.spawn_staged = p.is_staged_spawn();
                v.job = p.is_job();
                // WHAT THERE IS TO APPLY. Read from the job's own worktree, so a row that committed
                // offers `a` and one that did not does not. A chat row never has either.
                if v.job {
                    let work =
                        super::job_worktree(p).map(|wt| agent_manager::worktree::collect(&wt));
                    v.job_commit = work.as_ref().and_then(|found| found.commit.clone());
                    v.job_branch = work.map(|found| found.branch);
                }
                v.incomplete_fork =
                    !v.spawn_staged && super::forking::incomplete_fork_refusal(p).is_some();
                v.spawned_by = p.spawned_by.clone();
                // The parent's display label as its own row shows it, or the raw id once that
                // row is gone.
                v.spawned_by_label = p.spawned_by.as_deref().map(|parent| {
                    reg.projects
                        .iter()
                        .find(|row| row.id == parent)
                        .and_then(|row| row.display_name.as_deref())
                        .and_then(|name| {
                            agent_manager::registry::normalize_display_name(name)
                                .ok()
                                .flatten()
                        })
                        .unwrap_or_else(|| parent.to_string())
                });
                v.mode = p.mode;
                v.engine = p.engine;
                {
                    let session = session_name(&p.id, &p.root);
                    let session_exists = driver.is_alive(&session).unwrap_or(false);
                    let alive = session_exists && !driver.pane_dead(&session).unwrap_or(false);
                    let attach_intent = chat_lock::is_active(&paths, SystemClock.now());
                    v.human_attached =
                        attach_intent || (alive && driver.has_clients(&session).unwrap_or(true));
                    let live_pane = alive.then_some(session);
                    v.session_live = live_pane.is_some();
                    v.decider_live = v.advice_inflight.as_ref().is_some_and(|inflight| {
                        let supervisor =
                            tmux::supervisor_session_name(&p.id, &p.root, inflight.seq);
                        driver.is_alive(&supervisor).unwrap_or(false)
                            && !driver.pane_dead(&supervisor).unwrap_or(true)
                    });
                    // WORKING vs IDLE — the ● vs ○ split. The turn signal tells an
                    // Autopilot row whether its LAST pmd-nudged turn finished, but the same
                    // persistent terminal can start a later human/self-driven turn without a
                    // new baseline. Therefore a live Busy pane always overrides stale
                    // `Some(false)`. In the other direction the hook remains authoritative:
                    // an outstanding turn (`Some(true)`) is never softened by an
                    // Idle-looking frame, because Claude can draw its bare prompt mid-stream.
                    //
                    // An Idle pane is trusted only after two byte-stable captures. For
                    // Standard/unknown rows this supplies the whole answer; for Autopilot it
                    // confirms idle only when the hook did not already prove a turn is active.
                    // A capture error on an otherwise-live pane stays working rather than
                    // preserving a stale completed-turn/idle value.
                    let hook_working = v.agent_working;
                    v.agent_working = match &live_pane {
                        None => {
                            merge_activity(v.engine, hook_working, ActivityObservation::Missing)
                        }
                        Some(pane) => match driver.capture_tail(pane, ACTIVITY_CAPTURE_LINES) {
                            Err(_) => merge_activity(
                                v.engine,
                                hook_working,
                                ActivityObservation::Unreadable,
                            ),
                            Ok(cap) if tmux::classify_pane(&cap) == tmux::PaneActivity::Busy => {
                                merge_activity(v.engine, hook_working, ActivityObservation::Busy)
                            }
                            Ok(cap) => {
                                let (fp, count) = idle_gate_step(
                                    prev_gate.get(&p.id).copied(),
                                    tmux::idle_fingerprint(&cap),
                                );
                                new_gate.insert(p.id.clone(), (fp, count));
                                merge_activity(
                                    v.engine,
                                    hook_working,
                                    if count < ACTIVITY_IDLE_CONFIRMATIONS {
                                        ActivityObservation::IdleUnconfirmed
                                    } else {
                                        ActivityObservation::IdleConfirmed
                                    },
                                )
                            }
                        },
                    };
                    // A FROZEN LEDGER DOES NOT OUTRANK A WORKING AGENT. On a row pmd does not
                    // drive, the ledger and the needs-you marker are a snapshot from whenever
                    // autopilot last ran: nothing advances them, so a question the human already
                    // answered in the terminal keeps `needs you` on the row — and on the board
                    // column, and in the header's call-to-action count — with an agent visibly
                    // working inside it (user: *"if my session is on autopilot before but now it is
                    // off, when it is working, it still displays under needs you instead of
                    // working"*).
                    //
                    // The pane is the newer evidence and the glyph means WORKING, so a confirmed
                    // working pane takes the posture. `Some(true)` already implies a live pane: a
                    // missing one reads `Some(false)` through `merge_activity`.
                    //
                    // The STOP STAYS in `v.stops`. The question is still unanswered and `s` is
                    // still how a human answers it; what goes is only the claim that the harness is
                    // waiting on them while their agent works.
                    if !agent_manager::daemon::pmd_drives_row(v.mode, v.tier)
                        && v.posture.needs_attention()
                        && v.agent_working == Some(true)
                    {
                        v.posture = Posture::Working;
                        v.next_action = "working".to_string();
                    }
                    // `read_agent_loop` may treat an accepted Monitoring report as a completed
                    // turn when Codex missed its notify hook. A live Busy pane is newer evidence:
                    // the worker may still be finishing the yield or a human may have started a
                    // later turn. Keep the top-line action aligned with that live activity instead
                    // of showing a countdown beside a working glyph.
                    if agent_manager::daemon::pmd_drives_row(v.mode, v.tier)
                        && v.posture == Posture::Monitoring
                    {
                        if v.agent_working == Some(true) {
                            v.next_action = "working".to_string();
                        } else if hook_working == Some(true)
                            && v.agent_working == Some(false)
                            && let Ok(Some(ledger)) = job::load(&paths)
                        {
                            // The file-derived view was built before the live Codex composer
                            // corrected a stale hook. Recompute the hint from the same ledger
                            // with the reconciled activity so `next: working` cannot survive
                            // beside an idle glyph.
                            v.apply_agent_working(&ledger, now, Some(false));
                        }
                    }
                }
                v
            })
            .collect();
        views.sort_by(row_order);
        self.projects = views;
        // The freshly rebuilt working-vs-idle gate replaces the taken one; entries for rows not
        // seen this tick were never re-inserted, so a closed session's gate state is dropped.
        self.idle_gate = new_gate;
        // Follow the id if it is still here; otherwise fall back to clamping the old index, which
        // keeps the cursor near where the removed row was rather than jumping to the top. A missing
        // old id means the selected SESSION changed even if its numeric index did not.
        let next = keep
            .as_ref()
            .and_then(|id| self.projects.iter().position(|view| view.id == *id))
            .unwrap_or_else(|| self.selected.min(self.projects.len().saturating_sub(1)));
        let changed = keep.as_deref() != self.projects.get(next).map(|view| view.id.as_str());
        self.selected = next;
        if changed {
            self.reset_selection_context();
        }
    }

    /// Drain an armed auto-open ([`App::pending_first_chat`]): re-read the armed
    /// session's ledger and route it through [`armed_route`] — **attach-or-stay-armed on
    /// Autopilot, attach-or-chat on Standard**. A live `pmloop-` always attaches (either
    /// tier). On Standard only, a cleanly chattable ledger (a conversation id AND a parked
    /// `Monitoring`/`Idle` run — never `Running`, never `Blocked`) with NO live loop queues
    /// the resume `chat()` and disarms. If it parked `Blocked`, disarm and point the human
    /// at `a` (do NOT yank them into a REPL on a parked-blocked session — a codex
    /// capture-failure `Capability` stop routes here too). Anything else stays armed,
    /// spending one tick of a BOUNDED arm ([`App::armed_drains_left`]). A no-op when not
    /// armed.
    pub(crate) fn drain_armed_first_chat(&mut self) {
        let Some(id) = self.pending_first_chat.clone() else {
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            // The row was removed out from under us — drop the arm.
            self.disarm();
            return;
        };
        let session_paths = entry_state_paths(e);
        let engine = e.engine.unwrap_or(Engine::Claude);
        // The per-session worker model, from the SAME entry `engine` comes from, so an armed
        // pmd-down chat launch carries the set model as `--model`/`-m`.
        let worker_model = e.worker_model.clone();
        // The AUTONOMY DIAL, read through the SAME path `cycle_tier` WRITES
        // (`entry_state_paths(entry).config()`), so the drain obeys the tier the human set
        // and the `JobScheduler` reads. Missing/unreadable ⇒ Standard: that is what
        // `seed_agent_loop` writes by default, and it is the conservative choice (keep
        // today's behaviour rather than promise a hands-off drive we cannot confirm).
        let tier = state::read_json::<Config>(&session_paths.config())
            .map(|c| c.autonomy)
            .unwrap_or(Tier::Standard);
        let Some(ledger) = job::load(&session_paths).ok().flatten() else {
            self.tick_armed_bound(&id);
            return; // no ledger yet — stay armed
        };
        // The ledger's id is the daemon's authority; fall back to the registry seed
        // (harmless — an armed session was never pmtui-seeded, so this is the daemon's
        // adopted/minted id landing in the ledger).
        let captured = read_captured_conversation_id(&session_paths);
        let effective = effective_id(
            ledger.conversation_id.as_deref(),
            e.conversation_id.as_deref(),
            captured.as_deref(),
        );
        let decision = armed_open_decision(effective.as_deref(), &ledger.run);
        // PRIMARY (Slice 1 C1): the arm fired BECAUSE pmd was driving this session, so by
        // now the daemon has usually launched its persistent `pmloop-` claude on this very
        // conversation. Resuming the same cid in a second `pmchat-` REPL would fork the
        // transcript, so probe the loop session the SAME way `request_attach` does and
        // ATTACH it instead of chatting. Gated by `armed_probes_loop` so this shells out to
        // tmux only when the answer can change the route.
        let loop_session = session_name(&id, &e.root);
        let loop_alive = armed_probes_loop(tier, &decision)
            && TmuxDriver::with_socket(&self.socket)
                .is_alive(&loop_session)
                .unwrap_or(false);
        match armed_route(tier, decision, loop_alive) {
            ArmedRoute::AttachLoop => {
                self.disarm();
                self.status = format!("attached {id} — Ctrl+q to detach (it keeps running)");
                self.pending_attach_loop = Some(loop_session);
            }
            // STANDARD ONLY (see `armed_route`): no live `pmloop-` owns this conversation
            // (pmd-down fallback), so the resume `chat()` is safe. `chat()` itself also
            // backstops the collision.
            ArmedRoute::Chat(cid) => {
                self.disarm();
                self.status = format!("{id} is ready — opening its chat");
                // Compute the chat tmux session name while `id`/`e.root` are still
                // borrowable (before `id` moves into `label`).
                let session = session_name(&id, &e.root);
                let turn_signal = session_paths.turn_signal();
                let env = self.managed_env(&id, &e.root);
                self.pending_chat = Some(ChatReq {
                    argv: build_chat(engine, &cid, worker_model.as_deref(), &turn_signal),
                    session_paths,
                    // The REPL's cwd is the PROJECT ROOT (`e.root`), never the
                    // per-session state dir in `session_paths`.
                    root: e.root.clone(),
                    label: id,
                    socket: self.socket.clone(),
                    session,
                    engine,
                    env,
                });
            }
            ArmedRoute::Blocked => {
                self.disarm();
                self.status = format!("{id} has a question — press s to answer it before chatting");
            }
            ArmedRoute::Stay => self.tick_armed_bound(&id), // still waiting — keep the arm
        }
    }

    /// Clear an armed auto-open AND its bound together, so the two can never desync
    /// (a leftover `armed_drains_left` would bound the NEXT arm from the wrong tick).
    pub(crate) fn disarm(&mut self) {
        self.pending_first_chat = None;
        self.armed_drains_left = None;
    }

    /// One "still waiting" drain tick of an armed auto-open. Two independent give-up
    /// paths, checked in this order:
    ///
    /// 1. **pmd is observed DOWN** — for BOTH engines, bounded or not. This closes the
    ///    hole the drain bound alone cannot: `DaemonEnsure::Started` is optimistic by
    ///    construction (`spawn()` succeeded; nothing confirms the child took the
    ///    singleton flock), so a pmd that dies during boot leaves a CODEX arm — which is
    ///    unbounded on purpose, since codex captures its conversation id only after a wake
    ///    COMPLETES — waiting forever behind "autopilot is starting the agent". A liveness
    ///    OBSERVATION is a better instrument than a timeout here: it reports the actual
    ///    cause instead of inferring it from elapsed time, and it cannot false-trigger on
    ///    a merely slow wake.
    /// 2. the existing per-engine drain bound ([`App::armed_drains_left`]), unchanged.
    ///
    /// A no-op for an unbounded arm whose daemon looks fine.
    pub(crate) fn tick_armed_bound(&mut self, id: &str) {
        // Same CACHED sample the status bar renders (one probe path, so the screen and
        // this decision can never disagree), and the streak advances at most once per
        // `DAEMON_PROBE_TTL` — see `armed_wait_decision` for why one sample is not enough.
        let live = self.daemon_live();
        if armed_wait_decision(live, self.daemon_down_streak.get()) == ArmedWait::DisarmDaemonDown {
            self.disarm();
            // SHORT and front-loaded: `keybar_line` reserves at most a third of the bar
            // for a status and truncates the TAIL, so the verdict has to come first.
            self.status = format!("pmd is not running — nothing will drive {id}; Enter retries");
            return;
        }
        let Some(left) = self.armed_drains_left else {
            return; // unbounded (Standard's lease-keyed arm, or codex) — wait forever
        };
        match left.saturating_sub(1) {
            0 => {
                self.disarm();
                self.status =
                    format!("{id}: autopilot could not start the agent — press Enter to retry");
            }
            n => self.armed_drains_left = Some(n),
        }
    }

    fn reset_selection_context(&mut self) {
        self.disarm();
        self.detail_scroll = 0;
        self.detail_max.set(0);
        *self.preview_capture.borrow_mut() = None;
    }

    pub(crate) fn select_project_index(&mut self, next: usize) {
        if self.projects.is_empty() {
            return;
        }
        let next = next.min(self.projects.len() - 1);
        let changed = self.selected_view().map(|view| view.id.as_str())
            != self.projects.get(next).map(|view| view.id.as_str());
        self.selected = next;
        if changed {
            self.reset_selection_context();
        }
    }

    pub(crate) fn selected_view(&self) -> Option<&ProjectView> {
        self.projects.get(self.selected)
    }

    pub(crate) fn move_sel(&mut self, delta: isize) {
        if self.projects.is_empty() {
            return;
        }
        // Any explicit navigation cancels a pending auto-open, even at a list boundary: the human
        // took control of focus and must not be pulled into a session by an older arm.
        self.disarm();
        let n = self.projects.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, n - 1) as usize;
        self.select_project_index(next);
    }
}

pub(crate) fn session_work_summary(entry: &ProjectEntry, paths: &ProjectPaths) -> Option<String> {
    entry
        .task_title
        .clone()
        .or_else(|| {
            std::fs::read_to_string(paths.brief())
                .ok()
                .as_deref()
                .and_then(intent_title)
        })
        .or_else(|| entry.initial_prompt.as_deref().and_then(intent_title))
}

#[cfg(test)]
mod idle_gate_tests {
    use agent_manager::registry::Engine;

    use super::{ACTIVITY_IDLE_CONFIRMATIONS, ActivityObservation, idle_gate_step, merge_activity};

    #[test]
    fn the_idle_gate_confirms_only_a_byte_stable_transcript() {
        // m76. This is the guard that keeps an idle-at-prompt claude reading idle WITHOUT flipping a
        // WORKING one idle mid-stream (m70 streaming-blindness). The rule: a pane that classifies Idle
        // is only trusted as idle after ACTIVITY_IDLE_CONFIRMATIONS byte-stable observations.

        // First Idle observation ARMS the gate (count 1) but does not yet confirm idle.
        let (fp, c) = idle_gate_step(None, 42);
        assert_eq!((fp, c), (42, 1));
        assert!(
            c < ACTIVITY_IDLE_CONFIRMATIONS,
            "one observation is not idle yet ⇒ still running"
        );

        // A SECOND observation with the SAME fingerprint confirms idle.
        let (fp2, c2) = idle_gate_step(Some((42, 1)), 42);
        assert_eq!((fp2, c2), (42, 2));
        assert!(
            c2 >= ACTIVITY_IDLE_CONFIRMATIONS,
            "two stable observations ⇒ confirmed idle"
        );

        // A CHANGED fingerprint — a streaming transcript growing between captures — RESETS the count,
        // so a mid-stream agent never reaches the threshold and never falsely reads idle.
        let (fp3, c3) = idle_gate_step(Some((42, 5)), 99);
        assert_eq!((fp3, c3), (99, 1));
        assert!(
            c3 < ACTIVITY_IDLE_CONFIRMATIONS,
            "a changed transcript re-arms ⇒ stays running"
        );
    }

    #[test]
    fn live_activity_combines_with_the_hook_in_the_fail_safe_direction() {
        for hook in [None, Some(false), Some(true)] {
            assert_eq!(
                merge_activity(Some(Engine::Claude), hook, ActivityObservation::Missing),
                Some(false),
                "no live pane is idle regardless of stale hook state"
            );
            for active in [
                ActivityObservation::Unreadable,
                ActivityObservation::Busy,
                ActivityObservation::IdleUnconfirmed,
            ] {
                assert_eq!(
                    merge_activity(Some(Engine::Claude), hook, active),
                    Some(true),
                    "a live pane without confirmed idleness stays working: {hook:?} {active:?}"
                );
            }
        }
        assert_eq!(
            merge_activity(
                Some(Engine::Claude),
                Some(true),
                ActivityObservation::IdleConfirmed
            ),
            Some(true),
            "an outstanding hook-backed turn cannot be softened by pixels"
        );
        assert_eq!(
            merge_activity(
                Some(Engine::Claude),
                Some(false),
                ActivityObservation::IdleConfirmed
            ),
            Some(false),
            "a completed turn plus confirmed idle pane is idle"
        );
        assert_eq!(
            merge_activity(
                Some(Engine::Claude),
                None,
                ActivityObservation::IdleConfirmed
            ),
            Some(false),
            "without a hook, stable pane content confirms idle"
        );
        assert_eq!(
            merge_activity(
                Some(Engine::Codex),
                Some(true),
                ActivityObservation::IdleConfirmed
            ),
            Some(false),
            "Codex's stable idle composer overrides a stale optional notify byte"
        );
    }
}
