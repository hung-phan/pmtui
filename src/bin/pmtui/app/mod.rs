//! `App` — everything the dashboard knows between two frames, and the constructor
//! that fills it from disk. Its behaviour is split into one `impl App` block per
//! concern in the sibling modules; this file holds only the state they all share, so
//! there is exactly one place to read what a field means.

mod autopilot;
mod board;
mod create;
mod decisions;
mod enter;
mod forking;
mod lifecycle;
mod pmd;
mod prompts;
mod refresh;
mod rename;
pub(crate) mod scroll;
mod sending;
pub(crate) mod spawning;
mod switching;

use crate::*;

pub(crate) use board::{board_column, board_lane_message_applies};
pub(crate) use create::with_initial_prompt;
#[cfg(test)]
pub(crate) use create::{StandardStart, initial_launch_bytes, project_id_base};
#[cfg(test)]
pub(crate) use forking::cleanup_unstaged_fork;
#[cfg(test)]
pub(crate) use forking::start_refusal;
pub(crate) use forking::{fork_refusal, start_refused};
#[cfg(test)]
pub(crate) use refresh::session_work_summary;
#[cfg(test)]
pub(crate) use spawning::tmux_program;
pub(crate) use spawning::{SpawnBroker, job_worktree};
pub(crate) use switching::switch_matches;

pub(crate) struct App {
    pub(crate) registry_path: PathBuf,
    pub(crate) socket: String,
    pub(crate) projects: Vec<ProjectView>,
    pub(crate) selected: usize,
    pub(crate) mode: UiMode,
    /// Task inspector visibility while `mode == Board`.
    pub(crate) board_detail_open: bool,
    /// Board-native create keeps the Task view behind the form and returns to
    /// it after cancel or successful creation.
    pub(crate) return_to_board_after_create: bool,
    /// Board-native Message keeps the Task view and inspector as its shell.
    pub(crate) return_to_board_after_send: bool,
    /// Board-native Answer keeps the Task view behind the decision surface.
    pub(crate) return_to_board_after_answer: bool,
    /// Board-native quick switching keeps the Task projection behind the search
    /// and returns to it after selection or cancellation.
    pub(crate) return_to_board_after_switch: bool,
    /// Board-origin lifecycle/settings actions return to Task view after their
    /// existing confirmation or edit surface completes.
    pub(crate) return_to_board_after_action: bool,
    /// The exact stop that opened the answer overlay. Kept outside `UiMode` so existing render/test
    /// construction remains lightweight; production sets it only through `begin_answer`.
    pub(crate) answering_stop_id: Option<String>,
    /// The LATEST status line, shown on the keybar. Still a single string because ~50 sites assign it
    /// directly; the history lives in the log FILE, which [`App::record_status`] appends to.
    pub(crate) status: String,
    /// Where the status log is written — `<registry dir>/pmtui.log`, per [`status_log::log_path`].
    ///
    /// The log used to be a bounded `Vec<String>` drawn in a ` STATUS ` pane below the session list.
    /// The pane spent rows the list wanted, never appeared below `WIDE_W` columns, kept only the
    /// newest twenty lines and died with the process; the human asked for *"a better way to view the
    /// log itself"* and chose a file. A PATH on `App` rather than a global, so a scratch dashboard
    /// logs beside its own scratch registry instead of into the human's real log.
    pub(crate) status_log: PathBuf,
    /// The last line [`App::record_status`] wrote, so a status that simply persists across frames is
    /// not appended once per frame. Replaces reading the tail of the old in-memory log.
    pub(crate) last_logged: Option<String>,
    /// Where the dashboard's own preferences live — `<registry dir>/pmtui.json`.
    pub(crate) settings_path: PathBuf,
    /// The preferences in force, as last loaded or saved.
    pub(crate) settings: crate::settings::Settings,
    /// Every theme the Settings view can offer, built ONCE at startup.
    ///
    /// Fixed for the process so `UiMode::Settings`'s cursor keeps meaning the same row between the
    /// keypress that moves it and the frame that draws it — the same reason the session list commits
    /// by id rather than by index.
    pub(crate) themes: Vec<agent_manager::theme::ThemeChoice>,
    /// How far back the session detail's transcript is scrolled, in lines. `0` = the tail.
    pub(crate) detail_scroll: usize,
    pub(crate) should_quit: bool,
    /// Nonce recorded beside the held pmtui singleton lock. A matching takeover request asks this
    /// exact dashboard instance to exit cleanly; stale requests for prior owners are ignored.
    pub(crate) dashboard_owner_nonce: Option<String>,
    /// Set by the Ctrl+E handler on the Goal field; performed by `run()` (it
    /// suspends/restores the terminal around the editor).
    pub(crate) pending_brief_edit: Option<BriefEdit>,
    /// Set by `Ctrl+E` in the inline directive field (`i`); performed by `run()`, which owns
    /// the tty the editor needs — the directive twin of `pending_brief_edit`. Applied through
    /// the atomic `apply_directive_edit`, so a concurrent decider consult never reads a
    /// half-written `directive.md`.
    pub(crate) pending_directive_edit: Option<DirectiveEditReq>,
    /// Set by `Ctrl+E` in the send field (`s`); performed by `run()`, which owns the tty
    /// the editor needs. Both routes to a send — inline and editor-composed — converge on
    /// `App::deliver`, so there is one place bytes reach a pane.
    pub(crate) pending_send: Option<SendReq>,
    /// Initial Standard messages eligible for one same-process retry because the
    /// immediate create launch returned an error. Successful launches never enter this
    /// map, so a later fresh Codex terminal cannot replay launch provenance.
    pub(crate) initial_message_retries: std::collections::HashMap<String, String>,
    /// Unsent composer text, keyed by the session that owns it. Dashboard-local only:
    /// drafts survive selection changes but are never written into project state.
    pub(crate) message_drafts: std::collections::HashMap<String, Composer>,
    /// A request to drop into a real interactive `claude`/`codex` REPL on a parked
    /// agent-loop session's conversation (the on-demand chat), set by the Enter
    /// handler when the session has a `conversation_id` and no wake is in flight;
    /// drained by `run()` with the same terminal suspend/restore as `pending_attach`.
    /// `chat()` writes the per-session chat marker (so the daemon defers its wakes
    /// while attached). SURVIVE: the marker is cleared ONLY when the REPL is confirmed
    /// dead (`is_alive == Ok(false)`); on a detach (session still alive) or an unknown
    /// probe (`Err`) the marker is KEPT so the poll keeps deferring on the live session.
    pub(crate) pending_chat: Option<ChatReq>,
    /// A request to CREATE-and-chat a never-woken claude session's conversation,
    /// set by the Enter handler when it won the free `driver.lock` (pmd is not
    /// driving); drained by `run()` via `create_chat()`. Carries the held lease so
    /// creation stays fenced through the whole REPL.
    pub(crate) pending_create_chat: Option<CreateChatReq>,
    /// A request to ATTACH the daemon's ALREADY-RUNNING persistent `pmloop-` agent
    /// session (Slice 1). Set by the Enter handler when that session is alive, drained
    /// by `run()` via `attach_loop()` with the SAME terminal suspend/restore as
    /// `pending_chat` but WITHOUT launching. `attach_loop` holds `input.lock` and
    /// attach intent for the handoff so pmd cannot select/nudge during the race;
    /// pmd still owns the session's lifecycle.
    /// This supersedes the chat/create/wait routing whenever the loop session lives, so
    /// a second `claude --resume` on the same conversation can never collide with it.
    pub(crate) pending_attach_loop: Option<String>,
    /// A fork `f` asked for. It runs at the start of the next frame's input drain, after that
    /// frame drew the "forking" status, because the fork blocks the dashboard while it waits for
    /// the child's identity. `Some(true)` returns to the Task Board the key came from.
    pub(crate) pending_fork: Option<bool>,
    /// An ARMED auto-open: the session id of a never-woken session pmtui could not
    /// create itself (pmd already holds the lease, or codex, which only creates on a
    /// wake). Each `refresh` re-reads that session's ledger and, the instant it is
    /// cleanly chattable, drops the human into the resume `chat()` path (or, if it
    /// parked Blocked, disarms and points at `a`). Disarmed on navigation.
    pub(crate) pending_first_chat: Option<String>,
    /// How many more idle drains a BOUNDED arm may spend before it gives up.
    ///
    /// `None` = unbounded, which is every arm that existed before this slice (the
    /// Standard lease-keyed `Arm`, and codex — whose id is captured only after a first
    /// wake COMPLETES, so a bound there would disarm a legitimately slow wake; codex
    /// stays second-class and unregressed).
    ///
    /// `Some(n)` is set only by the Autopilot Enter, where a pinned id is a ~one-sweep
    /// event: `ensure_session` persists the id and parks `Monitoring` in ONE save. So if
    /// `n` drains pass with nothing pinned, the daemon we ensured is not actually driving
    /// (it died during boot, or it is sweeping a different registry) and the encouraging
    /// status has become a lie. Disarm with an honest one instead. Deliberately NOT a
    /// re-ensure loop — one Enter, one ensure.
    pub(crate) armed_drains_left: Option<u32>,
    /// Last max-scroll computed by the active scrolling surface: Wake, Audit, Help, or Answer.
    /// Only one is on screen at a time, so one cell serves all of them. Interior-mutable so
    /// rendering can publish real geometry for keyboard and mouse clamps. Sentinel `usize::MAX`
    /// means the first frame has not measured a ceiling yet.
    pub(crate) scroll_max: std::cell::Cell<usize>,
    /// Actual first row rendered in the Answer overlay's scrolling body. Option navigation may
    /// temporarily request cursor-follow with a sentinel, so wheel/Page handling reads this
    /// concrete position before returning to ordinary absolute scrolling.
    pub(crate) answer_scroll_top: std::cell::Cell<usize>,
    /// Last daemon-liveness observation + WHEN it was taken, reused for
    /// [`DAEMON_PROBE_TTL`]. Interior-mutable for the same reason as `scroll_max`:
    /// `render` takes `&App`, and the status bar is what needs the answer. `None` =
    /// never probed (or deliberately invalidated — see [`App::restart_daemon_watch`]).
    pub(crate) daemon_live: std::cell::Cell<Option<(std::time::Instant, DaemonLive)>>,
    /// How many CONSECUTIVE fresh `Down` samples the probe has returned. Advanced only
    /// when a probe actually runs (never by a cache hit), so it measures TIME with
    /// [`DAEMON_PROBE_TTL`] granularity rather than frames. Read by
    /// [`armed_wait_decision`]; reset by any non-`Down` sample and by
    /// [`App::restart_daemon_watch`].
    pub(crate) daemon_down_streak: std::cell::Cell<u32>,
    /// The `pmd` processes THIS pmtui spawned, kept for ONE reason: so they can be reaped.
    ///
    /// `Command::spawn` makes pmd our child, and a child that exits with nobody to `wait()`
    /// on it stays a zombie for the parent's whole life. That was invisible while pmtui only
    /// ever STARTED daemons; `r` (restart) stops one every press, so a real restart left a
    /// `[pmd] <defunct>` behind — found by `ps`, not by any test, since from inside the
    /// process a zombie is indistinguishable from a reaped child.
    ///
    /// A pmd we did NOT spawn is not our child at all (init reaps it when it dies), which is
    /// why only our own handles are tracked. [`App::reap_spawned_pmd`] uses `try_wait`, so a
    /// daemon that is still running — the normal case, since pmd outlives pmtui — is kept.
    pub(crate) spawned_pmd: std::cell::RefCell<Vec<std::process::Child>>,
    /// The tmux driver pmtui reaches a persistent agent's LIVE PANE through — for the
    /// preview's capture, and (since `s`) for one human-triggered `send_keys`.
    ///
    /// Production wires a [`TmuxDriver`] on the SAME `--socket` server as `pmd`
    /// and `attach`, so the pane pmtui shows is the one the daemon is driving.
    /// Boxed only so the render path has a seam a test can inject a fake through
    /// (`render` takes `&App`, so the driver has to live here).
    ///
    /// It was named `agent_tmux` and its doc comment promised "READ-ONLY by
    /// construction … never `send_keys`". `s` breaks that promise, so the name and the
    /// comment changed in the same commit rather than being left to lie. What is still
    /// true: pmtui NEVER calls `terminate` here, and never writes the ledger.
    pub(crate) agent_tmux: Box<dyn Driver>,
    /// Last max-scroll the DETAIL transcript rendered with, for clamping — the same interior-mutable
    /// arrangement, and the same reason, as [`App::scroll_max`].
    pub(crate) detail_max: std::cell::Cell<usize>,
    /// Where the panes were drawn on the LAST frame, so a mouse event can be routed to the one
    /// under the pointer. Interior-mutable and written by `render` (which takes `&App`) for the same
    /// reason the scroll maxima are: geometry is decided while drawing, and the run loop reads it a
    /// moment later. All-zero until the first frame, and the loop always draws before reading input.
    pub(crate) panes: std::cell::Cell<PaneRects>,
    /// Dedupe for the agent-pane FIT (see [`App::fit_agent_pane`]): the `(row id, cols, rows)`
    /// last pushed to tmux via `resize_window`. The render tick reflows the detached pane to
    /// the live preview size, but only shells out to tmux when this tuple CHANGES — so a
    /// steady dashboard forks nothing and a window-drag forks a handful. Interior-mutable for
    /// the same reason as `panes`: it is decided while drawing, from `&App`. `None` = "not
    /// fit yet / forget the last fit", which forces the next render to re-apply it.
    pub(crate) agent_pane_fit: std::cell::RefCell<Option<(String, u16, u16)>>,
    /// The working-vs-idle CONFIRMATION GATE for managed agent-loop rows, keyed by session id →
    /// `(last idle fingerprint, consecutive stable-Idle observation count)`. `refresh` classifies
    /// each live pane read-only, mirroring the daemon's drive-loop gate: a pane must classify Idle
    /// with a byte-STABLE [`tmux::idle_fingerprint`] across two ticks before the row flips to idle
    /// (`○`). A streaming answer grows the transcript ⇒ the fingerprint changes ⇒ the count resets
    /// ⇒ the row stays running (`●`). For Autopilot, a live Busy frame also overrides a completed
    /// old turn-signal baseline when the persistent terminal starts newer work without a pmd nudge.
    /// Rebuilt fresh each refresh (a row not seen this tick is simply not re-inserted), so it cannot
    /// leak entries for closed sessions.
    pub(crate) idle_gate: std::collections::HashMap<String, (u64, u32)>,
    /// Override for claude's config home, so [`App::start_undriven_session`]'s
    /// resume-vs-create probe ([`job_engine::claude_conversation_exists`]) can be pointed at
    /// a scratch `projects/` in a unit test instead of the real `$HOME/.claude`. `None` in
    /// production (the daemon's own `set_claude_home` mirror) ⇒ `CLAUDE_CONFIG_DIR` ⇒
    /// `$HOME/.claude`, the same precedence the daemon uses.
    pub(crate) claude_home: Option<PathBuf>,
    /// The canonical `pmtui` executable every terminal this dashboard launches names as
    /// `PMTUI_BIN` ([`App::managed_env`]), or `None` to omit it. Resolved once at startup
    /// from [`tmux::pmtui_bin_for_current_process`]; a unit test sets it directly.
    pub(crate) pmtui_bin: Option<PathBuf>,
    /// The PER-SESSION review watermark for the DECISION LANE (`v`): session id → the instant the
    /// human last opened THAT session's decision log, so the view can draw its "new since you last
    /// looked" divider against the right session. In-memory (resets each launch) — see the `app`
    /// `decisions` module; a durable, cross-restart watermark is a deliberate follow-up, kept off
    /// the single-writer ledger pmtui must never touch.
    pub(crate) decisions_seen: std::collections::HashMap<String, Epoch>,
    /// CLICK-to-select map for the SESSIONS list: each entry is `(absolute terminal row, index into
    /// projects)` for a row the LAST frame drew. Rebuilt by [`render_sessions`] every frame from the
    /// list's own scroll offset (so it survives scrolling), and interior-mutable for the same reason
    /// as [`App::panes`] — geometry is decided while drawing (`render` takes `&App`) and the run loop
    /// reads it a moment later. Header/blank rows map to nothing; empty until the first frame. A
    /// left-click over the sessions pane finds its row here and selects it via `move_sel`.
    pub(crate) row_hits: std::cell::RefCell<Vec<(u16, usize)>>,
    /// CLICK-to-select map for the Settings theme list, in the same `(absolute terminal row, index)`
    /// shape as [`App::row_hits`] and rebuilt from the list's own scroll offset every frame.
    ///
    /// A click SELECTS, exactly as it does in the session list; applying stays on Enter, because a
    /// pointer that repainted the whole dashboard on the way past the list would be the one thing
    /// this view is built not to do.
    pub(crate) settings_hits: std::cell::RefCell<Vec<(u16, usize)>>,
    /// Visible create-form summary rows from the last frame. Clicking one focuses
    /// that field without changing its value.
    pub(crate) create_hits: std::cell::RefCell<Vec<(Rect, usize)>>,
    /// Visible, fitted keybar chips from the last frame, carrying the exact key
    /// event their keyboard twin uses.
    pub(crate) key_hits: std::cell::RefCell<Vec<KeyHit>>,
    /// Persistent top-level Switch/Session/Task/New controls.
    pub(crate) top_hits: std::cell::RefCell<Vec<KeyHit>>,
    /// Clickable title of the selected preview. The transcript body is deliberately
    /// not an attach target.
    pub(crate) preview_attach_hit: std::cell::Cell<Rect>,
    /// Dormant composer shelf; clicking it routes through the ordinary `s` handler.
    pub(crate) composer_hit: std::cell::Cell<Rect>,
    /// Visible quick-switch result rows from the last frame. Each value is a position
    /// in the currently filtered result list and commits through the same method as Enter.
    pub(crate) switch_hits: std::cell::RefCell<Vec<(Rect, usize)>>,
    /// Visible Board cards keyed by stable session id. Rebuilt every frame so
    /// pointer selection cannot commit through a stale project index.
    pub(crate) board_hits: std::cell::RefCell<Vec<(Rect, String)>>,
    /// Visible Task columns, used to route mouse-wheel card navigation.
    pub(crate) board_column_hits: std::cell::RefCell<Vec<(Rect, BoardColumn)>>,
    /// First visible lane of the last Board frame, and each lane's first visible card. Kept like
    /// a list scroll offset: render moves a window only when the selection leaves it, so a lane
    /// or card under the pointer stays there while the wheel or a click selects inside it.
    pub(crate) board_column_start: std::cell::Cell<usize>,
    pub(crate) board_lane_offsets: std::cell::Cell<[usize; BoardColumn::ALL.len()]>,
    /// Cached PREVIEW capture — `(selected id, scroll depth, driven?, when captured, the log)` — so
    /// the autopilot rainbow can animate at ~8fps without re-forking `tmux capture-pane` on every
    /// draw. `render_preview` reuses it while the selected row, scroll AND driven-tier are unchanged
    /// (the tier decides the placeholder text, so a fresh `m` must not read the stale one) AND it is
    /// younger than `PREVIEW_CAPTURE_TTL`; otherwise it re-captures. Interior-mutable like
    /// `agent_pane_fit`: decided while drawing, from `&App`. `None` until the first preview draw.
    pub(crate) preview_capture: std::cell::RefCell<Option<PreviewCapture>>,
    /// PER-ENGINE model catalog, discovered lazily and cached — the pick list the [`ModelPicker`]
    /// offers. Filled by [`App::models_for`] the first time an engine's picker opens (one
    /// `available_models` discovery pass per engine per launch), so the render path reads it without
    /// ever doing discovery I/O. Unrecoverable misses are empty and render as "(default) only".
    pub(crate) model_catalog: std::collections::HashMap<Engine, Vec<ModelInfo>>,
    /// The spawn broker's requests in flight and its clock: see the `app` `spawning` module.
    pub(crate) spawn: SpawnBroker,
}

/// The last-drawn rectangle of each pane, so a MOUSE wheel can be routed to the pane under the
/// pointer. `Rect::ZERO` means "not on screen this frame" — the detail pane in the narrow layout —
/// and a zero rect hit-tests false, so a wheel there simply does nothing rather than scrolling a pane
/// the human cannot see.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PaneRects {
    pub(crate) sessions: Rect,
    pub(crate) detail: Rect,
}

impl PaneRects {
    /// Which pane covers `(x, y)`, if any — used to route a mouse wheel to the pane under the pointer.
    pub(crate) fn hit(&self, x: u16, y: u16) -> Option<Pane> {
        let inside = |r: &Rect| {
            r.width > 0
                && r.height > 0
                && x >= r.x
                && x < r.x.saturating_add(r.width)
                && y >= r.y
                && y < r.y.saturating_add(r.height)
        };
        if inside(&self.sessions) {
            Some(Pane::Sessions)
        } else if inside(&self.detail) {
            Some(Pane::Detail)
        } else {
            None
        }
    }
}

impl App {
    pub(crate) fn new(registry_path: PathBuf, socket: String) -> Self {
        let agent_tmux = Box::new(TmuxDriver::with_socket(&socket));
        // Derived ONCE, here, from the registry this dashboard owns: the log and the preferences
        // belong to the same directory as the state they describe, and a scratch registry therefore
        // gets a scratch log and scratch preferences.
        let status_log = status_log::log_path(&registry_path);
        let settings_path = crate::settings::settings_path(&registry_path);
        // A preferences file that will not parse must not cost the human their dashboard: the theme
        // it holds is applied by `main` (which can report the failure), and the default stands here.
        let settings = crate::settings::load(&settings_path).unwrap_or_default();
        let mut a = App {
            registry_path,
            socket,
            projects: Vec::new(),
            selected: 0,
            mode: UiMode::Normal,
            board_detail_open: false,
            return_to_board_after_create: false,
            return_to_board_after_send: false,
            return_to_board_after_answer: false,
            return_to_board_after_switch: false,
            return_to_board_after_action: false,
            answering_stop_id: None,
            status: String::new(),
            status_log,
            last_logged: None,
            settings_path,
            settings,
            themes: agent_manager::theme::available(),
            detail_scroll: 0,
            should_quit: false,
            dashboard_owner_nonce: None,
            pending_brief_edit: None,
            pending_directive_edit: None,
            pending_send: None,
            initial_message_retries: std::collections::HashMap::new(),
            message_drafts: std::collections::HashMap::new(),
            pending_chat: None,
            pending_create_chat: None,
            pending_attach_loop: None,
            pending_fork: None,
            pending_first_chat: None,
            armed_drains_left: None,
            scroll_max: std::cell::Cell::new(usize::MAX),
            answer_scroll_top: std::cell::Cell::new(0),
            daemon_live: std::cell::Cell::new(None),
            daemon_down_streak: std::cell::Cell::new(0),
            spawned_pmd: std::cell::RefCell::new(Vec::new()),
            agent_tmux,
            detail_max: std::cell::Cell::new(0),
            panes: std::cell::Cell::new(PaneRects::default()),
            agent_pane_fit: std::cell::RefCell::new(None),
            idle_gate: std::collections::HashMap::new(),
            claude_home: None,
            pmtui_bin: tmux::pmtui_bin_for_current_process(),
            decisions_seen: std::collections::HashMap::new(),
            row_hits: std::cell::RefCell::new(Vec::new()),
            settings_hits: std::cell::RefCell::new(Vec::new()),
            create_hits: std::cell::RefCell::new(Vec::new()),
            key_hits: std::cell::RefCell::new(Vec::new()),
            top_hits: std::cell::RefCell::new(Vec::new()),
            preview_attach_hit: std::cell::Cell::new(Rect::ZERO),
            composer_hit: std::cell::Cell::new(Rect::ZERO),
            switch_hits: std::cell::RefCell::new(Vec::new()),
            board_hits: std::cell::RefCell::new(Vec::new()),
            board_column_hits: std::cell::RefCell::new(Vec::new()),
            board_column_start: std::cell::Cell::new(0),
            board_lane_offsets: std::cell::Cell::new([0; BoardColumn::ALL.len()]),
            preview_capture: std::cell::RefCell::new(None),
            model_catalog: std::collections::HashMap::new(),
            spawn: SpawnBroker::default(),
        };
        a.refresh();
        a
    }

    /// Who the terminal of session `id` belongs to, handed to the agent inside it on every
    /// launch this dashboard makes: its stable id, its per-session state directory under
    /// `root`, and this pmtui.
    pub(crate) fn managed_env(&self, id: &str, root: &Path) -> tmux::ManagedEnv {
        tmux::ManagedEnv {
            session_id: id.to_string(),
            state_dir: ProjectPaths::for_session(root, id).state_dir(),
            pmtui_bin: self.pmtui_bin.clone(),
        }
    }

    /// Fit the selected agent-loop row's DETACHED pane(s) so the transcript fills its panel WIDTH on
    /// a wide terminal instead of sitting at tmux's 80-col create default. Called from `render_preview`
    /// with `cols = inner_w` and rows that include stable scroll lookahead. The pane must be taller
    /// than the window because alternate-screen TUIs have no tmux history; chunked row growth keeps
    /// deep scrolling possible without resizing for every wheel notch.
    ///
    /// Deduped against [`App::agent_pane_fit`]: the `resize_window` fork happens ONLY when the
    /// `(row, cols, rows)` changed since the last frame, so a still dashboard costs nothing and
    /// a drag costs one fork per distinct size. Both the loop and chat panes are sized (the
    /// preview shows whichever is live), which is why a single dedupe key covers the row.
    ///
    /// Deliberately does NOT `clear_history`: the preview scrolls through the pane's tmux scrollback,
    /// which `clear-history` would wipe (see the body). Skipped for a preview too small to reflow an
    /// engine into — better to leave the create size than to squeeze claude into a sliver.
    /// `resize_window` sets `window-size manual`; the attach paths restore `latest` before `Enter`,
    /// and the run loop calls [`App::forget_agent_pane_fit`] after an attach so this re-fits on return.
    pub(crate) fn fit_agent_pane(
        &self,
        id: &str,
        loop_s: &str,
        chat_s: &str,
        cols: u16,
        rows: u16,
    ) {
        if cols < 40 || rows < 8 {
            return;
        }
        let key = (id.to_string(), cols, rows);
        if self.agent_pane_fit.borrow().as_ref() == Some(&key) {
            return;
        }
        let _ = self.agent_tmux.resize_window(loop_s, cols, rows);
        if chat_s != loop_s {
            let _ = self.agent_tmux.resize_window(chat_s, cols, rows);
        }
        // Captures are geometry-dependent. A taller pane can expose older alternate-screen rows,
        // so the next read must not reuse bytes captured before this resize.
        *self.preview_capture.borrow_mut() = None;
        // NO `clear_history`. The preview scrolls through the pane's tmux SCROLLBACK (`capture-pane
        // -S`), and `clear-history` wipes exactly that, collapsing `detail_max` to ~0 — so clearing
        // on a fit left the row UNSCROLLABLE (user: *"i cannot scroll ... all the way to the top"* /
        // *"we can ignore the resize if it breaks scrolling"*). A resize PRESERVES scrollback
        // (verified against real tmux: `capture -S -600` is unchanged by `resize-window`, gutted only
        // by `clear-history`), so a reflow needs no repaint here. The cosmetic stale frame a resize
        // can stack ABOVE the transcript is the accepted trade for a preview that scrolls. Chunked
        // height growth means a steady dashboard reflows only on a real width/scroll-boundary
        // change, so those are rare anyway.
        *self.agent_pane_fit.borrow_mut() = Some(key);
    }

    /// Forget the last applied pane fit so the next render re-applies it. Called after any
    /// attach: tmux resized the pane to the human's real terminal on `Enter` (`window-size
    /// latest`), so without this the dedupe would think the pane is still at the preview size
    /// and leave it oversized — clipping the transcript — until the next real size change.
    pub(crate) fn forget_agent_pane_fit(&self) {
        *self.agent_pane_fit.borrow_mut() = None;
    }

    /// Open the Settings view on its first row, with every dropdown closed.
    pub(crate) fn open_settings(&mut self) {
        self.mode = UiMode::Settings {
            cursor: 0,
            open: None,
        };
    }

    /// Leave Settings for the session list, applying nothing.
    pub(crate) fn close_settings(&mut self) {
        self.mode = UiMode::Normal;
    }

    /// One setting, resolved for the frame (or the keypress) that needs it.
    ///
    /// Built on demand rather than cached: it is one row's worth of work, and a cached copy is how a
    /// settings page starts showing a value the process no longer has.
    pub(crate) fn setting_row(
        &self,
        kind: crate::settings::SettingKind,
    ) -> crate::settings::SettingRow {
        use crate::settings::{SettingKind, SettingOption, SettingRow};
        match kind {
            SettingKind::Theme => {
                let active = agent_manager::theme::active();
                let options: Vec<SettingOption> = self
                    .themes
                    .iter()
                    .map(|choice| SettingOption {
                        label: choice.display.clone(),
                        note: choice.variant.to_string(),
                        value: choice.id.clone(),
                    })
                    .collect();
                let current = options.iter().position(|option| option.value == active);
                SettingRow {
                    // The DISPLAY name, because that is what the dropdown offers; the id lives in the
                    // file, which the view names at its foot.
                    value: current.map_or_else(
                        || format!("{active} (unknown)"),
                        |i| options[i].label.clone(),
                    ),
                    current,
                    options,
                }
            }
        }
    }

    /// The setting the cursor is on, while the Settings view is open.
    fn settings_kind(&self) -> Option<crate::settings::SettingKind> {
        let UiMode::Settings { cursor, .. } = &self.mode else {
            return None;
        };
        crate::settings::SettingKind::ALL.get(*cursor).copied()
    }

    /// The last index the LIVE cursor may take: the open dropdown's options, else the settings.
    fn settings_last_index(&self) -> usize {
        let open = matches!(self.mode, UiMode::Settings { open: Some(_), .. });
        match (open, self.settings_kind()) {
            (true, Some(kind)) => self.setting_row(kind).options.len().saturating_sub(1),
            _ => crate::settings::SettingKind::ALL.len().saturating_sub(1),
        }
    }

    /// Put the live Settings cursor — the open dropdown's, else the setting's — on `index`, clamped.
    /// The pointer's way in.
    pub(crate) fn settings_select(&mut self, index: usize) {
        let last = self.settings_last_index();
        let UiMode::Settings { cursor, open } = &mut self.mode else {
            return;
        };
        *open.as_mut().unwrap_or(cursor) = index.min(last);
    }

    /// Drop the selected setting's dropdown, starting on the value in force.
    ///
    /// Opening ON the current value is what makes the dropdown answer "what is set?" before it offers
    /// to change it, and it is why opening and closing one changes nothing.
    pub(crate) fn settings_open(&mut self) {
        let Some(kind) = self.settings_kind() else {
            return;
        };
        let start = self.setting_row(kind).current.unwrap_or(0);
        if let UiMode::Settings { open, .. } = &mut self.mode {
            *open = Some(start);
        }
    }

    /// Close the dropdown, applying nothing. The cursor stays on its setting.
    pub(crate) fn settings_close(&mut self) {
        if let UiMode::Settings { open, .. } = &mut self.mode {
            *open = None;
        }
    }

    /// Move whichever Settings cursor is live by `delta`, clamped.
    ///
    /// Clamped rather than wrapped, and it applies NOTHING: the value changes when the human presses
    /// Enter, so walking a dropdown past thirty-eight themes never repaints the screen out from under
    /// them.
    pub(crate) fn settings_move(&mut self, delta: isize) {
        let last = self.settings_last_index();
        let UiMode::Settings { cursor, open } = &mut self.mode else {
            return;
        };
        let live = open.as_mut().unwrap_or(cursor);
        let next = (*live as isize)
            .saturating_add(delta)
            .clamp(0, last as isize);
        *live = next as usize;
    }

    /// Apply the option under the open dropdown's cursor, then close the dropdown.
    ///
    /// A no-op when no dropdown is open, which is what keeps `Enter` on a settings ROW from committing
    /// a value the human has not been shown.
    pub(crate) fn settings_commit(&mut self) {
        let (
            UiMode::Settings {
                open: Some(pick), ..
            },
            Some(kind),
        ) = (&self.mode, self.settings_kind())
        else {
            return;
        };
        let row = self.setting_row(kind);
        let Some(option) = row.options.get(*pick).cloned() else {
            return;
        };
        self.commit_setting(kind, &option);
        self.settings_close();
    }

    /// Put one setting's chosen value into force and remember it.
    ///
    /// Applies FIRST and saves second, and reports either failure: a value that cannot be applied must
    /// not be written to the preferences file, or the next start would fail the same way with nothing
    /// on screen saying why.
    fn commit_setting(
        &mut self,
        kind: crate::settings::SettingKind,
        option: &crate::settings::SettingOption,
    ) {
        use crate::settings::SettingKind;
        match kind {
            SettingKind::Theme => {
                if let Err(e) = agent_manager::theme::apply(&option.value) {
                    self.status = format!("could not use theme {}: {e:#}", option.value);
                    return;
                }
                self.settings.theme = option.value.clone();
            }
        }
        let name = kind.name().to_lowercase();
        match crate::settings::save(&self.settings_path, &self.settings) {
            Ok(()) => self.status = format!("{name} → {}", option.label),
            // The value IS in force; only remembering it failed, and saying so is the difference
            // between "it did not work" and "it will not survive a restart".
            Err(e) => {
                self.status = format!(
                    "{name} → {} for now; could not save it ({e:#})",
                    option.label
                );
            }
        }
    }

    /// Append the current [`App::status`] to the log FILE, if it says something new.
    ///
    /// Called from ONE place per input path — the end of `handle_key`, and the run loop after a
    /// refresh — rather than from the ~50 sites that assign `status`. That means the log records what
    /// the human actually SAW: the status as it stood when the frame was drawn, not every intermediate
    /// value a handler passed through.
    ///
    /// De-duplicates against the line last written, so a status that simply persists across frames —
    /// or a refused key held down — is one line rather than one per frame. A status that genuinely
    /// recurs later still gets its own line, with its own timestamp.
    pub(crate) fn record_status(&mut self) {
        if self.status.is_empty() || self.last_logged.as_deref() == Some(self.status.as_str()) {
            return;
        }
        self.last_logged = Some(self.status.clone());
        status_log::append(&self.status_log, &self.status);
    }

    /// Append a line to the status log that is NOT the status on screen — for the handful of events
    /// worth a record even though nothing shows them (the spawn broker's notes).
    ///
    /// Takes `&self`: the log is a file now, so noting something changes no dashboard state. It does
    /// not touch `last_logged` either, because that tracks the STATUS line and these are asides.
    pub(crate) fn log_line(&self, line: &str) {
        status_log::append(&self.status_log, line);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyHit {
    pub(crate) area: Rect,
    pub(crate) code: KeyCode,
    pub(crate) modifiers: KeyModifiers,
}

impl KeyHit {
    pub(crate) fn contains(&self, x: u16, y: u16) -> bool {
        rect_contains(self.area, x, y)
    }
}

pub(crate) fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    area.width > 0
        && area.height > 0
        && x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}
