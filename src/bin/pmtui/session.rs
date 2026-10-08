//! Suspend the TUI, hand the real terminal to a child process, and restore it.
//! Unified project-terminal attach/launch and `$EDITOR` handoffs share this
//! load-bearing terminal-mode sequence.

use crate::*;
use std::os::unix::process::ExitStatusExt;

/// Hand the real terminal to a foreground child: leave raw mode, DROP the two modes pmtui turns on
/// at startup (mouse capture and bracketed paste), leave the alternate screen. The exact
/// counterpart to [`restore_tui`] — the two MUST stay symmetric, and both must mirror `main`'s
/// startup so a return from a child lands the terminal in the SAME modes as a fresh launch.
///
/// RE-ASSERTING these on return (in `restore_tui`) is the fix for input going wrong after a detach:
/// a child like tmux / `claude` reprograms the terminal's mouse-tracking and paste modes, and pmtui
/// used to never re-issue its own on return, so the dashboard came back running in the CHILD's
/// leftover modes. For the WHEEL that meant the burst stopped coalescing into one frame and every
/// notch forced a `capture-pane` fork (user: *"after i enter the session then Ctrl+q to quit, the
/// scrolling stop working smoothly"*); for PASTE it would mean a bracketed paste arriving as raw
/// keystrokes. Best-effort on both toggles, like `main`'s startup: a terminal that refuses one is no
/// reason to fail the handoff.
#[cfg(not(test))]
fn suspend_tui() -> Result<()> {
    disable_raw_mode()?;
    let _ = std::io::stdout().execute(DisableMouseCapture);
    let _ = std::io::stdout().execute(DisableBracketedPaste);
    std::io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

/// Restore the TUI after the child returns: re-enter the alternate screen, RE-ASSERT pmtui's mouse
/// capture AND bracketed paste (the load-bearing half — see [`suspend_tui`]), re-enable raw mode,
/// repaint.
#[cfg(not(test))]
fn restore_tui(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let _ = std::io::stdout().execute(EnableMouseCapture);
    let _ = std::io::stdout().execute(EnableBracketedPaste);
    terminal.clear()?;
    Ok(())
}

pub(crate) trait TerminalHandoff {
    fn suspend(&mut self) -> Result<()>;
    fn restore(&mut self) -> Result<()>;
}

#[cfg(not(test))]
pub(crate) struct RealTerminalHandoff<'a> {
    pub(crate) terminal: &'a mut ratatui::DefaultTerminal,
}

#[cfg(not(test))]
impl TerminalHandoff for RealTerminalHandoff<'_> {
    fn suspend(&mut self) -> Result<()> {
        suspend_tui()
    }

    fn restore(&mut self) -> Result<()> {
        restore_tui(self.terminal)
    }
}

type ChildStatus = Result<std::process::ExitStatus>;
type ChildCallback<'a> = Box<dyn FnOnce() -> ChildStatus + 'a>;
type EditorCallback<'a> = Box<dyn FnOnce(&Path) -> ChildStatus + 'a>;
type EditorParser = fn(&str) -> String;

fn with_terminal_handoff(
    handoff: &mut dyn TerminalHandoff,
    child: ChildCallback<'_>,
) -> Result<ChildStatus> {
    handoff.suspend()?;
    let result = child();
    handoff.restore()?;
    Ok(result)
}

fn attach_with_terminal_handoff(
    handoff: &mut dyn TerminalHandoff,
    driver: &dyn Driver,
    session: &str,
) -> Result<()> {
    with_terminal_handoff(
        handoff,
        Box::new(|| {
            driver.attach_interactive(session)?;
            Ok(std::process::ExitStatus::from_raw(0))
        }),
    )??;
    Ok(())
}

/// A request to attach to, or relaunch, a project's unified terminal.
pub(crate) struct ChatReq {
    pub(crate) session_paths: ProjectPaths,
    /// The session's project root — the REPL's cwd. MUST be the registry entry's
    /// `root` (the path the user entered on create), NOT `session_paths` (the
    /// per-session state dir under `.project-state/sessions/<seg>/`). This mirrors
    /// the daemon's `spawn_step(&session, &work_dir, …)` `work_dir`, so an
    /// interactively-chatted agent operates on the same files the poll worker does.
    pub(crate) root: PathBuf,
    pub(crate) argv: Vec<String>,
    pub(crate) label: String,
    /// The private tmux server socket (`tmux -L <socket>`) to launch the REPL on —
    /// the SAME server the daemon/`attach`/`watch` use, so Ctrl+q → `detach-client`.
    pub(crate) socket: String,
    /// The unified `pm-…` terminal. Ctrl+q detaches without ending its process.
    pub(crate) session: String,
    /// The row's engine, so a launch ships the spawn skill in the form that engine discovers.
    pub(crate) engine: Engine,
    /// Who the terminal belongs to, applied to it if this request launches it
    /// ([`App::managed_env`]).
    pub(crate) env: tmux::ManagedEnv,
}

/// A request to create a fresh Claude conversation in the unified terminal.
/// The won project lease is carried by value through launch so pmtui and pmd
/// cannot create the same conversation concurrently.
pub(crate) struct CreateChatReq {
    pub(crate) session_paths: ProjectPaths,
    /// The session's project root — the REPL's cwd (see [`ChatReq::root`]). The
    /// registry entry's `root`, NOT the per-session state dir.
    pub(crate) root: PathBuf,
    pub(crate) argv: Vec<String>,
    pub(crate) label: String,
    pub(crate) lease: ProjectLease,
    /// The private tmux server socket (`tmux -L <socket>`) to launch the REPL on —
    /// the SAME server the daemon/`attach`/`watch` use, so Ctrl+q → `detach-client`.
    pub(crate) socket: String,
    /// The unified `pm-…` terminal.
    pub(crate) session: String,
    /// The row's engine (see [`ChatReq::engine`]).
    pub(crate) engine: Engine,
    /// Who the terminal belongs to ([`App::managed_env`]).
    pub(crate) env: tmux::ManagedEnv,
}

/// Ship the pmtui-spawn skill into `root` before pmtui launches a terminal there with `env`
/// ([`agent_manager::skills::install_spawn_skill_for_launch`]: nothing when `env` names no pmtui).
///
/// Fail-open: a failed install never blocks the launch, and pmtui cannot print over its live
/// dashboard, so `pmd doctor` is where a missing or stale spawn skill surfaces.
pub(crate) fn install_spawn_skill(root: &Path, engine: Engine, env: &tmux::ManagedEnv) {
    let _ = agent_manager::skills::install_spawn_skill_for_launch(root, engine, env);
}

/// Build the argv for a **real interactive** engine REPL resuming a session's
/// conversation by id — the on-demand chat launch. Mirrors the poll worker's
/// resume shape but WITHOUT the headless flags, so the human gets a live REPL on
/// the SAME conversation the poll drives:
/// - `Engine::Claude` → `env -u CLAUDECODE claude --settings <turn-hook>
///   --resume <id>` (NO `-p`). The
///   `env -u CLAUDECODE` drops the inherited Claude Code marker — pmtui may itself
///   run inside a Claude Code session — exactly like the poll worker and the
///   verified spike; without it the child refuses to start a nested session.
/// - `Engine::Codex` → `codex resume <id>` (NO `exec`; that is the headless path).
pub(crate) fn build_chat(
    engine: Engine,
    conversation_id: &str,
    model: Option<&str>,
    turn_signal: &Path,
) -> Vec<String> {
    agent_manager::worker::build_standard_command(
        engine,
        &agent_manager::worker::Resume::Continue(conversation_id.to_string()),
        agent_manager::worker::turn_hook_enabled().then_some(turn_signal),
        model,
    )
}

/// Build the argv for a **real interactive** engine REPL that CREATES a brand-new
/// conversation with a caller-chosen id — the "chattable-on-create" launch (the
/// CREATE sibling of [`build_chat`], which RESUMES). Only `Engine::Claude` is
/// creatable in v1: interactive `claude --session-id <uuid>` opens a chattable REPL
/// on that exact id, so pmtui can mint the id, seed it into the registry (the daemon
/// then adopts+resumes it), and hand the human a live conversation immediately —
/// with NO `-p` (interactive, not headless) and NO `--resume` (it is a fresh
/// create). `env -u CLAUDECODE` drops the inherited Claude Code marker exactly like
/// [`build_chat`]/the poll worker, so the child isn't refused as a nested session.
///
/// `Engine::Codex` is UNREACHABLE here: codex exposes no caller-chosen id for an
/// interactive session, so it is never created by pmtui in v1 (it uses the armed
/// auto-open fallback). The `request_attach` router never routes codex to a create,
/// so this arm cannot be hit; it panics to make that invariant explicit.
pub(crate) fn build_chat_create(
    engine: Engine,
    conversation_id: &str,
    model: Option<&str>,
    turn_signal: &Path,
) -> Vec<String> {
    match engine {
        Engine::Claude => agent_manager::worker::build_standard_command(
            engine,
            &agent_manager::worker::Resume::Fresh {
                session_id: Some(conversation_id.to_string()),
            },
            agent_manager::worker::turn_hook_enabled().then_some(turn_signal),
            model,
        ),
        Engine::Codex => {
            unreachable!(
                "codex conversations are never created by pmtui in v1 (no caller-chosen id)"
            )
        }
    }
}

/// Build the argv for a FRESH interactive codex chat — the hand-started (Enter) create for codex,
/// which has no caller-chosen id (so no `--session-id`/`resume`). Interactive, human-answered
/// approvals (NO unattended `--ask-for-approval never`/`--sandbox` — a human is present, mirroring
/// `build_chat`'s codex-resume posture). Directory trust remains an explicit
/// first-run prompt that the human answers in this terminal.
pub(crate) fn build_codex_fresh_chat(model: Option<&str>, turn_signal: &Path) -> Vec<String> {
    agent_manager::worker::build_standard_command(
        Engine::Codex,
        &agent_manager::worker::Resume::Fresh { session_id: None },
        agent_manager::worker::turn_hook_enabled().then_some(turn_signal),
        model,
    )
}

/// Attach to an already-running unified project terminal. Ctrl+q detaches and
/// leaves the process alive; any tmux attach exit returns to the dashboard.
pub(crate) fn attach_loop_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    driver: &dyn Driver,
    paths: &ProjectPaths,
    socket: &str,
    session: &str,
) -> Result<()> {
    let _input_lease =
        lease::acquire_with_retry(&paths.input_lock(), 40, Duration::from_millis(25))?
            .context("agent input is busy — try Enter again")?;
    chat_lock::mark(
        paths,
        std::process::id(),
        session,
        socket,
        SystemClock.now(),
    )?;
    // Ensure a bare Ctrl+q detaches on THIS server (pmd's `spawn_step` never binds it),
    // so a single keystroke pops back to the dashboard and LEAVES the agent running.
    driver.ensure_detach_key();
    // Undo any preview-fit `resize_window` (which set `window-size manual`) so this attach
    // resizes the pane to the human's real terminal, not the last preview size.
    let _ = driver.set_window_size_auto(session);
    // Hand the real terminal to tmux; restore the TUI on detach/exit — the SAME
    // suspend/restore sequence `attach`/`chat` use.
    let result = attach_with_terminal_handoff(handoff, driver, session);
    chat_lock::clear(paths);
    result
}

/// Attach to the unified terminal, launching it only when absent. Attach intent
/// is marked before launch and cleared on every return; tmux client detection
/// gates pmd while the human is attached. Ledger and lease checks protect the
/// remaining create/resume races.
pub(crate) fn chat_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    driver: &dyn Driver,
    req: &ChatReq,
    on_started: impl FnOnce(),
) -> Result<()> {
    // Write the marker so the daemon defers its wakes while we're attached.
    chat_lock::mark(
        &req.session_paths,
        std::process::id(),
        &req.session,
        &req.socket,
        SystemClock.now(),
    )?;
    // Clear attach intent on every exit path, including early errors and panics.
    struct ChatGuard<'a> {
        paths: &'a ProjectPaths,
    }
    impl Drop for ChatGuard<'_> {
        fn drop(&mut self) {
            chat_lock::clear(self.paths);
        }
    }
    let _guard = ChatGuard {
        paths: &req.session_paths,
    };

    // A launch can race a just-started wake. Refuse when the unified terminal is
    // still absent and the durable run record says another process is starting.
    if !driver.is_alive(&req.session).unwrap_or(false)
        && let Ok(Some(ledger)) = job::load(&req.session_paths)
        && matches!(ledger.run, job::JobRun::Running { .. })
    {
        anyhow::bail!("a wake just started — try again in a moment");
    }
    // Idempotent by unified session name: an existing process is attached, not relaunched.
    install_spawn_skill(&req.root, req.engine, &req.env);
    driver.launch_interactive(&req.session, &req.root, &req.argv, &req.env)?;
    on_started();

    // Undo any preview-fit `resize_window` (which set `window-size manual`) so this attach
    // resizes the pane to the human's real terminal, not the last preview size.
    let _ = driver.set_window_size_auto(&req.session);
    // Hand the real terminal to tmux; restore the TUI on detach/exit — the SAME
    // suspend/restore sequence `attach`/`watch`/`edit_brief` use.
    // Any attach exit is a clean "left the chat": Ctrl+q detaches (exit 0); a
    // `/exit`/Ctrl+D inside claude tears the pane down so `tmux attach` exits
    // non-zero — both mean "return to the dashboard" (cf. `watch`). Ignore it.
    attach_with_terminal_handoff(handoff, driver, &req.session)?;
    Ok(())
}

/// Create a fresh Claude conversation in the unified terminal. The project lease
/// remains held through launch and attach, preventing pmd from creating or adopting
/// the same conversation concurrently.
pub(crate) fn create_chat_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    driver: &dyn Driver,
    req: CreateChatReq,
    on_started: impl FnOnce(),
) -> Result<()> {
    // Write the marker so the daemon defers its wakes while we're attached.
    chat_lock::mark(
        &req.session_paths,
        std::process::id(),
        &req.session,
        &req.socket,
        SystemClock.now(),
    )?;
    // Clear attach intent before the lease is released on every exit path.
    struct CreateGuard {
        paths: ProjectPaths,
        _lease: ProjectLease,
    }
    impl Drop for CreateGuard {
        fn drop(&mut self) {
            chat_lock::clear(&self.paths);
        }
    }
    let _guard = CreateGuard {
        paths: req.session_paths,
        _lease: req.lease,
    };
    let argv = req.argv;
    let root = req.root;
    let session = req.session;

    // Idempotent by unified session name: an existing process is attached, not relaunched.
    install_spawn_skill(&root, req.engine, &req.env);
    driver.launch_interactive(&session, &root, &argv, &req.env)?;
    on_started();

    // Undo any preview-fit `resize_window` (which set `window-size manual`) so this attach
    // resizes the pane to the human's real terminal, not the last preview size.
    let _ = driver.set_window_size_auto(&session);
    attach_with_terminal_handoff(handoff, driver, &session)?;
    Ok(())
}

pub(crate) fn drain_terminal_requests(
    app: &mut App,
    handoff: &mut dyn TerminalHandoff,
    editor: &str,
) {
    if let Some(req) = app.pending_chat.take() {
        let label = req.label.clone();
        let result = chat_with_handoff(handoff, &*app.agent_tmux, &req, || {
            app.initial_message_retries.remove(&label);
        })
        .map(|()| app.agent_tmux.is_alive(&req.session).unwrap_or(false));
        finish_pending_chat(app, &req.label, result);
    }
    if let Some(req) = app.pending_create_chat.take() {
        let (label, session) = (req.label.clone(), req.session.clone());
        let result = create_chat_with_handoff(handoff, &*app.agent_tmux, req, || {
            app.initial_message_retries.remove(&label);
        })
        .map(|()| app.agent_tmux.is_alive(&session).unwrap_or(false));
        finish_pending_create_chat(app, &label, result);
    }
    if let Some(session) = app.pending_attach_loop.take() {
        let (label, paths) = pending_attach_context(app, &session);
        let result = paths.map(|paths| {
            attach_loop_with_handoff(handoff, &*app.agent_tmux, &paths, &app.socket, &session)
        });
        finish_pending_attach(app, &label, result);
    }
    if let Some(req) = app.pending_brief_edit.take() {
        let edited = edit_brief(handoff, &req.goal, editor);
        finish_pending_brief_edit(app, req, edited);
    }
    if let Some(req) = app.pending_directive_edit.take() {
        let edited = edit_directive(handoff, &req.current, editor);
        finish_pending_directive_edit(app, req, edited);
    }
    if let Some(req) = app.pending_send.take() {
        let edited = edit_send_message(handoff, &req.seed, editor);
        finish_pending_send(app, req, edited);
    }
}

pub(crate) fn editor_temp(kind: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("pmtui-{kind}-{}-{nonce}.md", std::process::id()))
}

#[cfg(not(test))]
pub(crate) fn editor_command() -> String {
    editor_command_from(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok())
}

pub(crate) fn editor_command_from(visual: Option<String>, editor: Option<String>) -> String {
    visual.or(editor).unwrap_or_else(|| "vi".to_string())
}

struct EditorTemp<'a>(&'a Path);

impl Drop for EditorTemp<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

fn edit_buffer_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    tmp: &Path,
    editor: &str,
    kind: &str,
    seed: &str,
    run_editor: EditorCallback<'_>,
    parse: EditorParser,
) -> Result<Option<String>> {
    let _temp = EditorTemp(tmp);
    std::fs::write(tmp, seed).context(format!("write {kind} temp {}", tmp.display()))?;

    let status = with_terminal_handoff(handoff, Box::new(|| run_editor(tmp)))?;
    match status {
        Ok(st) if st.success() => {
            let raw = std::fs::read_to_string(tmp)
                .with_context(|| format!("read {kind} temp {}", tmp.display()))?;
            let text = parse(&raw);
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }
        Ok(st) => Err(anyhow::anyhow!("editor {editor:?} exited with status {st}")),
        Err(e) => Err(e.context(format!("could not launch editor {editor:?}"))),
    }
}

/// Compose a MESSAGE in `$EDITOR` for `s`. Same terminal suspend/restore ownership as
/// [`edit_brief`], and a different parser on purpose.
///
/// The buffer is taken VERBATIM apart from control-byte sanitising. It deliberately does
/// NOT reuse `brief_from_editor_buffer`: briefs are plain text, while a message must remove
/// control bytes that could change terminal mode or submit early. A message starting with
/// `# heading`, `#!/bin/sh`, or `# TODO` remains content. The seed carries no guidance header,
/// and an empty save simply sends nothing.
///
/// The temp filename is `pmtui-send-…`, not `pmtui-brief-…`, so a concurrent goal edit
/// cannot collide with it.
pub(crate) fn edit_send_message(
    handoff: &mut dyn TerminalHandoff,
    seed: &str,
    editor: &str,
) -> Result<Option<String>> {
    edit_send_message_with_handoff(
        handoff,
        seed,
        &editor_temp("send"),
        editor,
        Box::new(|tmp| Ok(std::process::Command::new(editor).arg(tmp).status()?)),
    )
}

pub(crate) fn edit_send_message_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    seed: &str,
    tmp: &Path,
    editor: &str,
    run_editor: EditorCallback<'_>,
) -> Result<Option<String>> {
    let mut body = seed.trim_end().to_string();
    if !body.is_empty() {
        body.push('\n');
    }
    edit_buffer_with_handoff(
        handoff,
        tmp,
        editor,
        "send",
        &body,
        run_editor,
        tmux::sanitize_send_text,
    )
}

/// Compose a session DIRECTIVE in the user's external editor, then return the cleaned
/// directive text — the directive twin of [`edit_brief`], with the same suspend→run→restore
/// ownership of the tty (a divergence corrupts the terminal). Seeds a unique temp file with
/// [`directive_editor_seed`] (the current directive as PLAIN text — no `#`-comment guidance),
/// launches `$VISUAL`/`$EDITOR` (fallback `vi`), and parses the saved buffer through
/// [`directive_from_editor_buffer`] (keep-everything). Returns `Ok(Some(text))` for a non-empty
/// save, `Ok(None)` when the buffer was empty (KEEP the current directive — rescinding
/// is the field's `^X`, never an emptied editor), and `Err` on a spawn failure or non-zero
/// exit. The temp file (`pmtui-directive-…`, its own name so a concurrent goal/send edit can't
/// collide) is removed after every editor outcome. Tests inject only the physical terminal-mode
/// boundary while still running real editor subprocesses.
pub(crate) fn edit_directive(
    handoff: &mut dyn TerminalHandoff,
    current_directive: &str,
    editor: &str,
) -> Result<Option<String>> {
    edit_directive_with_handoff(
        handoff,
        current_directive,
        &editor_temp("directive"),
        editor,
        Box::new(|tmp| Ok(std::process::Command::new(editor).arg(tmp).status()?)),
    )
}

pub(crate) fn edit_directive_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    current_directive: &str,
    tmp: &Path,
    editor: &str,
    run_editor: EditorCallback<'_>,
) -> Result<Option<String>> {
    edit_buffer_with_handoff(
        handoff,
        tmp,
        editor,
        "directive",
        &directive_editor_seed(current_directive),
        run_editor,
        directive_from_editor_buffer,
    )
}

/// Compose a session brief in the user's external editor, then return the cleaned
/// goal text. Serves BOTH brief-edit targets — the create form's Goal field (Ctrl+E)
/// and a live session's `brief.md` (`g`) — because the fiddly part (owning and
/// restoring the tty) is identical; the caller decides what to do with the result.
/// This mirrors [`attach`]'s exact suspend→run→
/// restore of the terminal: leave raw mode + the alternate screen so the editor
/// owns the tty, run the child, then re-enter raw mode + the alternate screen and
/// clear — a divergence from that sequence would corrupt the terminal.
///
/// Seeds a unique temp file (pid + nanos) with the current goal as PLAIN text (no
/// `#`-comment guidance), launches `$VISUAL`/`$EDITOR` (fallback `vi`), and parses
/// the saved buffer back through [`brief_from_editor_buffer`] (keep-everything).
/// Returns `Ok(Some(goal))` for a non-empty save, `Ok(None)` when the buffer was
/// empty (keep the previous goal), and `Err` on a spawn failure or a
/// non-zero editor exit. The temp file is removed after every editor outcome. Tests inject only
/// the physical terminal-mode boundary while still running real editor subprocesses.
pub(crate) fn edit_brief(
    handoff: &mut dyn TerminalHandoff,
    current_goal: &str,
    editor: &str,
) -> Result<Option<String>> {
    edit_brief_with_handoff(
        handoff,
        current_goal,
        &editor_temp("brief"),
        editor,
        Box::new(|tmp| Ok(std::process::Command::new(editor).arg(tmp).status()?)),
    )
}

pub(crate) fn edit_brief_with_handoff(
    handoff: &mut dyn TerminalHandoff,
    current_goal: &str,
    tmp: &Path,
    editor: &str,
    run_editor: EditorCallback<'_>,
) -> Result<Option<String>> {
    edit_buffer_with_handoff(
        handoff,
        tmp,
        editor,
        "brief",
        &brief_editor_seed(current_goal),
        run_editor,
        brief_from_editor_buffer,
    )
}
