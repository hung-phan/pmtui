//! `pmtui` — a simple, functional supervisory dashboard over the projects `pmd`
//! drives. It is **file-based**: it reads each project's `.project-state/`
//! directly and records actions by writing files (answer -> answers.json,
//! remove -> registry, tier -> config.json), so it needs no socket and works
//! whether or not the daemon is running.
//!
//! Daemon lifecycle is the exception to file-only actions. Because `Tier::Autopilot`
//! *is* the "let pmd drive this" switch, pmtui SPAWNS a detached `pmd` process
//! when one is missing — at startup if an enabled session is already on Autopilot
//! (`ensure_daemon_for_enabled_autopilot`), on a `m` press that turns Autopilot ON for
//! a mode pmd actually drives (`cycle_tier` -> `ensure_daemon`), and on a create whose
//! Autonomy dial is already Autopilot (`submit_create`). Before the Standard→Autopilot
//! edge it also recycles a stale pmd so the replacement can install its embedded worker
//! skill before the tier is published. Project terminals survive that recycle. Everything
//! else still only reads and writes files, and pmtui NEVER writes a ledger.
//!
//! `m` is the ONE autonomy switch: ON hands the session to pmd, OFF means pmd does not
//! drive it at all (`agent_manager::daemon::pmd_drives_row`) and the human drives it by
//! attaching with Enter. `p` is the other off switch, and a blunter one: it stops autopilot
//! AND ends the agent's session, so nothing is left running. Enter on a paused row resumes
//! it and `m` turns autopilot back on. The difference is deliberate — `m` parks the driving
//! and leaves the conversation alive, `p` puts the whole session down.
//!
//! One screen: header counts, a project list (attention first), a detail pane
//! for the selection, and an adaptive footer. `s` on an open stop opens an answer overlay.
//!
//! The session GOAL (`brief.md`, which the loop's nudge re-reads every heartbeat) is editable
//! at any time by ONE key, mirroring the create form's Goal field: `g` opens a one-line INLINE
//! field for a quick re-aim, and `Ctrl+E` inside it escalates to `$VISUAL`/`$EDITOR` for a real
//! multi-paragraph brief. Both routes write that one file through the one atomic
//! [`apply_goal_edit`]; an empty save keeps the previous goal either way. `g` is AUTOPILOT-only,
//! because the nudge prompt is the only reader of `brief.md` and it does not run on a row pmd
//! does not drive.
//!
//! The footer is adaptive in two senses (see [`keybar_line`]): by WIDTH — full
//! `key + label` chips on a wide terminal, key-only badges in a mid-sized tmux split,
//! and a bare `?`/`q` minimum on a narrow one — and by CONTEXT, offering only the
//! actions that apply to the selected row. Because that HIDES keys, `?` opens a help
//! overlay listing every binding. Both surfaces read one table, [`BINDINGS`], so they
//! cannot disagree.
//!
//! ONE MODULE PER CONCERN, so `ls src/bin/pmtui` reads as an index. `app` is `App` —
//! every fact the dashboard holds between two frames, plus the action methods that change
//! them; `keys` is the only place a keypress or a paste is dispatched; `mode` is the
//! [`UiMode`] state machine both of those switch on; `render` draws, one file per pane and
//! per overlay; `bindings` is the ONE key table the keybar and `?` both read; `display`
//! turns a value into the text, glyph or colour that lands on screen; `decide` holds the
//! PURE routing rules behind Enter and the armed auto-open; `create_form` is the `n` form's
//! fields and the rules that move around them; `seed` writes the on-disk state a create
//! brings into existence; `edit` writes the two settings a live session can change (its
//! goal and its cadence); `send` decides whether one press of `s` may type into a pane;
//! `daemonctl` is what pmtui knows and says about the `pmd` process; `session` owns
//! every action that suspends the TUI to hand the real terminal to a child (attach, chat,
//! `$EDITOR`), because a divergence from that sequence corrupts the terminal; and `spawn_cli`
//! is the `pmtui spawn` subcommand, which never opens the dashboard at all.
//!
//! Usage: pmtui [--registry <path>] [--socket <name>]
//!        pmtui spawn --message <text> [options]
//!
//! The command line is parsed into an [`Action`] before anything else happens, so `spawn`
//! runs without terminal setup, the dashboard singleton, the registry, or tmux.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::Frame;
use ratatui::crossterm::ExecutableCommand;
#[cfg(not(test))]
use ratatui::crossterm::event;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
#[cfg(not(test))]
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};

use agent_manager::advise;
use agent_manager::attention;
use agent_manager::chat_lock;
use agent_manager::clock::{Clock, Epoch, SystemClock};
use agent_manager::daemon;
use agent_manager::job::{self, AgentLoopState};
use agent_manager::job_engine;
use agent_manager::lease::{self, ProjectLease};
use agent_manager::models::{ModelInfo, available_models};
use agent_manager::policy;
use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
// `stop_product_text` comes from the library, shared with
// `escalation::Escalation::for_stops` (the desktop notification), so the three surfaces
// that show a stop to a human cannot drift apart. It lived in THIS file until a second
// binary needed it.
use agent_manager::state::{
    self, Answer, Config, DriverState, ExitReason, ProjectPaths, RiskClass, Stop, Tier,
    stop_product_text,
};
use agent_manager::tmux::{self, Driver, TmuxDriver, session_name};
use agent_manager::view::{Posture, ProjectView, next_tier, prev_tier};
// The dashboard is one binary split by concern. `crate::` IS this file, so each module
// reaches its siblings through the re-exports below rather than through a path — which is
// also what keeps every unqualified name (and the test module's `use crate::*`) resolving
// exactly where it did when all of this lived in one file.
mod app;
mod bindings;
mod composer;
mod create_form;
mod daemonctl;
mod decide;
mod display;
mod edit;
mod event_dispatch;
mod input;
mod keys;
mod mode;
mod path_complete;
mod render;
mod seed;
mod send;
mod session;
mod settings;
mod spawn_cli;
mod spawn_status;
mod status_log;
mod takeover;

pub(crate) use crate::app::*;
pub(crate) use crate::bindings::*;
pub(crate) use crate::composer::*;
pub(crate) use crate::create_form::*;
pub(crate) use crate::daemonctl::*;
pub(crate) use crate::decide::*;
pub(crate) use crate::display::*;
pub(crate) use crate::edit::*;
pub(crate) use crate::event_dispatch::*;
pub(crate) use crate::input::*;
pub(crate) use crate::keys::*;
pub(crate) use crate::mode::*;
pub(crate) use crate::render::*;
pub(crate) use crate::seed::*;
pub(crate) use crate::send::*;
pub(crate) use crate::session::*;
pub(crate) use crate::takeover::*;

fn default_registry_path() -> PathBuf {
    registry_path_for_home(std::env::var_os("HOME").map(PathBuf::from))
}

fn registry_path_for_home(home: Option<PathBuf>) -> PathBuf {
    home.unwrap_or_else(|| PathBuf::from("."))
        .join(".config/pmd/registry.json")
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    registry: PathBuf,
    socket: String,
    help: bool,
}

fn parse_args_from(argv: &[String]) -> Result<Args> {
    let mut out = Args {
        registry: default_registry_path(),
        socket: "pmd".to_string(),
        help: false,
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--registry" => {
                let value = argv
                    .get(i + 1)
                    .with_context(|| "--registry needs a value")?;
                out.registry = PathBuf::from(value);
                i += 2;
            }
            "--socket" => {
                out.socket = argv
                    .get(i + 1)
                    .cloned()
                    .with_context(|| "--socket needs a value")?;
                i += 2;
            }
            "-h" | "--help" => {
                out.help = true;
                i += 1;
            }
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }
    Ok(out)
}

/// What one `pmtui` command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Dashboard(Args),
    Spawn(spawn_cli::SpawnCli),
    Help,
    SpawnHelp,
}

/// Parse a whole command line. `spawn` is recognized first and parsed on its own, so a spawn
/// never resolves the dashboard's defaults (the registry path under `$HOME`).
fn parse_action(argv: &[String]) -> std::result::Result<Action, spawn_cli::UsageError> {
    if argv.first().is_some_and(|arg| arg == "spawn") {
        return spawn_cli::parse(&argv[1..])
            .map(|parsed| parsed.map_or(Action::SpawnHelp, Action::Spawn));
    }
    match parse_args_from(argv) {
        Ok(args) if args.help => Ok(Action::Help),
        Ok(args) => Ok(Action::Dashboard(args)),
        Err(error) => Err(spawn_cli::UsageError {
            message: error.to_string(),
            json: spawn_cli::wants_json(argv),
        }),
    }
}

const HELP: &str = concat!(
    "pmtui — agent-manager dashboard\n\n",
    "Usage: pmtui [--registry <path>] [--socket <name>]\n",
    "       pmtui spawn --message <text> [options]\n\n",
    "--registry <path>  registry JSON (default: ~/.config/pmd/registry.json)\n",
    "--socket <name>    private tmux server socket (default: pmd)\n",
    "spawn              from inside a managed session, ask the dashboard for a Standard child\n",
    "                   session (see `pmtui spawn --help`)\n",
);

fn print_help(out: &mut dyn Write) {
    emit(out, HELP);
}

/// Write one piece of output and flush it. A closed stream is ignored: the exit code still
/// carries the outcome, and a spawn's request and receipt are already on disk.
fn emit(out: &mut dyn Write, text: &str) {
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Run a parsed command line and return its exit code. The dashboard is `dashboard`; every
/// other action finishes here, before any terminal setup.
fn dispatch(
    parsed: std::result::Result<Action, spawn_cli::UsageError>,
    dashboard: &mut dyn FnMut(Args) -> Result<()>,
    spawn: &mut spawn_cli::SpawnDeps<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32> {
    match parsed {
        Ok(Action::Dashboard(args)) => {
            dashboard(args)?;
            Ok(0)
        }
        // A LISTING, not an outcome: it exits 0 whenever it could read the session's own directory, and
        // what to do next is in the lines themselves.
        Ok(Action::Spawn(cli)) if cli.status => match spawn_status::run_status(spawn.env) {
            Ok(children) => {
                let text = if cli.json {
                    spawn_status::render_status_json(&children)
                } else {
                    spawn_status::render_status_plain(&children)
                };
                emit(out, &format!("{text}\n"));
                Ok(0)
            }
            Err(message) => {
                emit(err, &format!("spawn --status: {message}\n"));
                Ok(1)
            }
        },
        Ok(Action::Spawn(cli)) => {
            let output = spawn_cli::run_spawn(&cli, spawn.env, spawn.clock, &mut *spawn.waiter);
            let text = if cli.json {
                spawn_cli::render_json(&output)
            } else {
                spawn_cli::render_plain(&output)
            };
            emit(out, &format!("{text}\n"));
            Ok(spawn_cli::exit_code(&output))
        }
        Ok(Action::Help) => {
            print_help(err);
            Ok(0)
        }
        Ok(Action::SpawnHelp) => {
            emit(err, spawn_cli::HELP);
            Ok(0)
        }
        Err(usage) if usage.json => {
            let output = spawn_cli::CliOutput::usage(&usage);
            emit(out, &format!("{}\n", spawn_cli::render_json(&output)));
            Ok(spawn_cli::exit_code(&output))
        }
        Err(usage) => {
            emit(
                err,
                &format!(
                    "pmtui: {}\nsee `pmtui --help` or `pmtui spawn --help`\n",
                    usage.message
                ),
            );
            Ok(2)
        }
    }
}

#[cfg(test)]
fn drain_pending_chat(app: &mut App, launch: impl FnOnce(&ChatReq) -> Result<bool>) {
    let Some(req) = app.pending_chat.take() else {
        return;
    };
    finish_pending_chat(app, &req.label, launch(&req));
}

fn finish_pending_chat(app: &mut App, label: &str, result: Result<bool>) {
    if result.is_ok() {
        app.initial_message_retries.remove(label);
    }
    app.status = match result {
        Ok(live) => chat_return_status(label, live),
        Err(e) => format!("chat ended: {e}"),
    };
    app.forget_agent_pane_fit();
}

#[cfg(test)]
fn drain_pending_create_chat(app: &mut App, launch: impl FnOnce(CreateChatReq) -> Result<bool>) {
    let Some(req) = app.pending_create_chat.take() else {
        return;
    };
    let label = req.label.clone();
    finish_pending_create_chat(app, &label, launch(req));
}

fn finish_pending_create_chat(app: &mut App, label: &str, result: Result<bool>) {
    if result.is_ok() {
        app.initial_message_retries.remove(label);
    }
    app.status = match result {
        Ok(live) => format!("created {label}; {}", chat_park_note(live)),
        Err(e) => format!("create chat ended: {e}"),
    };
    app.forget_agent_pane_fit();
}

#[cfg(test)]
fn drain_pending_attach(
    app: &mut App,
    attach: impl FnOnce(&ProjectPaths, &str, &str) -> Result<()>,
) {
    let Some(session) = app.pending_attach_loop.take() else {
        return;
    };
    let (label, attach_paths) = pending_attach_context(app, &session);
    let result = match attach_paths {
        Some(paths) => Some(attach(&paths, &app.socket, &session)),
        None => None,
    };
    finish_pending_attach(app, &label, result);
}

fn pending_attach_context(app: &App, session: &str) -> (String, Option<ProjectPaths>) {
    let label = match app.selected_view() {
        Some(view) => view.id.clone(),
        None => session.to_string(),
    };
    let attach_paths = match Registry::load(&app.registry_path) {
        Ok(registry) => registry
            .projects
            .iter()
            .find(|entry| entry.id == label)
            .map(entry_state_paths),
        Err(_) => None,
    };
    (label, attach_paths)
}

fn finish_pending_attach(app: &mut App, label: &str, result: Option<Result<()>>) {
    app.status = match result {
        Some(result) => match result {
            Ok(()) => format!("detached from {label} (still running)"),
            Err(e) => format!("attach failed: {e}"),
        },
        None => format!("attach failed: {label} is gone from the registry"),
    };
    app.forget_agent_pane_fit();
}

#[cfg(test)]
fn drain_pending_brief_edit(app: &mut App, edit: impl FnOnce(&str) -> Result<Option<String>>) {
    let Some(req) = app.pending_brief_edit.take() else {
        return;
    };
    let edited = edit(&req.goal);
    finish_pending_brief_edit(app, req, edited);
}

fn finish_pending_brief_edit(app: &mut App, req: BriefEdit, edited: Result<Option<String>>) {
    match req.target {
        BriefEditTarget::CreateForm => match edited {
            Ok(Some(goal)) => {
                let n = goal.lines().count();
                if let UiMode::Creating(form) = &mut app.mode {
                    // Caret at the end of the composed goal, ready to extend inline.
                    form.goal = Field::from(goal);
                }
                app.status = if n > 1 {
                    format!("brief updated ({n} lines)")
                } else {
                    "brief updated".into()
                };
            }
            Ok(None) => app.status = "brief unchanged (empty save keeps it)".into(),
            Err(e) => app.status = format!("brief editor failed: {e}"),
        },
        BriefEditTarget::Session {
            id,
            brief,
            then_autopilot,
        } => {
            // `ok` tracks whether a goal is actually ON DISK now, which is the only condition under
            // which the pending autopilot flip may proceed.
            let ok;
            app.status = match edited {
                Ok(Some(goal)) => {
                    let outcome = apply_goal_edit(&brief, &goal);
                    ok = outcome.is_ok();
                    app.goal_edit_status(&id, outcome)
                }
                Ok(None) => {
                    ok = !req.goal.trim().is_empty();
                    format!("{id} goal unchanged (empty/all-comments)")
                }
                Err(e) => {
                    ok = false;
                    format!("{id} goal editor failed: {e}")
                }
            };
            if then_autopilot {
                app.status = if ok {
                    format!("{}; goal via $EDITOR", app.turn_autopilot_on(&id))
                } else {
                    format!("{} — autopilot NOT turned on", app.status)
                };
                app.refresh();
            }
            app.finish_board_action();
        }
    }
}

#[cfg(test)]
fn drain_pending_directive_edit(app: &mut App, edit: impl FnOnce(&str) -> Result<Option<String>>) {
    let Some(req) = app.pending_directive_edit.take() else {
        return;
    };
    let edited = edit(&req.current);
    finish_pending_directive_edit(app, req, edited);
}

fn finish_pending_directive_edit(
    app: &mut App,
    req: DirectiveEditReq,
    edited: Result<Option<String>>,
) {
    app.status = match edited {
        Ok(Some(text)) => {
            app.directive_edit_status(&req.id, apply_directive_edit(&req.directive, &text))
        }
        Ok(None) => format!(
            "{} directive unchanged (empty save keeps it — press ^X to rescind)",
            req.id
        ),
        Err(e) => format!("{} directive editor failed: {e}", req.id),
    };
}

#[cfg(test)]
fn drain_pending_send(app: &mut App, edit: impl FnOnce(&str) -> Result<Option<String>>) {
    let Some(req) = app.pending_send.take() else {
        return;
    };
    let edited = edit(&req.seed);
    finish_pending_send(app, req, edited);
}

/// The inline draft as it was when `^X^E` was pressed, caret and all.
///
/// The caret is restored because the human did not lose their place by opening an editor — and it is
/// `(row, column)` rather than one offset because the composer is multi-line now.
fn parked_draft(req: &SendReq) -> Composer {
    let mut draft = Composer::from_text(req.seed.clone());
    draft.set_cursor(req.cursor.0, req.cursor.1);
    draft
}

fn finish_pending_send(app: &mut App, req: SendReq, edited: Result<Option<String>>) {
    match edited {
        Ok(Some(text)) => {
            // Into the FIELD, not straight down the wire: the same `submit_send` gate then runs.
            app.mode = UiMode::Sending {
                target: req.target,
                input: Composer::from_text(text),
            };
            app.submit_send();
        }
        Ok(None) => {
            let kept = !req.seed.is_empty();
            let draft = parked_draft(&req);
            app.restore_send_draft(req.target.id, draft);
            app.status = if kept {
                "nothing sent; draft kept".into()
            } else {
                "nothing to send (empty buffer)".into()
            };
            app.return_to_board_after_send = false;
        }
        Err(e) => {
            let kept = !req.seed.is_empty();
            let draft = parked_draft(&req);
            app.restore_send_draft(req.target.id, draft);
            app.status = if kept {
                format!("send editor failed: {e}; draft kept")
            } else {
                format!("send editor failed: {e}")
            };
            app.return_to_board_after_send = false;
        }
    }
}

fn finish_frame(app: &mut App, handled_key: bool, last_refresh: &mut std::time::Instant) {
    // Before the status is logged below, so a spawn this frame finished is recorded with it.
    app.step_spawns();
    if handled_key {
        app.after_input(true);
    } else if last_refresh.elapsed().as_millis() >= REFRESH_MS {
        app.after_input(false);
        *last_refresh = std::time::Instant::now();
    }
}

fn set_terminal_input_modes(output: &mut impl Write, enabled: bool) {
    if enabled {
        let _ = output.execute(EnableBracketedPaste);
        let _ = output.execute(EnableMouseCapture);
    } else {
        let _ = output.execute(DisableMouseCapture);
        let _ = output.execute(DisableBracketedPaste);
    }
}

fn finish_terminal(
    output: &mut impl Write,
    restore: impl FnOnce(),
    result: Result<()>,
) -> Result<()> {
    set_terminal_input_modes(output, false);
    restore();
    result
}

#[cfg(test)]
fn run_loop(app: &mut App, mut frame: impl FnMut(&mut App) -> Result<bool>) -> Result<()> {
    // Last time the EXPENSIVE refresh ran, so it can be throttled independently of the redraw: the
    // rainbow animates by redrawing fast, but `refresh()` (tmux has-session per project + disk) must
    // NOT run every animation frame. `Instant::now()` is a monotonic clock read (not the wall clock).
    let mut last_refresh = std::time::Instant::now();
    loop {
        let handled_key = frame(app)?;
        if app.should_quit {
            break;
        }
        finish_frame(app, handled_key, &mut last_refresh);
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(not(test))]
fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_refresh = std::time::Instant::now();
    loop {
        terminal.draw(|f| render(f, app))?;
        // Handle at most one key per iteration. Crucially, do NOT refresh() on every
        // iteration: event::poll returns immediately while input is queued, so a
        // per-iteration refresh would shell `tmux has-session` per interactive
        // project on every keystroke (a subprocess storm while typing/pasting).
        //
        // Poll on a SHORT frame interval while a row is on autopilot, so the rainbow id flows; the
        // calm half-second otherwise. Only the cheap redraw at the top of the loop speeds up — the
        // preview capture is cached and the refresh is throttled below, so a fast frame forks nothing.
        let handled_key = drain_events(app, event::poll, event::read)?;
        // Launch or attach the unified terminal. Ctrl+q detaches while the process
        // keeps running; attach intent is cleared on return so pmd may resume.
        // Terminal handoffs are drained as one operation so every child follows the same
        // suspend/restore protocol and the event loop itself owns no per-action terminal logic.
        let editor = editor_command();
        let mut handoff = RealTerminalHandoff { terminal };
        drain_terminal_requests(app, &mut handoff, &editor);
        if app.should_quit {
            break;
        }
        finish_frame(app, handled_key, &mut last_refresh);
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn start_app(args: Args, dashboard: &mut dyn FnMut(&mut App) -> Result<()>) -> Result<()> {
    start_app_with_takeover(
        args,
        dashboard,
        &mut takeover::confirm_terminal,
        &mut takeover::force_owner_and_wait,
    )
}

fn start_app_with_takeover(
    args: Args,
    dashboard: &mut dyn FnMut(&mut App) -> Result<()>,
    confirm: &mut dyn FnMut(&str) -> Result<bool>,
    force: &mut ForceTakeover<'_>,
) -> Result<()> {
    if args.help {
        print_help(&mut std::io::stderr());
        return Ok(());
    }
    let registry = args.registry;
    let socket = args.socket;
    // SINGLE INSTANCE per (registry, socket). Two dashboards on one registry race its
    // read-modify-write edits (a lost create/pause is silent) and both drive the same tmux
    // sessions — so a second launch either refuses or completes a confirmed lock handoff rather
    // than starting "a random one". Held for life; the OS drops the flock on exit/crash, so a
    // stale lock never blocks the next run.
    // Taken BEFORE `ratatui::init()` so the refusal prints to a normal terminal, not a
    // half-initialised alt-screen. A different `--socket`/`--registry` is a different lock,
    // which is the supported way to run more than one (see `pmtui_lock_path`).
    let claim = takeover::acquire_dashboard(&registry, &socket, confirm, force)?;
    if !claim.proceed {
        return Ok(());
    }
    let singleton = claim.ownership;
    let mut app = App::new(registry, socket);
    app.dashboard_owner_nonce = singleton
        .as_ref()
        .map(|ownership| ownership.owner.nonce.clone());
    // The saved theme, BEFORE the first frame, so the dashboard never flashes the default on its way
    // to the human's choice. A theme that will not load is reported rather than swallowed: the default
    // is already in force, and the status names the id that was refused — the alternative is a
    // preferences file that looks applied and is not.
    if let Err(e) = agent_manager::theme::apply(&app.settings.theme) {
        app.status = format!("{e:#}");
    }
    // "Autopilot on" has to mean "it runs", including across a pmd crash or a reboot.
    // Do this BEFORE taking over the screen: it is a lock probe plus at most one
    // detached spawn (both fast, both non-fatal), and running it pre-`init` keeps any
    // stray child output away from the live dashboard.
    app.ensure_daemon_for_enabled_autopilot();
    dashboard(&mut app)
}

#[cfg(not(test))]
fn main() -> Result<()> {
    let argv = std::env::args().skip(1).collect::<Vec<_>>();
    let mut waiter = spawn_cli::ThreadWaiter::new();
    let mut spawn = spawn_cli::SpawnDeps {
        env: &spawn_cli::process_env,
        clock: &SystemClock,
        waiter: &mut waiter,
    };
    let code = dispatch(
        parse_action(&argv),
        &mut |args| start_app(args, &mut run_dashboard),
        &mut spawn,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )?;
    std::process::exit(code)
}

#[cfg(not(test))]
fn run_dashboard(app: &mut App) -> Result<()> {
    let mut terminal = ratatui::init();
    // PANIC-PATH PARITY. `ratatui::init()` installs a panic hook, but it only disables raw mode and
    // leaves the alternate screen — it knows nothing about the mouse-capture and bracketed-paste
    // modes enabled just below (they live outside ratatui's managed state). So a panic in
    // `run()`/`render()` used to drop the human to a shell still emitting mouse-tracking bytes on
    // every pointer move and wrapping pastes in `\e[200~`/`\e[201~`, needing a manual `reset`.
    // Chain a hook that disables BOTH, then delegates to ratatui's — the panic path now restores
    // exactly what the normal exit does (`DisableMouseCapture` + `DisableBracketedPaste` +
    // `ratatui::restore()`). A hook, not a `Drop` guard, because it also fires under `panic=abort`.
    let _ = PREVIOUS_PANIC_HOOK.set(std::panic::take_hook());
    std::panic::set_hook(Box::new(terminal_panic_hook));
    // BRACKETED PASTE, and it is a bug fix rather than a nicety.
    //
    // Without it a pasted block arrives as individual key events: the first newline
    // becomes `KeyCode::Enter`, which SUBMITS the field and returns to Normal mode — and
    // every remaining character is then read as a Normal-mode BINDING. `d` opens the
    // remove-confirm, `q` quits pmtui, `m` flips autopilot, and a literal ESC byte
    // cancels the overlay. That has been true of `a` and `G` since they existed, and `s`
    // (a field whose whole purpose is text you did not type by hand) would have made it
    // routine. `bracketed-paste` is a DEFAULT crossterm feature — no new dependency.
    //
    // Best-effort on purpose: a terminal that refuses the mode still gets today's
    // behaviour, and losing the paste mode is never worth refusing to start.
    // MOUSE CAPTURE, for wheel-to-scroll. Best-effort like the paste mode above: a terminal that
    // refuses it just leaves the mouse doing whatever it did before. (The session handoffs in
    // `session.rs` re-assert this on return so the wheel stays smooth after a detach.)
    //
    // The cost, and it is worth knowing: while this is on, the terminal's own click-drag text
    // selection is taken over by the application. Most terminals still offer it under Shift.
    set_terminal_input_modes(&mut std::io::stdout(), true);
    let res = run(&mut terminal, app);
    finish_terminal(&mut std::io::stdout(), ratatui::restore, res)
}

#[cfg(not(test))]
type PanicHook = Box<dyn for<'a> Fn(&std::panic::PanicHookInfo<'a>) + Send + Sync + 'static>;

#[cfg(not(test))]
static PREVIOUS_PANIC_HOOK: std::sync::OnceLock<PanicHook> = std::sync::OnceLock::new();

#[cfg(not(test))]
fn terminal_panic_hook(info: &std::panic::PanicHookInfo<'_>) {
    set_terminal_input_modes(&mut std::io::stdout(), false);
    PREVIOUS_PANIC_HOOK
        .get()
        .expect("panic hook installed before dashboard run")(info);
}

#[cfg(test)]
mod tests;
