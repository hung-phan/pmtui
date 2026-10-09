//! Enter — the one key with a route per situation. It reads the row's tier, ledger,
//! lease and tmux sessions, hands those facts to the pure routers in `decide.rs`, and
//! queues whatever the answer was: attach the live agent, open a chat, watch a wake,
//! resume a paused row, or arm an auto-open. The registry conversation-id seed lives
//! here because only the create-and-chat arm writes it.

use crate::*;

impl App {
    /// Seed `registry.conversation_id` for `id` (pmtui OWNS the registry). The
    /// daemon's first-wake adopt arm reads this seed and RESUMES it instead of
    /// minting a fresh id, so the conversation pmtui just created via
    /// `claude --session-id` is the one the poll continues. Reloads the registry
    /// from disk (so a concurrent edit isn't clobbered), sets the id, and saves —
    /// pmtui NEVER writes `state.json`.
    pub(crate) fn seed_registry_conversation_id(&mut self, id: &str, cid: &str) -> Result<()> {
        let mut reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(p) = reg.projects.iter_mut().find(|p| p.id == id) else {
            anyhow::bail!("{id} is gone from the list");
        };
        p.conversation_id = Some(cid.to_string());
        reg.save(&self.registry_path)
    }

    /// Enter on the selected `Mode::AgentLoop` row: attach-to-WATCH the daemon-owned
    /// worker pane when a wake is live (its `driver.json` says `Running` AND the
    /// `pmj-…` pane is alive); otherwise point the human at what DOES apply (it wakes on
    /// its cadence; `a` to answer a decision, `d` to close). With autopilot OFF this key
    /// is the ONLY way to reach the agent — pmd is not driving that row — which is
    /// exactly what the `a` refusal points at (see [`App::begin_answer`]).
    pub(crate) fn request_attach(&mut self) {
        // The dead end below was a bare `return`, i.e. a key that looked broken —
        // the single most-repeated complaint about this UI. `cycle_tier` already
        // answers the same situation; this matches it rather than inventing a third shape.
        let Some(v) = self.selected_view() else {
            self.status = "Enter attaches a session (nothing is selected)".into();
            return;
        };
        let id = v.id.clone();
        let mode = v.mode;
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        // A row is on screen but gone from the registry (deleted out-of-band, or the
        // file changed under a view this tick has not re-read). `refresh()` is the
        // load-bearing half: WITHOUT it the stale row survives and every later Enter
        // takes this same branch, so the human presses it forever with nothing
        // changing. Same status + refresh as `cycle_tier`/`remove_project`.
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        // A PAUSED row RESUMES instead of attaching — the user's design: *"When i press enter on
        // pause session, it will resume for me."* Enter is already "the key that gets you into a
        // session", and on a paused one the way in is to start it again first.
        //
        // Placed after the registry lookup because `enabled` lives there.
        if !e.enabled {
            let root = e.root.clone();
            self.resume_session(&id, &root);
            return;
        }
        {
            // A fresh Enter SUPERSEDES any prior arm: clear it up front so a stale
            // `pending_first_chat` from an earlier Enter cannot re-fire after this
            // action (e.g. Enter arms X, then a later Enter create-and-chats X — the
            // post-exit idle drain must find NO arm, not re-open the REPL the human
            // just left). Only the `Arm` branches below re-set it.
            self.disarm();
            // Extract every input as an OWNED value so the `reg`/`e` borrow can be
            // dropped before any `&mut self` registry write below (create-and-chat
            // seeds `registry.conversation_id`).
            let session_paths = entry_state_paths(e);
            let engine = e.engine.unwrap_or(Engine::Claude);
            // The per-session worker model, read from the SAME entry `engine` comes from, so a
            // human-driven launch (resume-of-ghost create, or resume) carries the set model as
            // `--model`/`-m`. Owned like the other inputs so the `reg`/`e` borrow can drop.
            let worker_model = e.worker_model.clone();
            let initial_prompt = self.initial_message_retries.get(&id).cloned();
            let registry_cid = e.conversation_id.clone();
            let cadence_s = e.cadence_s;
            // The AUTONOMY DIAL — the input this key was missing. Read through the SAME
            // path `cycle_tier` WRITES (`entry_state_paths(entry).config()`), so Enter acts
            // on the tier the human actually set and the `JobScheduler` actually reads.
            // Missing/unreadable ⇒ Standard: that is what `seed_agent_loop` writes by
            // default, and it is the conservative fallback (a config pmtui cannot read is
            // one pmd cannot read either, so nothing is driving that row anyway — keep the
            // chattable route rather than arm behind a drive that will not happen).
            let tier = state::read_json::<Config>(&session_paths.config())
                .map(|c| c.autonomy)
                .unwrap_or(Tier::Standard);
            // The REPL's cwd is the PROJECT ROOT (`e.root`) — the path the user
            // entered on create — NOT `session_paths` (the per-session state dir).
            // Moved into whichever launcher this Enter queues (chat or create-chat).
            let root = e.root.clone();
            // The worker pane is the daemon's `pmj-…` session recorded in the
            // per-session `driver.json`. `watch_pane` is the pure selection step
            // (Running ⇒ the pane); the liveness probe is gated separately because
            // it shells out to tmux (the daemon may have reaped a finished wake).
            // `live_pane` is `Some` only when the recorded pane is genuinely alive.
            let driver = state::read_json_opt::<DriverState>(&session_paths.driver())
                .ok()
                .flatten();
            let live_pane = watch_pane(driver.as_ref())
                .filter(|pane| self.agent_tmux.is_alive(pane).unwrap_or(false));
            // The ledger carries the conversation id (set on the first wake) and the
            // run state — a `Running` wake is in flight, so a second resume of the
            // same id would collide; don't offer the chat then. `effective_id` keys
            // Enter on the ledger's id (the daemon's authority) OR, failing that, the
            // registry seed — so a session pmtui already seeded resumes that same id
            // on a second Enter rather than minting a second conversation.
            let ledger = job::load(&session_paths).ok().flatten();
            let ledger_cid = ledger.as_ref().and_then(|l| l.conversation_id.clone());
            let run_is_running = ledger
                .as_ref()
                .map(|l| matches!(l.run, job::JobRun::Running { .. }))
                .unwrap_or(false);
            // THIRD source, last in priority: the id codex's own turn hook reported. Codex has
            // no caller-chosen id, so neither the ledger nor the registry can hold one until a
            // wake adopts it — which is why a long-running codex session used to read as
            // never-woken and get a SECOND conversation on Enter.
            let captured = read_captured_conversation_id(&session_paths);
            let effective = effective_id(
                ledger_cid.as_deref(),
                registry_cid.as_deref(),
                captured.as_deref(),
            );
            // A live unified terminal always wins over ledger routing. Enter only
            // attaches; tmux client detection makes pmd defer while the human is there.
            let loop_session = session_name(&id, &root);
            let loop_alive = self.agent_tmux.is_alive(&loop_session).unwrap_or(false);
            if loop_alive {
                self.status = format!("attached {id} — Ctrl+q to detach (it keeps running)");
                self.pending_attach_loop = Some(loop_session); // drained in run()
                return; // do NOT fall through to the chat/watch/first-wake routing
            }
            // The terminal is absent but an active pmd owns this Autopilot row.
            // Let that owner relaunch it rather than racing a second launch here.
            if effective.is_some()
                && agent_manager::daemon::pmd_drives_row(mode, Some(tier))
                && self.daemon_live() == DaemonLive::Up
            {
                self.status =
                    format!("{id}: pmd is starting the agent — press Enter again in a moment");
                return;
            }
            match agent_loop_enter(live_pane.as_deref(), effective.as_deref(), run_is_running) {
                EnterAction::Watch(_pane) => {
                    // No status set: the next `render` short-circuits to
                    // `render_wake_view` (which ignores `app.status`), and exit
                    // overwrites it with "left wake view".
                    self.mode = UiMode::WakeView {
                        id: id.clone(),
                        paths: session_paths,
                        scroll: 0,
                    };
                }
                EnterAction::Chat(cid) => {
                    // A GHOST id (recorded in the ledger/registry but with no transcript on
                    // disk) cannot be `--resume`d — that opens claude into a dead/empty session
                    // (the same failure the paused-resume path had). On a FRESH launch, CREATE
                    // it with `--session-id` instead, mirroring the daemon's seed-probe and
                    // `start_undriven_session`. The terminal was confirmed absent above.
                    // The probe returns `None` for codex, so codex never reaches the create arm.
                    let retrying_initial_launch = initial_prompt.is_some();
                    let create_ghost = retrying_initial_launch
                        || job_engine::claude_conversation_exists(
                            self.claude_home.as_deref(),
                            &root,
                            engine,
                            &cid,
                        ) == Some(false);
                    let turn_signal = session_paths.turn_signal();
                    let argv = if create_ghost {
                        build_chat_create(engine, &cid, worker_model.as_deref(), &turn_signal)
                    } else {
                        build_chat(engine, &cid, worker_model.as_deref(), &turn_signal)
                    };
                    let argv = with_initial_prompt(argv, initial_prompt.as_deref());
                    let env = self.managed_env(&id, &root);
                    self.pending_chat = Some(ChatReq {
                        argv,
                        session_paths,
                        root,
                        label: id.clone(),
                        socket: self.socket.clone(),
                        session: loop_session,
                        engine,
                        env,
                    });
                    // The SESSION's name, not the conversation UUID: `{cid}` printed 36 hex characters the
                    // human has never seen and cannot act on. Both exits are named because they do
                    // different things — Ctrl+q leaves the chat running, /exit ends it.
                    self.status =
                        format!("chatting with {id} — Ctrl+q returns; /exit ends the chat");
                }
                EnterAction::WaitingFirstWake => {
                    // No conversation exists yet — so WHO should create it? That is a
                    // question about intent, and intent is the TIER. Route on it FIRST,
                    // before any lease probe (see `first_enter_route` for the deadlock this
                    // removes).
                    if first_enter_route(tier) == FirstEnterRoute::EnsureDaemonThenArm {
                        // AUTOPILOT: pmd owns this conversation. Ensure a daemon, and arm
                        // ONLY if one is actually up — the ensure is a PRECONDITION of the
                        // arm, not a race with it. No lease probe, no `mint_uuid_v4`, no
                        // `seed_registry_conversation_id`, no launcher: pmd's
                        // `ensure_session` performs the whole conversation-id + `pmloop-`
                        // launch + ledger save (ledger writes are the daemon's alone), and
                        // the idle drain attaches what comes up. No launcher is ADDED here;
                        // both fresh-`pmchat-` launch sites (this Enter's `CreateAndChat`
                        // and the arm's old chat fallback) become unreachable from the
                        // autopilot route.
                        match autopilot_first_enter(&id, &self.ensure_daemon()) {
                            AutopilotFirstEnter::Arm(status) => {
                                self.pending_first_chat = Some(id.clone());
                                // Bound the wait for claude, whose id lands in one sweep
                                // (`ensure_session` pins it and parks `Monitoring` in ONE
                                // save). codex captures its id only after a wake COMPLETES,
                                // so it stays unbounded — second-class and unregressed.
                                self.armed_drains_left = match engine {
                                    Engine::Claude => Some(AUTOPILOT_ARM_DRAINS),
                                    Engine::Codex => None,
                                };
                                // The daemon-DOWN give-up (`armed_wait_decision`) counts
                                // CONSECUTIVE down samples, so the count has to start HERE:
                                // the ensure just ran, and a streak carried over from before
                                // it would disarm on the next drain in front of a pmd that is
                                // booting normally.
                                self.restart_daemon_watch();
                                self.status = status;
                            }
                            AutopilotFirstEnter::Refuse(status) => self.status = status,
                        }
                        return;
                    }
                    // STANDARD, unchanged: the per-session `driver.lock` is the fork
                    // fence: a claude session may CREATE-and-chat right now ONLY while
                    // pmtui exclusively holds the lock (pmd is not driving). If pmd holds
                    // it — or this is codex (which never caller-mints an id) — arm an
                    // auto-open instead. This lock path MUST equal the daemon's
                    // (`entry_paths(p).daemon_dir().join("driver.lock")`).
                    let lock_path = session_paths.daemon_dir().join("driver.lock");
                    match lease::try_acquire(&lock_path) {
                        Ok(lease_opt) => {
                            let lease_free = lease_opt.is_some();
                            match first_wake_action(engine, lease_free, &id) {
                                FirstWakeAction::CreateAndChat => {
                                    // claude + free lease: mint the id, seed the
                                    // registry (the daemon adopts it), and queue the
                                    // create REPL holding the lease continuously.
                                    let lease = lease_opt.expect(
                                        "CreateAndChat is only chosen when the lease is free",
                                    );
                                    let u = job_engine::mint_uuid_v4();
                                    match self.seed_registry_conversation_id(&id, &u) {
                                        Ok(()) => {
                                            self.status = format!(
                                                "creating {id} — a live REPL opens now (Ctrl+q to return); the poll resumes it"
                                            );
                                            // Compute the chat tmux session name
                                            // before `root` moves into the request.
                                            let chat_session = session_name(&id, &root);
                                            let turn_signal = session_paths.turn_signal();
                                            let env = self.managed_env(&id, &root);
                                            self.pending_create_chat = Some(CreateChatReq {
                                                // `engine` is guaranteed `Claude` here
                                                // (first_wake_action returns
                                                // CreateAndChat only for (Claude, free));
                                                // passing it — not a hardcoded Claude —
                                                // keeps build_chat_create's codex
                                                // `unreachable!` guard live.
                                                argv: with_initial_prompt(
                                                    build_chat_create(
                                                        engine,
                                                        &u,
                                                        worker_model.as_deref(),
                                                        &turn_signal,
                                                    ),
                                                    initial_prompt.as_deref(),
                                                ),
                                                session_paths,
                                                root,
                                                label: id.clone(),
                                                lease,
                                                socket: self.socket.clone(),
                                                session: chat_session,
                                                engine,
                                                env,
                                            });
                                        }
                                        Err(err) => {
                                            // Registry seed failed → do NOT launch;
                                            // dropping `lease` lets pmd take over.
                                            self.status = format!(
                                                "{id}: could not seed the conversation: {err}"
                                            );
                                        }
                                    }
                                }
                                FirstWakeAction::Arm(status) => {
                                    // pmd holds the lease (or codex + pmd): arm the
                                    // auto-open — no mint, no registry write. Any held
                                    // `lease_opt` here is `None` (the daemon holds it).
                                    self.pending_first_chat = Some(id.clone());
                                    self.status = status;
                                }
                                FirstWakeAction::CreateChatNoSeed(status) => {
                                    // codex + free lease: pmtui opens a FRESH codex REPL now (codex
                                    // has no caller-chosen id → nothing to mint/seed). Standard ⇒ pmd
                                    // won't drive this row, so drop the lease; nothing races.
                                    drop(lease_opt);
                                    let chat_session = session_name(&id, &root);
                                    let turn_signal = session_paths.turn_signal();
                                    let env = self.managed_env(&id, &root);
                                    self.pending_chat = Some(ChatReq {
                                        argv: with_initial_prompt(
                                            build_codex_fresh_chat(
                                                worker_model.as_deref(),
                                                &turn_signal,
                                            ),
                                            initial_prompt.as_deref(),
                                        ),
                                        session_paths,
                                        root,
                                        label: id.clone(),
                                        socket: self.socket.clone(),
                                        session: chat_session,
                                        engine,
                                        env,
                                    });
                                    self.status = status;
                                }
                            }
                        }
                        Err(err) => {
                            self.status = format!(
                                "{id}: something else may be starting it — try again ({err})"
                            );
                        }
                    }
                }
                EnterAction::NoWake => self.status = no_wake_status(&id, cadence_s),
            }
        }
    }
}
