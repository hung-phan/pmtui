//! `n` — turning the create form into a session that exists: validate the form, seed
//! the per-session state on disk, register the row, and start a daemon when (and only
//! when) the Autonomy dial says Autopilot.

use crate::*;

pub(crate) fn project_id_base(dir: &Path) -> String {
    match dir.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => "session".into(),
    }
}

impl App {
    pub(crate) fn begin_create(&mut self) {
        self.return_to_board_after_create = false;
        self.open_create_form(CreateForm::new());
    }

    pub(crate) fn begin_task_create(&mut self) {
        let selected = self.selected_view().map(|view| view.id.clone());
        let entry = Registry::load(&self.registry_path)
            .ok()
            .and_then(|registry| {
                registry
                    .projects
                    .into_iter()
                    .find(|entry| Some(entry.id.as_str()) == selected.as_deref())
            });
        let mut form = CreateForm::new();
        form.task_mode = true;
        if let Some(entry) = entry {
            form.engine = entry.engine.unwrap_or(Engine::Claude);
            form.worker_model = entry.worker_model;
            form.dir = Field::from(entry.root.display().to_string());
        }
        self.return_to_board_after_create = true;
        self.open_create_form(form);
    }

    fn open_create_form(&mut self, mut form: CreateForm) {
        // Fill the Worker Model catalog NOW, not at render time: the render and the Worker Model
        // toggle read `form.model_choices` directly, and `models_for` does the one-per-engine
        // discovery I/O (cached). The engine-toggle path in `handle_create_key` refreshes it.
        form.model_choices = self.models_for(form.engine).to_vec();
        // The Decider Model catalog is per-DECIDER-engine; fill it the same way, so the render and
        // the Decider Model toggle read `form.decider_model_choices` with no render-time discovery
        // I/O. The decider-engine-toggle path in `handle_create_key` refreshes it.
        form.decider_model_choices = self.models_for(form.decider_engine).to_vec();
        self.mode = UiMode::Creating(form);
    }

    pub(crate) fn cancel_create_form(&mut self) {
        self.mode = if self.return_to_board_after_create {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.return_to_board_after_create = false;
    }

    /// Turn the create form into a new `Mode::AgentLoop` session: seed its
    /// per-session config/brief/ledger and register it. A SINGLE path — every
    /// session is the same self-driving loop; only the Autonomy dial (`form.tier`,
    /// the tier the engine reads) differs.
    pub(crate) fn submit_create(&mut self) {
        let form = match &self.mode {
            UiMode::Creating(f) => f.clone(),
            _ => return,
        };
        // AUTOPILOT needs a direction; Standard does not. The goal lands in `brief.md`,
        // which the harness-owned wake prompt injects verbatim each heartbeat — so a
        // hands-off session with no goal is a nudge with nothing to steer by, while a
        // collaborative one is legitimately steered by the human at the keyboard.
        // Checked BEFORE tearing the form down, so a miss shows a status and keeps the
        // form open for correction without writing anything. Typed stop kind/effect policy
        // is enforced independently at runtime, not here.)
        if form.goal.trim().is_empty() && form.tier == Tier::Autopilot {
            self.status = "autopilot needs a goal to steer by (Standard doesn't)".into();
            return;
        }
        if let Err(error) = agent_manager::registry::normalize_display_name(form.name.as_str()) {
            self.status = error.into();
            return;
        }
        let raw = form.dir.trim().to_string();
        if raw.is_empty() {
            // Keep the form OPEN (like the goal miss above) so the human can type a directory,
            // rather than dropping them back to the dashboard having lost the whole form.
            self.status = "create needs a directory".into();
            return;
        }
        // Resolve to an absolute, symlink-free path (so tmux resolves it correctly on the shared
        // socket, and no relative root is stored). `canonicalize` requires the path to EXIST —
        // which used to be a hard rejection of a typo. Now a MISSING path is offered for creation
        // (user: *"if the folder is not defined in the popup to allow user to create the folder
        // too."*): confirm first, so a genuine typo is still caught but a real new folder is one
        // keystroke away. Any OTHER error (a path component that is a file, permissions) still
        // rejects, but keeps the form open so the path can be fixed.
        match std::fs::canonicalize(&raw) {
            Ok(dir) => self.create_at(form, dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // `create_at` measures the exact launch only once the folder exists. A Message
                // no folder can launch is refused first, so no folder is made for it.
                if let Some(bytes) = initial_message_floor_bytes(&form)
                    && bytes > tmux::LAUNCH_COMMAND_MAX_BYTES
                {
                    let over = bytes - tmux::LAUNCH_COMMAND_MAX_BYTES;
                    let unit = byte_unit(over);
                    self.status = format!(
                        "initial message too long — shorten it by at least {over} {unit} (it is {bytes} bytes after shell quoting; tmux accepts {} for the whole launch command)",
                        tmux::LAUNCH_COMMAND_MAX_BYTES
                    );
                    return;
                }
                self.mode = UiMode::ConfirmCreateDir { form, dir: raw };
            }
            Err(e) => self.status = format!("directory {raw:?} not usable: {e}"),
        }
    }

    /// `y`/Enter on the "create this directory?" prompt: make the tree the human approved, then
    /// finish the create exactly as an existing directory would. On a `mkdir`/resolve failure it
    /// returns to the create form (not a dead end) so the path can be corrected.
    pub(crate) fn confirm_create_dir(&mut self) {
        let (form, raw) = match &self.mode {
            UiMode::ConfirmCreateDir { form, dir } => (form.clone(), dir.clone()),
            _ => return,
        };
        if let Err(e) = std::fs::create_dir_all(&raw) {
            self.mode = UiMode::Creating(form);
            self.status = format!("could not create {raw:?}: {e}");
            return;
        }
        // Resolve the freshly-created tree the SAME way the existing-path arm does, so both
        // routes into `finish_create` hand it an absolute, symlink-free root.
        match std::fs::canonicalize(&raw) {
            Ok(dir) => self.create_at(form, dir),
            Err(e) => {
                self.mode = UiMode::Creating(form);
                self.status = format!("created {raw:?} but could not resolve it: {e}");
            }
        }
    }

    /// Create the session in the resolved `dir`, unless its initial Message cannot launch there.
    /// Checked here, after the directory resolves, because the launch command embeds the
    /// session's state paths; a refusal keeps the whole form open for the human to shorten it.
    fn create_at(&mut self, form: CreateForm, dir: PathBuf) {
        let registry = Registry::load(&self.registry_path).unwrap_or_default();
        if let Some(bytes) = initial_launch_bytes(&form, &dir, &registry)
            && bytes > tmux::LAUNCH_COMMAND_MAX_BYTES
        {
            let over = bytes - tmux::LAUNCH_COMMAND_MAX_BYTES;
            let unit = byte_unit(over);
            self.mode = UiMode::Creating(form);
            self.status = format!(
                "initial message too long — shorten it by {over} {unit} (its launch command is {bytes} bytes after shell quoting; tmux accepts {})",
                tmux::LAUNCH_COMMAND_MAX_BYTES
            );
            return;
        }
        self.mode = UiMode::Normal;
        let retry = form.clone();
        let created = self.finish_create(form, dir);
        self.finish_create_destination(created, retry);
    }

    /// Any non-confirm key on the "create this directory?" prompt: return to the create form with
    /// every field intact, so a mistyped path is one edit from fixed rather than a full re-entry.
    pub(crate) fn cancel_create_dir(&mut self) {
        if let UiMode::ConfirmCreateDir { form, .. } = &self.mode {
            let form = form.clone();
            self.mode = UiMode::Creating(form);
            self.status = "directory not created — fix the path, or esc to cancel".into();
        }
    }

    /// Turn a validated create form + a resolved (existing) directory into a live session: seed
    /// its per-session state, register the row, and start a daemon iff the Autonomy dial says
    /// Autopilot. Reached from `submit_create` (the directory already existed) and from
    /// `confirm_create_dir` (the human just created it) — ONE finish path, so a session made
    /// either way is identical.
    fn finish_create_destination(&mut self, created: bool, retry: CreateForm) {
        if self.return_to_board_after_create {
            if created {
                self.mode = UiMode::Board;
                self.board_detail_open = true;
                self.return_to_board_after_create = false;
            } else {
                self.mode = UiMode::Creating(retry);
            }
        } else {
            self.return_to_board_after_create = false;
        }
    }

    fn finish_create(&mut self, form: CreateForm, dir: PathBuf) -> bool {
        // Corrupt-safe read: an ABSENT registry is fine (a first session is legitimate), but a
        // PRESENT-but-unreadable one must NOT proceed — seeding the session and then saving a
        // default-empty registry would erase every other project. Refuse and say so.
        let reg = match Registry::load(&self.registry_path) {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("could not read the session list ({e}) — not overwriting it");
                return false;
            }
        };

        // One-tree invariant: agent-loop sessions stack freely in a folder — each keeps
        // its state under its own `sessions/<id>/` subtree with a per-session lease — so
        // there is no conflicting session-kind to refuse here.
        let display_name = match agent_manager::registry::normalize_display_name(form.name.as_str())
        {
            Ok(name) => name,
            Err(error) => {
                self.status = error.into();
                return false;
            }
        };

        // The Autonomy dial the form carries at submit IS the seeded tier — the one
        // control the engine reads (`policy::decide_kind`).
        let tier = form.tier;
        // This path only ever creates an agent-loop session. Naming the mode once lets
        // the registry entry and the daemon ensure below agree by construction, and lets
        // that ensure route through the same `pmd_drives` predicate `cycle_tier` uses
        // instead of hardcoding the answer.
        let mode = Mode::AgentLoop;

        // Seed PER-SESSION state under `sessions/<id>/` (config + brief + a fresh
        // Idle ledger), then register a Mode::AgentLoop entry so pmd's JobScheduler
        // re-invokes it on its heartbeat. The scheduler treats a missing ledger as
        // WaitingForIntake, so the session must exist on disk first — this create is
        // what bootstraps it. conversation_id stays None: the first wake pins (claude)
        // or captures (codex) it.
        let now = SystemClock.now();
        let initial_prompt = (tier == Tier::Standard)
            .then(|| form.goal.trim().to_string())
            .filter(|message| !message.is_empty());
        let brief = if tier == Tier::Autopilot {
            form.goal.trim()
        } else {
            ""
        };
        // UNSET on Standard, because the form did not ask (`shows_cadence`) and a number nobody
        // chose should not look like one they did. `m`'s prompt asks for it when autopilot turns
        // the heartbeat on; until then `job_engine`'s own default applies.
        let cadence = form.shows_cadence().then_some(form.cadence_s);
        let setup = reserve_new_session(
            &reg,
            &dir,
            NewSession {
                tier,
                engine: form.engine,
                decider_engine: form.decider_engine,
                decider_model: form.decider_model.clone(),
                brief,
                cadence_s: cadence,
            },
            now,
        );
        let id = match setup {
            Ok(setup) => setup,
            Err(error) => {
                self.status = format!("could not set up the session: {error}");
                return false;
            }
        };
        let entry = ProjectEntry {
            id: id.clone(),
            display_name,
            root: dir.clone(),
            enabled: true,
            mode,
            engine: Some(form.engine),
            worker_model: form.worker_model.clone(),
            initial_prompt: initial_prompt.clone(),
            task_title: form
                .task_mode
                .then(|| intent_title(form.goal.as_str()))
                .flatten(),
            forked_from: None,
            spawned_by: None,
            launch: None,
            conversation_id: None,
            cadence_s: cadence,
        };
        // Captured before the entry moves into the registry write below, so the Standard
        // launch (`start_standard_now`) can thread it. Read from the entry — the SAME source
        // `engine` comes from — so the launch model always matches what was seeded (the create
        // form's Worker Model field now sets it; `None` = the engine default).
        let worker_model = entry.worker_model.clone();
        // Append through `Registry::update`, which re-reads corrupt-safe and refuses to write on an
        // unreadable file — so the push can never turn a registry that went bad between the read
        // above and now into an empty list. (Single-threaded pmtui: the re-read matches the read
        // that computed `id`, so it stays unique.)
        if let Err(e) = Registry::update(&self.registry_path, |r| r.projects.push(entry)) {
            self.status = format!("could not save the session list: {e}");
            return false;
        }
        // No attach: agent-loop sessions run on their heartbeat, not human-attached.
        // Names the cadence only when there IS one: "created x (standard, 300s)" claimed a
        // heartbeat that neither exists nor is recorded.
        let created = match cadence {
            Some(secs) => format!(
                "created {id} ({}, every {})",
                tier_name(tier),
                job_engine::human_cadence(secs)
            ),
            None => format!("created {id} ({})", tier_name(tier)),
        };
        // pmd is the ONLY thing that runs that heartbeat, so a create that does not
        // ensure a daemon hands back a session nothing drives — which is why "autopilot
        // at create" used to do nothing until pmtui was restarted while "autopilot via
        // `m`" worked. Same ensure `cycle_tier` does, so both routes to autopilot behave
        // identically.
        //
        // Gated on the mode AND the tier. This REVERSES the call m12 made here, and the
        // reversal is the point: m12 gated on `pmd_drives(mode)` alone, reasoning that
        // "`config.autonomy` is read in the whole engine at exactly ONE site
        // (`policy::decide_kind`) while the sweep's only skip is `Mode::Interactive`, so a
        // STANDARD agent-loop row is exactly as undriven without a daemon as an Autopilot
        // one — a tier gate would leave the identical bug for Standard." That reasoning
        // was true then and is FALSE now: m15's `daemon::pmd_drives_row` makes a Standard
        // agent-loop row undriven BY DESIGN, so starting a daemon for one would be a
        // daemon nothing asked for, contradicting the copy that just told the human they
        // drive this session themselves. Autopilot remains the one thing that means "let
        // pmd drive this", and it is still the ONLY thing that starts a daemon here —
        // matching `cycle_tier` (which only ensures on the landing-on-Autopilot arm) and
        // `ensure_daemon_for_enabled_autopilot` (which filters on Autopilot).
        //
        // `pmd_drives(mode)` is kept as the second half rather than dropped: it is always
        // true on this path today (every create is `Mode::AgentLoop`), and routing through
        // it keeps the rule right if another mode ever becomes creatable here.
        //
        // The daemon fragment goes FIRST, and the engine label is gone, because
        // `keybar_line` caps a transient status at a THIRD of the bar and `truncate`s the
        // tail: at ~120 columns that budget is ~39, so the old
        // "created loop <id> (claude, autopilot, cadence 300s); daemon started" lost the
        // daemon clause exactly when the human most needed it — the whole complaint here
        // was not being able to tell whether anything was running. Order by information
        // value instead: "created <id>" is the predictable half (they just pressed Enter
        // on a create form, and the row shows engine/tier), the daemon state is the half
        // they cannot otherwise see. A persistent `pmd up`/`pmd DOWN` indicator is the
        // durable answer and is a separate slice; this only stops the transient message
        // from lying by omission.
        //
        // The OFF branch says who drives instead, for the same reason: nothing else on
        // screen tells a human that the row they just made is theirs to run.
        self.status = if pmd_drives(mode) && tier == Tier::Autopilot {
            format!("{}; {created}", self.ensure_daemon())
        } else {
            // STANDARD: start the agent NOW rather than at the first Enter. User: *"why do i need
            // to press enter for it to render the claude/codex? why can it just runs the session"*.
            // Every arm keeps "you drive it (Enter)": on Standard that is the standing promise —
            // pmd does not drive this row and Enter is the way in — and it stays true whether or not
            // the agent came up. What CHANGES is the state in front of it.
            let outcome = self.start_standard_now(
                &id,
                &dir,
                form.engine,
                worker_model.as_deref(),
                initial_prompt.as_deref(),
            );
            self.standard_start_status(&id, &created, initial_prompt.clone(), outcome)
        };
        self.refresh();
        if let Some(index) = self.projects.iter().position(|view| view.id == id) {
            // Creation should reveal the session it just launched, not preserve an unrelated row
            // or that row's scrollback offset in the main preview.
            self.select_project_index(index);
        }
        true
    }

    pub(crate) fn standard_start_status(
        &mut self,
        id: &str,
        created: &str,
        initial_prompt: Option<String>,
        outcome: StandardStart,
    ) -> String {
        match outcome {
            StandardStart::Running => {
                self.initial_message_retries.remove(id);
                format!("{created}; running — you drive it (Enter)")
            }
            StandardStart::RunningWithWarning(why) => {
                self.initial_message_retries.remove(id);
                format!("{created}; running — you drive it (Enter); {why}")
            }
            StandardStart::NotStarted(why) => {
                if let Some(prompt) = initial_prompt {
                    self.initial_message_retries.insert(id.to_string(), prompt);
                }
                format!("{created}; you drive it (Enter) — not started: {why}")
            }
        }
    }

    /// Launch a brand-new STANDARD session's agent immediately, detached, so the row the human
    /// just made is a row that is RUNNING.
    ///
    /// This is exactly what Enter's create-and-chat arm has always done — mint the conversation id,
    /// seed it into the registry so pmd adopts it if autopilot is turned on later, and launch
    /// `claude --session-id <u>` in the session's own `pmchat-` tmux session — minus the attach.
    /// Enter then RE-attaches it (`request_attach` routes a live chat session to
    /// [`EnterAction::Chat`]), and `s` types into it (`App::resolve_send_pane`).
    ///
    /// Deliberately the CHAT posture (`build_chat_create`), not the daemon's
    /// [`agent_manager::worker::build_loop_command`]. The daemon's form adds
    /// `--permission-mode auto` because nobody is watching it; a Standard session is one a human
    /// drives, and launching it unattended would silently stop it asking before it writes — a
    /// safety change nobody asked for. Keeping the two postures on two tmux NAMES is also what
    /// lets the existing chat interlock arbitrate if autopilot is turned on while this is still up.
    ///
    /// Ordering: LAUNCH, then seed. A seed written for a session that failed to launch is the
    /// poison case — pmd would later `--resume` a conversation that was never created, and the
    /// de-poison self-heal was dropped in the persistent-session pivot — so the seed is only
    /// written once there is a live process to justify it.
    fn start_standard_now(
        &mut self,
        id: &str,
        root: &Path,
        engine: Engine,
        model: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> StandardStart {
        // CREATE mints; see `start_undriven_session` for the resume case. `model` comes from the
        // freshly-created registry entry — what the create form's Worker Model field set (`None`
        // for the engine default) — so a Standard session launches on the chosen model.
        self.start_undriven_session(id, root, engine, None, model, initial_prompt)
    }

    /// Bring a row's session back up after `r` or Enter-after-pause, when NOTHING ELSE WILL.
    ///
    /// Returns a status fragment to append, or `None` when the launch is somebody else's job — which
    /// is the interesting half: on a DRIVEN row pmd relaunches within a sweep (~0.5s measured), so
    /// starting it here as well would race the daemon for the same conversation. On an UNDRIVEN row
    /// nobody is coming, and that was the bug: both keys ended with a dead agent and an empty pane.
    ///
    /// A session that is somehow still ALIVE is left alone rather than re-launched: `r` terminates
    /// before calling this, so a live session here means something else just started one, and killing
    /// or duplicating it would be worse than doing nothing.
    pub(crate) fn start_after_lifecycle_key(
        &mut self,
        id: &str,
        root: &Path,
        engine: Engine,
        mode: Mode,
        model: Option<&str>,
    ) -> Option<String> {
        let paths = ProjectPaths::for_session(root, id);
        let tier = state::read_json::<Config>(&paths.config())
            .map(|c| c.autonomy)
            .unwrap_or(Tier::Standard);
        if agent_manager::daemon::pmd_drives_row(mode, Some(tier)) {
            return None; // pmd's job, and it is already on its way
        }
        if self
            .agent_tmux
            .is_alive(&session_name(id, root))
            .unwrap_or(false)
        {
            return None;
        }
        // The conversation to resume: the LEDGER first (the daemon's authority), then the registry
        // seed. Only a session with neither gets a fresh one.
        let existing = job::load(&paths)
            .ok()
            .flatten()
            .and_then(|l| l.conversation_id)
            .or_else(|| {
                Registry::load(&self.registry_path).ok().and_then(|r| {
                    r.projects
                        .iter()
                        .find(|p| p.id == id)?
                        .conversation_id
                        .clone()
                })
            });
        Some(
            match self.start_undriven_session(id, root, engine, existing, model, None) {
                StandardStart::Running => {
                    self.initial_message_retries.remove(id);
                    " and started it (Enter attaches)".to_string()
                }
                StandardStart::RunningWithWarning(why) => {
                    self.initial_message_retries.remove(id);
                    format!(" and started it, but {why}")
                }
                StandardStart::NotStarted(why) => format!(" but could not start it: {why}"),
            },
        )
    }

    /// Start the interactive session of a row NOTHING WILL DRIVE, on `existing`'s conversation when
    /// there is one and a freshly minted id when there is not.
    ///
    /// The `existing` split is the whole reason this is one function: `--session-id` CREATES a
    /// conversation and `--resume` CONTINUES one, so a restart that minted would silently abandon the
    /// conversation the human has been working in, and a create that resumed would fail on an id no
    /// engine has ever seen.
    ///
    /// WHY pmtui launches at all, rather than leaving it to the daemon: `pmd_drives_row` is false for
    /// a Standard agent-loop row by design (m15), so on those rows pmd will never launch anything. `r`
    /// and Enter-after-pause both used to hand the relaunch to pmd and return, which on a Standard row
    /// meant the agent stayed dead and the pane stayed empty — user: *"when i press r to restart, or
    /// enter after press p to pause. The main panel doesn't render the session immediate … it should
    /// start the session and render it immediately"*. On a DRIVEN row pmd does launch it (measured at
    /// ~0.5s after the flip), so these callers only reach here when nothing else will.
    pub(crate) fn start_undriven_session(
        &mut self,
        id: &str,
        root: &Path,
        engine: Engine,
        existing: Option<String>,
        model: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> StandardStart {
        let paths = ProjectPaths::for_session(root, id);
        // The SAME fork fence Enter uses: mint only while holding the per-session `driver.lock`,
        // so pmtui and pmd can never mint two ids for one session.
        let lease = match lease::try_acquire(&paths.daemon_dir().join("driver.lock")) {
            Ok(Some(l)) => l,
            Ok(None) => {
                return StandardStart::NotStarted("pmd is already starting this session".into());
            }
            Err(e) => {
                return StandardStart::NotStarted(format!("could not claim the session: {e}"));
            }
        };
        let session = session_name(id, root);
        let env = self.managed_env(id, root);
        crate::session::install_spawn_skill(root, engine, &env);
        if engine == Engine::Codex && existing.is_none() {
            let argv = with_initial_prompt(
                build_codex_fresh_chat(model, &paths.turn_signal()),
                initial_prompt,
            );
            let launched = self
                .agent_tmux
                .launch_interactive(&session, root, &argv, &env);
            drop(lease);
            return match launched {
                Ok(_) => StandardStart::Running,
                Err(e) => StandardStart::NotStarted(e.to_string()),
            };
        }
        let minted = existing.is_none();
        let cid = existing.unwrap_or_else(job_engine::mint_uuid_v4);
        // RESUME only a conversation claude can actually load. A recorded id whose transcript
        // is GONE (`Some(false)`) is a GHOST: `--resume <ghost>` opens claude into a dead/empty
        // session, so pressing Enter to resume a paused Standard row that pointed at one did
        // nothing. Mirror the daemon's seed-probe and CREATE that id with `--session-id`
        // instead — re-establishing the registry cid as a real conversation. The probe is
        // advisory (`Some(true)`/`None` ⇒ resume), and returns `None` for codex, so codex
        // (which is never created here) always resumes and never reaches the create arm.
        let resume = !minted
            && job_engine::claude_conversation_exists(
                self.claude_home.as_deref(),
                root,
                engine,
                &cid,
            ) != Some(false);
        let turn_signal = paths.turn_signal();
        let argv = if resume {
            build_chat(engine, &cid, model, &turn_signal)
        } else {
            build_chat_create(engine, &cid, model, &turn_signal)
        };
        let argv = with_initial_prompt(argv, initial_prompt);
        // Through the INJECTABLE driver, not a fresh `TmuxDriver`: this is the one place create
        // starts a process, and a unit test must be able to observe it without a real tmux server.
        let launched = self
            .agent_tmux
            .launch_interactive(&session, root, &argv, &env);
        drop(lease);
        if let Err(e) = launched {
            return StandardStart::NotStarted(format!("{e}"));
        }
        // A live process now exists, so recording its id is safe. If THIS fails the agent is still
        // running and reachable by Enter; pmd would mint its own id only after this one dies, so a
        // lost seed orphans a conversation rather than forking a live one.
        // Only a MINTED id needs recording — a resumed OR create-from-ghost id came from the
        // registry/ledger already, and rewriting it would be a no-op write on the hot path of
        // every restart.
        if minted && let Err(e) = self.seed_registry_conversation_id(id, &cid) {
            return StandardStart::RunningWithWarning(format!("conversation id unrecorded: {e}"));
        }
        StandardStart::Running
    }
}

/// What a brand-new session's state directory is seeded with (see [`reserve_new_session`]).
pub(crate) struct NewSession<'a> {
    pub(crate) tier: Tier,
    pub(crate) engine: Engine,
    pub(crate) decider_engine: Engine,
    pub(crate) decider_model: Option<String>,
    pub(crate) brief: &'a str,
    pub(crate) cadence_s: Option<u64>,
}

/// Reserve the next free id for a new session in `dir`, named after the folder, and seed its
/// human-owned files (`config.json`, `control.json`, `brief.md`).
///
/// Shared by New and the spawn broker, so a spawned child is laid out exactly like a session a
/// human created. A seed failure leaves the reserved directory behind, as it always has for New;
/// [`reserve_unique_id`] never hands a directory that exists to a later session.
pub(crate) fn reserve_new_session(
    reg: &Registry,
    dir: &Path,
    seed: NewSession<'_>,
    now: Epoch,
) -> Result<String> {
    let id = reserve_unique_id(reg, dir, &new_session_id_base(dir))?;
    seed_agent_loop(
        &ProjectPaths::for_session(dir, &id),
        seed.tier,
        seed.engine,
        seed.decider_engine,
        seed.decider_model,
        seed.brief,
        seed.cadence_s,
        now,
    )?;
    Ok(id)
}

/// The id base a new session in `dir` is named from: the folder's name, made id-safe.
fn new_session_id_base(dir: &Path) -> String {
    sanitize_id(&project_id_base(dir))
}

/// A conversation id of the length `job_engine::mint_uuid_v4` produces, for sizing a launch
/// before its real id is minted.
const MINTED_ID_SHAPE: &str = "00000000-0000-4000-8000-000000000000";

/// The argv of a FRESH Standard launch carrying its one-time initial Message: Claude creates
/// conversation `claude_id` (`build_chat_create`), while Codex has no caller-chosen id, starts
/// its own (`build_codex_fresh_chat`) and ignores `claude_id`.
///
/// The one builder both the launch budget ([`fresh_launch_bytes`]) and the spawn broker's launch
/// use, so a budget check measures exactly the command that later runs.
pub(crate) fn fresh_standard_argv(
    engine: Engine,
    claude_id: &str,
    model: Option<&str>,
    turn_signal: &Path,
    message: Option<&str>,
) -> Vec<String> {
    let argv = match engine {
        Engine::Codex => build_codex_fresh_chat(model, turn_signal),
        Engine::Claude => build_chat_create(Engine::Claude, claude_id, model, turn_signal),
    };
    with_initial_prompt(argv, message)
}

/// The exact size, as tmux receives it ([`tmux::launch_command`]), of the fresh Standard launch
/// a new session in `dir` would start with `message`: built with [`fresh_standard_argv`] for the
/// id [`reserve_new_session`] would claim next and a conversation id of the minted length.
pub(crate) fn fresh_launch_bytes(
    reg: &Registry,
    dir: &Path,
    engine: Engine,
    model: Option<&str>,
    message: &str,
) -> usize {
    let id = unreserved_id(reg, dir, &new_session_id_base(dir));
    let turn_signal = ProjectPaths::for_session(dir, &id).turn_signal();
    let argv = fresh_standard_argv(engine, MINTED_ID_SHAPE, model, &turn_signal, Some(message));
    tmux::launch_command(&argv).len()
}

/// The size of the fresh Standard launch this form would start in `dir`, as tmux receives it
/// ([`tmux::launch_command`]), or `None` when there is no initial Message to bound.
///
/// Built with the argv builders [`App::start_undriven_session`] uses, for the id
/// `finish_create` would reserve and a conversation id of the minted length, so the estimate
/// is the launch itself. Enter's later retries rebuild the same shape (or the shorter resume).
pub(crate) fn initial_launch_bytes(form: &CreateForm, dir: &Path, reg: &Registry) -> Option<usize> {
    let message = initial_message(form)?;
    Some(fresh_launch_bytes(
        reg,
        dir,
        form.engine,
        form.worker_model.as_deref(),
        message,
    ))
}

/// A lower bound on [`initial_launch_bytes`] that needs no directory: the quoted `-- <Message>`
/// tail every Standard launch ends with. Over budget, no folder can make the launch fit.
pub(crate) fn initial_message_floor_bytes(form: &CreateForm) -> Option<usize> {
    let message = initial_message(form)?;
    Some(tmux::launch_command(&with_initial_prompt(Vec::new(), Some(message))).len())
}

fn byte_unit(count: usize) -> &'static str {
    if count == 1 { "byte" } else { "bytes" }
}

/// The Message a Standard form hands its fresh launch, or `None` when it hands none.
fn initial_message(form: &CreateForm) -> Option<&str> {
    let message = form.goal.trim();
    (form.tier == Tier::Standard && !message.is_empty()).then_some(message)
}

/// The id [`reserve_unique_id`] would claim for `base` in `root`, without claiming it.
fn unreserved_id(reg: &Registry, root: &Path, base: &str) -> String {
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        n += 1;
        if !reg.projects.iter().any(|project| project.id == candidate)
            && !ProjectPaths::for_session(root, &candidate)
                .state_dir()
                .exists()
        {
            return candidate;
        }
    }
}

pub(crate) fn with_initial_prompt(mut argv: Vec<String>, prompt: Option<&str>) -> Vec<String> {
    if let Some(prompt) = prompt.filter(|prompt| !prompt.trim().is_empty()) {
        argv.push("--".to_string());
        argv.push(prompt.to_string());
    }
    argv
}

/// What [`App::start_undriven_session`] managed, separating launch truth from
/// post-launch bookkeeping so one-shot input is never made retryable after it ran.
pub(crate) enum StandardStart {
    Running,
    RunningWithWarning(String),
    NotStarted(String),
}
