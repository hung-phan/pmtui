use crate::*;
use agent_manager::registry::MAX_CHILDREN_PER_PARENT;

const FORK_IDENTITY_ATTEMPTS: usize = 150;

#[cfg(not(test))]
const FORK_IDENTITY_POLL: Duration = Duration::from_millis(100);

#[cfg(test)]
const FORK_IDENTITY_POLL: Duration = Duration::ZERO;

/// Gap between the two source captures that must agree before a live source counts as idle:
/// one dashboard tick, the spacing the row's own idle gate compares across.
#[cfg(not(test))]
const FORK_IDLE_RECHECK: Duration = Duration::from_millis(500);

#[cfg(test)]
const FORK_IDLE_RECHECK: Duration = Duration::ZERO;

/// The source terminal as observed under the source input lock.
enum SourceTerminal {
    /// No live process, so only the saved identity can name the conversation.
    Absent,
    /// Live, unattached, and stably idle at its prompt.
    Idle,
}

/// What one fork created once its row was staged. A failed fork removes exactly this.
struct StagedFork {
    child_id: String,
    root: PathBuf,
    session: String,
    paths: ProjectPaths,
}

/// Why `fork_selected` refuses this row before touching any file, or `None` when it may try. The
/// Board derives its `f` chip from this, so the lane never offers a fork the handler always refuses.
pub(crate) fn fork_refusal(view: &ProjectView) -> Option<String> {
    // A staged spawn has not launched: its spawn request owns the row's first start.
    if view.spawn_staged {
        return Some(staged_refusal(&view.id));
    }
    // A failed fork's leftover has no conversation of its own; a live terminal under it is the
    // unverified child, so branching it would copy a conversation nobody confirmed.
    if view.incomplete_fork {
        return Some(format!(
            "{} is an incomplete fork with no conversation - delete it with d",
            view.id
        ));
    }
    if view.human_attached {
        return Some(format!(
            "{}: a human is attached; fork after detaching",
            view.id
        ));
    }
    if view.session_live && view.agent_working != Some(false) {
        return Some(format!("{}: still working; fork once it is idle", view.id));
    }
    None
}

impl App {
    /// `f`: queue a fork of the selected session for the next frame, which draws the "forking"
    /// status before [`App::run_pending_fork`] blocks the dashboard on the child's identity. A
    /// refusal the row alone decides is reported now instead.
    pub(crate) fn request_fork(&mut self, return_to_board: bool) {
        let Some(view) = self.selected_view() else {
            self.status = "no session selected".into();
            return;
        };
        if let Some(refusal) = fork_refusal(view) {
            self.status = refusal;
            return;
        }
        self.status = format!(
            "forking {} - the dashboard waits until the child reports its conversation",
            view.id
        );
        self.pending_fork = Some(return_to_board);
    }

    /// Run the fork [`App::request_fork`] queued, if any, and say whether one ran.
    pub(crate) fn run_pending_fork(&mut self) -> bool {
        let Some(return_to_board) = self.pending_fork.take() else {
            return false;
        };
        self.fork_selected();
        if return_to_board {
            self.mode = UiMode::Board;
            self.board_detail_open = false;
        }
        true
    }

    /// Branch the selected engine conversation into a new, ordinary Standard
    /// session. The source is read under its input lock and is never mutated.
    pub(crate) fn fork_selected(&mut self) {
        self.mode = UiMode::Normal;
        let Some(source_view) = self.selected_view().cloned() else {
            self.status = "no session selected".into();
            return;
        };
        if let Some(refusal) = fork_refusal(&source_view) {
            self.status = refusal;
            return;
        }

        let registry = match Registry::load(&self.registry_path) {
            Ok(registry) => registry,
            Err(error) => {
                self.status =
                    format!("could not read the session list ({error}) - not creating a fork");
                return;
            }
        };
        let Some(source) = registry
            .projects
            .iter()
            .find(|entry| entry.id == source_view.id)
            .cloned()
        else {
            self.status = format!("{} is gone from the session list", source_view.id);
            self.refresh();
            return;
        };
        // The same rule every start path applies: a row still mid-spawn has no conversation yet.
        if self.refuse_unstartable(&source) {
            return;
        }
        // The fork inherits `spawned_by`, so it would count as one more child of that parent.
        if let Some(parent) = source.spawned_by.as_deref()
            && registry.children_of(parent) >= MAX_CHILDREN_PER_PARENT
        {
            self.status =
                format!("parent {parent} already has {MAX_CHILDREN_PER_PARENT} spawned sessions");
            return;
        }
        let source_paths = entry_state_paths(&source);
        let ledger = match job::load(&source_paths) {
            Ok(ledger) => ledger,
            Err(error) => {
                self.status = format!("{}: runtime state unreadable ({error})", source.id);
                return;
            }
        };
        let ledger_id = ledger
            .as_ref()
            .and_then(|state| state.conversation_id.as_deref());
        let saved = match (ledger_id, source.conversation_id.as_deref()) {
            (Some(ledger), Some(registry)) if ledger != registry => {
                self.status = format!(
                    "{}: ledger conversation {ledger} disagrees with registry {registry}",
                    source.id
                );
                return;
            }
            (Some(id), _) | (None, Some(id)) => Some(id.to_string()),
            (None, None) => None,
        };
        let engine = match (ledger.as_ref().map(|state| state.engine), source.engine) {
            (Some(ledger), Some(registry)) if ledger != registry => {
                self.status = format!(
                    "{}: ledger engine {ledger:?} disagrees with registry {registry:?}",
                    source.id
                );
                return;
            }
            (Some(engine), _) | (None, Some(engine)) => engine,
            (None, None) => Engine::Claude,
        };
        // A Claude id is minted before the first turn, and Claude writes no transcript until
        // that turn runs. `--resume` of such an id opens an empty session, so a confidently
        // absent transcript is a session nobody has messaged yet.
        if engine == Engine::Claude
            && saved.as_deref().is_none_or(|id| {
                job_engine::claude_conversation_exists(
                    self.claude_home.as_deref(),
                    &source.root,
                    engine,
                    id,
                ) == Some(false)
            })
        {
            self.refuse_unstarted_fork(&source.id);
            return;
        }

        let _source_input = match lease::try_acquire(&source_paths.input_lock()) {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                self.status = format!("{} is receiving input; try the fork again", source.id);
                return;
            }
            Err(error) => {
                self.status = format!("{}: could not lock its input ({error})", source.id);
                return;
            }
        };
        let Some(terminal) = self.fork_source_terminal(&source, &source_paths, engine) else {
            return;
        };
        let Some(source_conversation) =
            self.fork_source_conversation(&source, engine, terminal, saved)
        else {
            return;
        };

        let config = match state::read_json::<Config>(&source_paths.config()) {
            Ok(config) => config,
            Err(error) => {
                self.status = format!("{}: settings unreadable ({error})", source.id);
                return;
            }
        };
        let brief = match read_optional_text(&source_paths.brief()) {
            Ok(brief) => brief.unwrap_or_default(),
            Err(error) => {
                self.status = format!("{}: goal unreadable ({error})", source.id);
                return;
            }
        };
        let directive = match read_optional_text(&source_paths.directive()) {
            Ok(directive) => directive,
            Err(error) => {
                self.status = format!("{}: directive unreadable ({error})", source.id);
                return;
            }
        };

        let base = sanitize_id(&format!("{}-fork", source.id));
        let child_id = match reserve_unique_id(&registry, &source.root, &base) {
            Ok(id) => id,
            Err(error) => {
                self.status = format!("could not reserve a fork session: {error}");
                return;
            }
        };
        let child_paths = ProjectPaths::for_session(&source.root, &child_id);
        let child = ProjectEntry {
            id: child_id.clone(),
            display_name: None,
            root: source.root.clone(),
            enabled: false,
            mode: Mode::AgentLoop,
            engine: Some(engine),
            worker_model: source.worker_model.clone(),
            initial_prompt: None,
            task_title: source
                .task_title
                .clone()
                .or_else(|| intent_title(&brief))
                .or_else(|| source.initial_prompt.as_deref().and_then(intent_title)),
            forked_from: Some(source.id.clone()),
            // Lineage survives the fork, so forking a child cannot escape the parent's depth rule
            // or its child cap (checked above).
            spawned_by: source.spawned_by.clone(),
            launch: None,
            conversation_id: None,
            cadence_s: None,
        };
        let staged = match Registry::load(&self.registry_path) {
            Ok(mut registry) => {
                registry.projects.push(child);
                registry.save(&self.registry_path)
            }
            Err(error) => Err(error),
        };
        if let Err(error) = staged {
            let cleanup = cleanup_unstaged_fork(&child_paths);
            self.status = match cleanup {
                Ok(()) => format!("could not stage {child_id} in the session list: {error}"),
                Err(cleanup) => format!(
                    "could not stage {child_id} in the session list ({error}) or remove its reservation ({cleanup})"
                ),
            };
            return;
        }
        let fork = StagedFork {
            child_id: child_id.clone(),
            root: source.root.clone(),
            session: session_name(&child_id, &source.root),
            paths: child_paths,
        };
        let child_lease = match prepare_fork_child(&fork, engine, config, &brief, directive) {
            Ok(lease) => lease,
            Err(error) => {
                self.abandon_fork(&fork, false, format!("{error:#}"));
                return;
            }
        };

        let identity_file = fork.paths.daemon_dir().join("fork-conversation-id");
        let argv = agent_manager::worker::build_fork_command(
            engine,
            &source_conversation,
            agent_manager::worker::turn_hook_enabled().then_some(&fork.paths.turn_signal()),
            (engine == Engine::Claude).then_some(identity_file.as_path()),
            source.worker_model.as_deref(),
        );
        let env = self.managed_env(&fork.child_id, &fork.root);
        crate::session::install_spawn_skill(&fork.root, engine, &env);
        let launched = self
            .agent_tmux
            .launch_interactive(&fork.session, &fork.root, &argv, &env);
        drop(child_lease);
        if let Err(error) = launched {
            self.abandon_fork(&fork, true, format!("could not start {child_id}: {error}"));
            return;
        }

        let conversation_id = match self.wait_for_fork_identity(
            engine,
            &fork.session,
            &fork.root,
            &identity_file,
            &source_conversation,
        ) {
            Ok(id) => id,
            Err(error) => {
                let reason = format!("{child_id}: could not capture its conversation ({error:#})");
                self.abandon_fork(&fork, true, reason);
                return;
            }
        };
        let promoted = match Registry::load(&self.registry_path) {
            Ok(registry)
                if registry.projects.iter().any(|entry| {
                    entry.id != child_id
                        && entry
                            .conversation_id
                            .as_deref()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&conversation_id))
                }) =>
            {
                Err(anyhow::anyhow!(
                    "conversation identity already belongs to another session"
                ))
            }
            Ok(mut registry) => {
                match registry
                    .projects
                    .iter_mut()
                    .find(|entry| entry.id == child_id && entry.root == source.root)
                {
                    Some(child) => {
                        child.conversation_id = Some(conversation_id.clone());
                        child.enabled = true;
                        registry.save(&self.registry_path)
                    }
                    None => Err(anyhow::anyhow!("staged row disappeared")),
                }
            }
            Err(error) => Err(error),
        };
        if let Err(error) = promoted {
            self.abandon_fork(&fork, true, format!("could not save {child_id} ({error})"));
            return;
        }

        self.refresh();
        if let Some(index) = self.projects.iter().position(|view| view.id == child_id) {
            self.select_project_index(index);
        }
        self.status = format!(
            "forked {} -> {child_id}; Standard, ready for Enter or Message",
            source.id
        );
    }

    /// Record the refusal for a source with no conversation a fork could branch.
    fn refuse_unstarted_fork(&mut self, id: &str) {
        self.status = format!("{id} has no conversation to fork - send its first message first");
    }

    /// Poll, within a bounded number of attempts, until the child engine reports its own
    /// conversation.
    ///
    /// The caller keeps the source input lock for the whole wait. The child's identity is the
    /// earliest evidence that the engine has finished reading the source history, so input that
    /// reached the source before then could land in the snapshot.
    fn wait_for_fork_identity(
        &self,
        engine: Engine,
        session: &str,
        root: &Path,
        identity_file: &Path,
        source_id: &str,
    ) -> Result<String> {
        let mut last_error = None;
        for _ in 0..FORK_IDENTITY_ATTEMPTS {
            let probe = match engine {
                Engine::Claude => self.agent_tmux.claude_session_id(session, identity_file),
                Engine::Codex => self.agent_tmux.codex_session_id(session, root),
            };
            match probe {
                // Codex holds the source rollout open while it copies the history, so seeing
                // it is a startup state. Claude's hook reports once, so its answer is final.
                Ok(Some(id)) if id.eq_ignore_ascii_case(source_id) => {
                    if engine == Engine::Claude {
                        anyhow::bail!("engine returned the source conversation id");
                    }
                    last_error = Some(anyhow::anyhow!(
                        "engine still reports the source conversation id"
                    ));
                }
                Ok(Some(id)) => return Ok(id),
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
            if !self
                .fork_terminal_running(session)
                .context("inspect fork terminal while waiting for identity")?
            {
                anyhow::bail!("fork terminal exited before reporting its conversation id");
            }
            std::thread::sleep(FORK_IDENTITY_POLL);
        }
        match last_error {
            Some(error) => Err(error.context("identity probe did not settle")),
            None => anyhow::bail!("identity probe timed out"),
        }
    }

    /// Whether the child's process is still running. Under `remain-on-exit` a crashed child
    /// leaves its session behind with a dead pane, which is an exit, not a slow start. The spawn
    /// broker asks the same question of a spawned child.
    pub(super) fn fork_terminal_running(&self, session: &str) -> Result<bool> {
        Ok(self.agent_tmux.is_alive(session)? && !self.agent_tmux.pane_dead(session)?)
    }

    /// Inspect the source terminal under its input lock. A live source must be unattached and
    /// stably idle; `None` means the fork was refused and the status says why.
    fn fork_source_terminal(
        &mut self,
        source: &ProjectEntry,
        paths: &ProjectPaths,
        engine: Engine,
    ) -> Option<SourceTerminal> {
        let session = session_name(&source.id, &source.root);
        let alive = match self.agent_tmux.is_alive(&session) {
            Ok(alive) => alive,
            Err(error) => {
                self.status = format!("{}: could not inspect its terminal ({error})", source.id);
                return None;
            }
        };
        if !alive {
            return Some(SourceTerminal::Absent);
        }
        match self.agent_tmux.pane_dead(&session) {
            Ok(true) => return Some(SourceTerminal::Absent),
            Ok(false) => {}
            Err(error) => {
                self.status = format!("{}: could not inspect its terminal ({error})", source.id);
                return None;
            }
        }
        if chat_lock::is_active(paths, SystemClock.now())
            || self.agent_tmux.has_clients(&session).unwrap_or(true)
        {
            self.status = format!("{}: a human is attached; fork after detaching", source.id);
            return None;
        }
        self.source_stays_idle(source, paths, engine, &session)
            .then_some(SourceTerminal::Idle)
    }

    /// A live source is idle only when two captures one recheck apart are both Idle with the
    /// same transcript, and no hook-reported turn is still outstanding. One Idle frame is not
    /// proof: Claude can draw its bare prompt while a response is still streaming. The hook is
    /// merged exactly as the dashboard row merges it.
    fn source_stays_idle(
        &mut self,
        source: &ProjectEntry,
        paths: &ProjectPaths,
        engine: Engine,
        session: &str,
    ) -> bool {
        let Some(first) = self.idle_source_fingerprint(source, session) else {
            return false;
        };
        std::thread::sleep(FORK_IDLE_RECHECK);
        let Some(second) = self.idle_source_fingerprint(source, session) else {
            return false;
        };
        let hook_working =
            ProjectView::read_agent_loop(&source.id, paths, source.enabled, SystemClock.now())
                .agent_working;
        if first != second
            || super::refresh::confirmed_idle_activity(Some(engine), hook_working) != Some(false)
        {
            self.status = format!("{}: still working; fork once it is idle", source.id);
            return false;
        }
        true
    }

    /// The transcript fingerprint of one Idle capture of the source; `None` records a refusal.
    fn idle_source_fingerprint(&mut self, source: &ProjectEntry, session: &str) -> Option<u64> {
        match self.agent_tmux.capture_tail(session, 40) {
            Ok(capture) if tmux::classify_pane(&capture) == tmux::PaneActivity::Idle => {
                Some(tmux::idle_fingerprint(&capture))
            }
            Ok(_) => {
                self.status = format!("{}: still working; fork once it is idle", source.id);
                None
            }
            Err(error) => {
                self.status = format!("{}: could not read its terminal ({error})", source.id);
                None
            }
        }
    }

    /// Name the conversation the fork branches.
    ///
    /// A live Codex source is identified from its exact process, which is the only record a
    /// Standard Codex session has: pmtui starts it without an id and pmd never drives it. A saved
    /// id that disagrees with the live conversation refuses, as restart does, rather than
    /// branching a thread the human has left. A probe that cannot prove any conversation falls
    /// back to the saved id.
    fn fork_source_conversation(
        &mut self,
        source: &ProjectEntry,
        engine: Engine,
        terminal: SourceTerminal,
        saved: Option<String>,
    ) -> Option<String> {
        if engine == Engine::Codex && matches!(terminal, SourceTerminal::Idle) {
            let session = session_name(&source.id, &source.root);
            match self.agent_tmux.codex_session_id(&session, &source.root) {
                Ok(Some(live)) => {
                    if let Some(saved) = saved.as_deref()
                        && !saved.eq_ignore_ascii_case(&live)
                    {
                        self.status = format!(
                            "{}: live Codex conversation {live} does not match saved id {saved}",
                            source.id
                        );
                        return None;
                    }
                    return Some(live);
                }
                Ok(None) => {}
                Err(error) => {
                    self.status = format!(
                        "{}: could not identify the live Codex conversation ({error})",
                        source.id
                    );
                    return None;
                }
            }
        }
        if saved.is_none() {
            self.refuse_unstarted_fork(&source.id);
        }
        saved
    }

    /// Undo a fork that could not publish a working child, so a failure never leaves a row that
    /// looks like a paused session. The source is never touched.
    fn abandon_fork(&mut self, fork: &StagedFork, launched: bool, reason: String) {
        self.status = match self.discard_fork(fork, launched) {
            Ok(()) => format!("{reason}; fork discarded"),
            Err(kept) => format!("{reason}; {kept}"),
        };
        self.refresh();
    }

    /// Stop the child terminal, then remove the staged row and the state directory this fork
    /// reserved. A terminal that cannot be stopped keeps its row, because removing the row would
    /// hide a live process; the kept row stays disabled and refuses to start.
    fn discard_fork(&self, fork: &StagedFork, launched: bool) -> Result<(), String> {
        if launched && let Err(error) = self.agent_tmux.terminate(&fork.session) {
            return Err(format!(
                "could not stop its terminal ({error}); disabled row kept - delete it with d"
            ));
        }
        self.discard_staged_row(&fork.child_id, &fork.root, |entry| {
            entry.conversation_id.is_none()
        })
    }

    /// Remove the disabled row `id` under `root` that a fork or a spawn request staged, then the
    /// exact state directory it reserved. `still_staged` revalidates the row inside the registry
    /// write: a row it no longer accepts is kept, and so is its directory, because the row has
    /// become something this caller did not stage.
    pub(crate) fn discard_staged_row(
        &self,
        id: &str,
        root: &Path,
        still_staged: impl Fn(&ProjectEntry) -> bool,
    ) -> Result<(), String> {
        let mut kept = false;
        Registry::update(&self.registry_path, |registry| {
            registry.projects.retain(|entry| {
                let ours = entry.id == id && entry.root == root;
                let discard = ours && still_staged(entry);
                kept |= ours && !discard;
                !discard
            });
        })
        .map_err(|error| {
            format!("could not remove its disabled row ({error}) - delete it with d")
        })?;
        if kept {
            return Err("its row changed after it was staged, so it was kept".into());
        }
        cleanup_unstaged_fork(&ProjectPaths::for_session(root, id))
            .map_err(|error| format!("could not remove its reserved state ({error:#})"))
    }

    /// Refuse to start a row that may not start by any route (see [`start_refusal`]), recording
    /// why in the status. `true` means the caller must stop.
    pub(crate) fn refuse_unstartable(&mut self, entry: &ProjectEntry) -> bool {
        match start_refusal(entry) {
            Some(refusal) => {
                self.status = refusal;
                true
            }
            None => false,
        }
    }
}

/// Why a row may not start by resume, restart, Autopilot, Message or fork, or `None` for an
/// ordinary row. Three rows qualify:
/// - a staged spawn ([`ProjectEntry::is_staged_spawn`]): the dashboard's spawn broker owns its
///   one launch, and any other start could send its Message a second time;
/// - a JOB ([`ProjectEntry::is_job`]): there is no conversation to resume, nobody to Message, and no
///   dial to turn — it does its one task and exits, and the broker retires it;
/// - an incomplete fork ([`incomplete_fork_refusal`]).
pub(crate) fn start_refusal(entry: &ProjectEntry) -> Option<String> {
    if entry.is_staged_spawn() {
        return Some(staged_refusal(&entry.id));
    }
    if entry.is_job() {
        return Some(job_refusal(&entry.id));
    }
    incomplete_fork_refusal(entry)
}

/// What a job row says to every key that assumes an agent is waiting in it.
pub(crate) fn job_refusal(id: &str) -> String {
    format!("{id} is a job \u{2014} it reports its result and exits")
}

fn staged_refusal(id: &str) -> String {
    format!("{id} is still being created by a spawn request")
}

/// Whether [`start_refusal`] refuses the row this view shows: `refresh` records its two arms as
/// [`ProjectView::spawn_staged`] and [`ProjectView::incomplete_fork`]. The keybar and Task board
/// read this, so neither offers a start, pause or fork the handler would refuse.
pub(crate) fn start_refused(view: &ProjectView) -> bool {
    view.spawn_staged || view.incomplete_fork
}

/// Why a row a failed fork left behind may not start, or `None` for any other row: lineage
/// recorded, no conversation ever captured. Starting it by resume, restart or Autopilot would
/// open a blank conversation under a "fork from" label. The dashboard marks
/// [`ProjectView::incomplete_fork`] from this alone, so a staged spawn never reads as a fork.
pub(crate) fn incomplete_fork_refusal(entry: &ProjectEntry) -> Option<String> {
    let parent = entry.forked_from.as_deref()?;
    let captured = entry.conversation_id.is_some()
        || job::load(&entry_state_paths(entry))
            .ok()
            .flatten()
            .is_some_and(|ledger| ledger.conversation_id.is_some());
    (!captured).then(|| {
        format!(
            "{} is an incomplete fork of {parent} with no conversation - delete it with d",
            entry.id
        )
    })
}

/// Seed the staged child's human-owned files and claim its driver lock for the launch.
fn prepare_fork_child(
    fork: &StagedFork,
    engine: Engine,
    config: Config,
    brief: &str,
    directive: Option<String>,
) -> Result<ProjectLease> {
    let child = &fork.child_id;
    seed_agent_loop(
        &fork.paths,
        Tier::Standard,
        engine,
        config.decider_engine,
        config.decider_model,
        brief,
        None,
        SystemClock.now(),
    )
    .context(format!("could not seed {child}"))?;
    if let Some(directive) = directive {
        state::write_text_atomic(&fork.paths.directive(), &directive)
            .context(format!("could not copy the directive into {child}"))?;
    }
    lease::try_acquire(&fork.paths.daemon_dir().join("driver.lock"))
        .context(format!("{child}: could not claim the fork"))?
        .context(format!("{child}: another process claimed the fork"))
}

fn read_optional_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn cleanup_unstaged_fork(paths: &ProjectPaths) -> Result<()> {
    match std::fs::remove_dir_all(paths.state_dir()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", paths.state_dir().display())),
    }
}
