//! What the dashboard is doing right now: the [`UiMode`] state machine every key
//! handler and the renderer switch on, plus the two small values its arms carry —
//! what a confirm is about, and a queued external-editor brief edit.

use crate::*;

/// WHICH per-session model the [`UiMode::ModelPicker`] is choosing — the two callers of the one
/// reusable list. The DECIDER (config.json's `decider_engine`/`decider_model`) walks Engine→Model;
/// the WORKER (registry.json's `worker_model`) picks Model only, because a worker's engine is fixed
/// at create time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickTarget {
    Decider,
    Worker,
}

/// Which STAGE of the two-stage pick the list is showing. The decider starts at `Engine` and
/// advances to `Model`; the worker opens straight on `Model`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickStage {
    Engine,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuditTab {
    Turns,
    Decisions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoardColumn {
    Paused,
    NeedsYou,
    Pending,
    Autopilot,
    Working,
}

impl BoardColumn {
    pub(crate) const ALL: [Self; 5] = [
        Self::Paused,
        Self::NeedsYou,
        Self::Pending,
        Self::Autopilot,
        Self::Working,
    ];

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Paused => 0,
            Self::NeedsYou => 1,
            Self::Pending => 2,
            Self::Autopilot => 3,
            Self::Working => 4,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Paused => "PAUSED",
            Self::NeedsYou => "NEEDS YOU",
            Self::Pending => "PENDING",
            Self::Autopilot => "AUTOPILOT",
            Self::Working => "WORKING",
        }
    }
}

/// One immutable quick-switch result captured when `/` opens. The id is the stable
/// selection key; the root is display/search context only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SwitchItem {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) root: String,
}

impl AuditTab {
    pub(crate) fn other(self) -> Self {
        match self {
            Self::Turns => Self::Decisions,
            Self::Decisions => Self::Turns,
        }
    }
}

#[derive(Debug)]
pub(crate) enum UiMode {
    Normal,
    /// Derived fleet board over the same managed sessions. No board state is
    /// persisted; cards move when their real runtime projection changes.
    Board,
    /// The dashboard's own preferences — a VIEW, reached by `0`, not an overlay.
    ///
    /// A third tab rather than a modal, at the human's word (*"make the tab 0 after others tab and
    /// name it settings. we then can use it to config theme"*), and numbered `0` so the two views
    /// they work in keep `1` and `2`: a setting is a place you visit, not a dialog that interrupts.
    ///
    /// `cursor` indexes [`crate::settings::SettingKind::ALL`] — the SETTINGS, one row each, because
    /// there will be more of them than one (*"we may have many settings in the future"*). `open` is
    /// `Some(option index)` while that row's dropdown is down, and it is the only state in which a
    /// value can change: exactly one of the two cursors moves at a time, so `j` is never ambiguous.
    Settings {
        cursor: usize,
        open: Option<usize>,
    },
    /// THE DECISION MOMENT: answer the selected session's open stop.
    ///
    /// This overlay used to be a text field with the question clamped to two lines and every
    /// option crushed onto one truncated row. The user's report: *"when it needs me, it is not
    /// interactable or for me to be able to see the whole set of commands"* — both halves
    /// true, and both fatal for the one screen where a human has to READ before acting.
    ///
    /// Now: the whole question and every option are fully wrapped in a scrolling body; a cursor
    /// moves with ↑↓; and Enter on an empty field sends the highlighted option. Typing still
    /// overrides with free text, because "none of these — do X instead" has to stay reachable.
    Answering {
        input: Field,
        /// Which option the cursor is on. Sent by Enter when `input` is empty. Clamped at
        /// use, never trusted: the stop can be replaced under the overlay by a refresh, and a
        /// stale index must not index past a shorter option list.
        choice: usize,
        /// First visible row of the scrolling middle region (question + options). `usize::MAX`
        /// temporarily requests cursor-follow after ↑/↓ option navigation; the renderer publishes
        /// the concrete top row before wheel/Page scrolling resumes. Header and input stay pinned.
        scroll: usize,
    },
    Creating(CreateForm),
    /// Edit human display metadata while retaining the stable runtime id.
    Renaming {
        id: String,
        current: Option<String>,
        input: Field,
    },
    /// The create form named a directory that does not exist yet, and this asks whether to
    /// CREATE it. `submit_create` rejects a missing path (canonicalize fails on it) rather than
    /// silently `mkdir`ing — that guard catches a typo'd path — so instead of a hard no this
    /// offers the choice. User: *"when we create new session, if the folder is not defined in
    /// the popup to allow user to create the folder too."*
    ///
    /// `y`/Enter creates the tree and finishes the create; any other key returns to the create
    /// form with its fields intact, so a typo is one edit from fixed rather than a full re-entry.
    ConfirmCreateDir {
        /// The create form as it stood at submit — kept so cancelling loses nothing and confirm
        /// has every field it needs to finish.
        form: CreateForm,
        /// The (trimmed) directory the human typed that does not exist — shown in the prompt and
        /// `create_dir_all`'d on confirm.
        dir: String,
    },
    /// INLINE goal editing for the selected session (`g`): a [`Composer`] over the dashboard, for
    /// the edit that does not justify suspending the TUI for an external editor. `^X^E` still opens
    /// `$EDITOR`, escalating to exactly the same [`BriefEdit`] path, seeded with the buffer.
    ///
    /// Saving routes through [`apply_goal_edit`], the ONE `brief.md` write path (atomic
    /// — see its doc comment for why that is load-bearing), and an empty/whitespace-only
    /// save keeps the previous goal, like the editor path's empty buffer.
    EditingGoal {
        id: String,
        /// The `brief.md` this edit lands on — resolved ONCE at open (via
        /// [`App::selected_brief`], the same resolution `g` uses), so a selection change
        /// or a registry rewrite while the overlay is up cannot re-target the write.
        brief: PathBuf,
        /// The goal as it stood on disk when the overlay opened. Read ONCE here, never
        /// per frame: the render path deliberately does no file I/O (same rule as the
        /// cached daemon probe). It SEEDS `input`, and it is what "did this session already
        /// have a goal?" asks on the autopilot path.
        current: String,
        /// The goal being edited, SEEDED FROM `current` at open. A multi-line brief used to open
        /// this field empty above a read-only preview, because a one-line `Field` could not hold
        /// one; a [`Composer`] can, so the field opens on the mandate itself.
        input: Composer,
        /// Set when this field was opened BY `m` on its way into Autopilot, in which case
        /// saving also turns autopilot on and cancelling leaves it off.
        ///
        /// User: *"When they switch from standard to autopilot, we need to pop up and ask them to
        /// enter a goal. With existing session, if we turn off autopilot, the brief will stay but
        /// when we turn it on again, it needs to ask the new goal and put the old goal there so
        /// people can update."* So this is not a validation gate that fires when the goal is
        /// missing — it ALWAYS asks, seeded with whatever is on disk, because "what is this
        /// session for now?" is the question worth asking at the moment you hand it the wheel.
        then_autopilot: bool,
    },
    /// INLINE directive editing for the selected session (`i`): a [`Composer`] over
    /// the dashboard, the RESTRICTIVE counterpart to [`UiMode::EditingGoal`]. Where the goal
    /// (`brief.md`) says what the session is FOR, the directive (`directive.md`) is a standing
    /// human rule the decider may ONLY ever forbid by — read fresh at each consult and fenced
    /// as trusted-but-restrictive (see `ProjectPaths::directive`).
    ///
    /// Mirrors the goal field's mechanism exactly — the same `ratatui-textarea` buffer, `^X^E`
    /// escalation to the SAME `$EDITOR` path (via [`DirectiveEditReq`]), Enter saves through
    /// the atomic [`apply_directive_edit`], and an empty/whitespace-only save KEEPS the current
    /// directive (the anti-wipe guard). The one addition is `^X^R` = RESCIND: rescinding must
    /// be a DELIBERATE, explicit action, so it is a distinct chord, never a side effect of saving
    /// an emptied buffer (which would make rescinding ambiguous with "I just cleared the text").
    /// It was bare `^X` until the buffer became a real editor, where `^X` is the prefix `^X^E`
    /// starts with — and a two-key rescind is if anything the more deliberate of the two.
    EditingDirective {
        id: String,
        /// The `directive.md` this edit lands on — resolved ONCE at open (via
        /// [`App::selected_directive`]), so a selection change or a registry rewrite while the
        /// overlay is up cannot re-target the write.
        directive: PathBuf,
        /// The directive being edited, SEEDED FROM `current` at open — multi-line and all, like
        /// the goal field.
        input: Composer,
    },
    /// EDIT THE CADENCE (`c`) — how often pmd nudges this session.
    ///
    /// A typed field rather than the create form's `←/→` stepper. The stepper moves in
    /// 60-second steps, which is fine when you are choosing a value once, but going from 5
    /// minutes to an hour after the fact would be 55 presses. Typing takes one.
    ///
    /// It accepts what a human would write — `600`, `10m`, `1h30m` — through
    /// [`parse_cadence`], and clamps to the SAME `job_engine::CADENCE_{MIN,MAX}_S` the
    /// agent's own proposals are clamped to, so the two routes to this dial cannot disagree
    /// about what is sane.
    EditingCadence {
        id: String,
        /// The registry entry's root, resolved ONCE at open, so a selection change or a
        /// registry rewrite while the overlay is up cannot re-target the write.
        root: PathBuf,
        /// Set when this field was opened on the way INTO Autopilot, because the session had no
        /// cadence recorded. Saving then turns autopilot on; cancelling leaves it off.
        ///
        /// User: *"we need to prompt user for cadence if the value is not set"*. A Standard session
        /// has no cadence at all now (the create form does not ask), so the moment autopilot starts
        /// the heartbeat is the moment the number first means something — and the same moment the
        /// goal is asked for. The two prompts CHAIN: goal, then cadence, then the flip.
        then_autopilot: bool,
        /// The EFFECTIVE cadence when the overlay opened, in seconds — read from the
        /// ledger, falling back to the harness default, so the field shows what pmd is
        /// really doing rather than what the registry once asked for. Read ONCE here: the
        /// render path does no file I/O.
        current: u64,
        /// What has been typed. Empty save keeps the current value, like the goal field.
        input: Field,
    },
    /// SEND ONE MESSAGE to the row's already-running agent (`s`), without taking over
    /// the terminal. An inline single-line field, mirroring [`UiMode::EditingGoal`]
    /// exactly — same `Char`-push/`Backspace`-pop, same `Ctrl+E` escalation to `$EDITOR`,
    /// Enter sends, Esc cancels.
    ///
    /// The typed text SURVIVES a refusal. `submit_send` leaves this mode only on a
    /// delivered send, because a message that evaporates with a one-line status is the
    /// same "it said nothing and nothing happened" failure as a dead key — and here the
    /// human may have composed kilobytes in an editor.
    Sending {
        /// Resolved ONCE when the field opens (registry lookup + session name), so a
        /// selection change while the overlay is up cannot re-target the send.
        target: SendTarget,
        /// A [`crate::Composer`], not a [`Field`]: a message is prose, so this one surface carries a
        /// real multi-line editor with the terminal's own bindings. See that module for why.
        input: Composer,
    },
    /// Search and select one of the existing managed sessions (`/`). The item list is
    /// a snapshot of the dashboard order at open time, while commit resolves the chosen
    /// id against the latest refreshed rows so a background reorder cannot select the
    /// wrong session.
    Switching {
        query: Field,
        cursor: usize,
        items: Vec<SwitchItem>,
    },
    /// Confirming an action that ENDS a live agent — removal (`d`) or restart (`r`).
    /// Idle rows are removed instantly, so this only ever gates a running agent.
    Confirming {
        id: String,
        session: String,
        what: Confirmable,
    },
    /// Full-screen, readable, auto-following view of the SELECTED agent-loop row's
    /// live wake: re-reads `paths.step_log(latest_seq)` each render tick and renders it
    /// via `stream_json::render_transcript`. `scroll` = lines scrolled UP from the tail
    /// (0 = follow the bottom). Read-only; Esc/q returns to Normal. Replaces the old raw
    /// tmux `watch()` attach.
    WakeView {
        id: String,
        paths: ProjectPaths,
        scroll: usize,
    },
    /// The `?` key reference: every binding in [`BINDINGS`], drawn as an overlay
    /// over the dashboard. It exists because the keybar is now ADAPTIVE — a narrow
    /// pane drops chips, and this is the safety net that keeps them discoverable.
    /// `scroll` = rows scrolled DOWN from the top (the table can outgrow a short
    /// pane). Reached from Normal mode only, so closing it always means Normal.
    Help {
        scroll: usize,
    },
    /// The per-session audit (`v`): a full-screen, read-only view with Turns and Decisions tabs.
    /// Decisions preserves the existing structured decider audit. Turns shows pmd-owned heartbeat
    /// inputs correlated to accepted worker reports plus meaningful held events. One session only;
    /// Esc/q returns. `scroll` belongs to the active tab and `other_scroll` preserves the inactive
    /// tab's position across Tab switches. `since` is the prior per-session review watermark.
    Decisions {
        tab: AuditTab,
        scroll: usize,
        other_scroll: usize,
        since: Option<Epoch>,
        id: String,
    },
    /// Pick engine and/or model for a session's DECIDER (config.json) or WORKER (registry.json).
    /// Reusable two-stage list: the decider walks Engine→Model; the worker picks Model only (its
    /// engine is create-time). Writes only the pmtui-owned file, never the ledger.
    ///
    /// Opened by [`App::begin_model_pick`] (the Decider path refuses on a non-autopilot row, naming
    /// `m`), stepped by [`App::advance_model_pick`]/[`App::back_or_cancel_model_pick`], and committed
    /// by [`App::commit_model_pick`]. The catalog for `engine` is filled into `App::model_catalog` at
    /// open, so the render path can read it without discovery I/O.
    ModelPicker {
        /// WHICH file this pick writes — DECIDER (config.json) or WORKER (registry.json).
        target: PickTarget,
        /// The session PINNED at open, so a background refresh cannot swap the target under the
        /// overlay. Used for the write and the title.
        id: String,
        /// Which stage the list is on — `Engine` (decider only) or `Model`.
        stage: PickStage,
        /// The engine whose model catalog the `Model` stage lists. On the decider's `Engine` stage
        /// it is the engine on disk (drawn `●`); once advanced it is the engine just chosen; for the
        /// worker it is the create-time engine, fixed.
        engine: Engine,
        /// Which row the cursor is on. On `Engine` it indexes [`Engine::ALL`]; on `Model` it indexes
        /// `[(default), catalog…]` (0 = the `(default)`/`None` row). Clamped at use.
        cursor: usize,
        /// The model VALUE on disk for this target (config.json's `decider_model` / registry.json's
        /// `worker_model`), captured at open and carried across the Engine→Model advance. The `Model`
        /// stage draws the `●` "current" marker against THIS — independent of the cursor — so moving
        /// `▸` never loses the "which model is set now" signal. `None` = `(default)` is current.
        stored_model: Option<String>,
    },
}

/// What the confirm overlay is about to do. Both arms end a running agent, which is why
/// they share one gate rather than each inventing a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirmable {
    /// `d` — drop the session from the registry and kill its tmux.
    Remove,
    /// `r` — stop pmd, end the agent's pane, start pmd again.
    Restart,
    /// `a` — cherry-pick a finished job's commit onto this project's own checkout. Confirmed because it
    /// is the one action here that writes to the files a human is working in.
    Apply,
}

/// A request to compose a session brief in the user's external editor. Handled by
/// `run()` because — like the chat launchers — it must suspend the TUI (leave raw
/// mode / the alternate screen), hand the tty to a child process, then restore. Set
/// by the create form's Ctrl+E handler on the Goal field, or by `g` on a Normal-mode
/// row (which edits that session's LIVE goal).
pub(crate) struct BriefEdit {
    /// The goal text used to seed the editor buffer — the create form's current goal,
    /// or the selected session's `brief.md` as it stands on disk.
    pub(crate) goal: String,
    /// Where the saved text lands.
    pub(crate) target: BriefEditTarget,
}

/// Where an [`BriefEdit`]'s saved text is applied.
pub(crate) enum BriefEditTarget {
    /// The create form's Goal field — nothing is written to disk (the session does
    /// not exist yet; `submit_create` seeds `brief.md`).
    CreateForm,
    /// A LIVE session's on-disk `brief.md`. `JobScheduler::nudge` re-reads that file
    /// on every heartbeat, so replacing it re-aims the agent from the next nudge on;
    /// nothing else is touched (emphatically NOT `state.json` — pmtui never writes
    /// the ledger).
    Session {
        id: String,
        brief: PathBuf,
        /// Carried from [`UiMode::EditingGoal::then_autopilot`]: this edit was `^E` out of the
        /// goal prompt `m` opens, so the drain turns autopilot on once the editor has saved.
        ///
        /// The flag has to travel with the request rather than staying in `UiMode`, because the
        /// editor takes over the terminal and the mode is back to `Normal` by the time it
        /// returns — without this, escalating to `$EDITOR` would quietly abandon the flip the
        /// human was in the middle of.
        then_autopilot: bool,
    },
}

/// A request to compose a session's DIRECTIVE in the user's external editor — the directive
/// twin of [`BriefEdit`], drained by `run()` because (exactly like a brief edit) it must
/// suspend the TUI, hand the tty to `$EDITOR`, then restore. Set by `Ctrl+E` inside the inline
/// directive field ([`App::escalate_directive_edit`]).
///
/// Simpler than [`BriefEdit`]: there is only ONE target — a live session's `directive.md` — so
/// no target enum, and no `then_autopilot` (setting a directive never flips the autonomy dial).
pub(crate) struct DirectiveEditReq {
    pub(crate) id: String,
    /// Where the saved text lands (resolved once at open, via [`App::selected_directive`]).
    pub(crate) directive: PathBuf,
    /// The seed the editor buffer opens on — the directive as it stands on disk, or what was
    /// typed into the inline field before `Ctrl+E`.
    pub(crate) current: String,
}

/// Which pane the MOUSE POINTER is over, for wheel hit-testing ([`crate::PaneRects::hit`]).
///
/// There is no tracked "active" pane. The keyboard moves the SESSION SELECTION
/// (`j`/`k`/`PgUp`/`PgDn`/`Home`/`End`); the panes scroll by MOUSE WHEEL ONLY, over whichever one
/// the pointer is on. This enum just NAMES the four regions so a wheel event can be routed to the
/// right one — the focus/`Tab` machinery it used to drive was removed at the user's request
/// (*"we can just use mouse to scroll at that point"*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    /// The SESSIONS list — a wheel notch there moves the selection by a row.
    Sessions,
    /// The session detail transcript — a wheel notch scrolls it.
    Detail,
}
