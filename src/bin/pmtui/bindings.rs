//! The ONE key table. Every key pmtui binds is declared exactly once here, with the
//! scope it applies in, the rank the keybar sheds it at, and the help text `?` shows —
//! so the adaptive bar and the help overlay read the same rows and cannot disagree.

// ── The ONE key table ────────────────────────────────────────────────────────────
//
// Every key pmtui binds is declared exactly once, here. Both surfaces that advertise
// keys read THIS array and nothing else:
//
//   * `keybar_line` — the adaptive bottom bar, which HIDES chips (narrow pane, or an
//     action that does not apply to the selected row), and
//   * `help_lines` — the `?` overlay, which lists every binding unconditionally and
//     is therefore the safety net that makes hiding chips acceptable.
//
// So they cannot drift: a key added to `handle_key` and to this table shows up in
// both, and `every_bound_normal_key_is_documented_in_the_help` fails if a Normal-mode
// key is bound without a row here.

/// Help-overlay section a binding is filed under.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyGroup {
    Navigation,
    Session,
    Autonomy,
    Other,
    /// Keys that belong to a specific overlay/view rather than the dashboard.
    Overlays,
}

impl KeyGroup {
    pub(crate) fn heading(self) -> &'static str {
        match self {
            KeyGroup::Navigation => "NAVIGATION",
            KeyGroup::Session => "SESSION",
            KeyGroup::Autonomy => "AUTONOMY",
            KeyGroup::Other => "OTHER",
            KeyGroup::Overlays => "OVERLAYS & VIEWS",
        }
    }
}

/// The order the help overlay prints its sections in.
pub(crate) const KEY_GROUPS: [KeyGroup; 5] = [
    KeyGroup::Navigation,
    KeyGroup::Session,
    KeyGroup::Autonomy,
    KeyGroup::Other,
    KeyGroup::Overlays,
];

/// Which keybar a binding appears on — i.e. the [`UiMode`] it is live in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The dashboard, whose bar is the ADAPTIVE one.
    Normal,
    Answer,
    Create,
    /// The inline goal field (`g`).
    Goal,
    /// The inline directive field (`i`).
    Directive,
    /// The inline cadence field (`c`).
    Cadence,
    /// The send field (`s`).
    Send,
    /// The inline session display-name field (`R`).
    Rename,
    /// The quick session switcher (`/`).
    Switcher,
    /// The derived session-status board.
    Board,
    /// The dashboard's own preferences (`0`), with every dropdown closed.
    Settings,
    /// One setting's dropdown, down: its keys are the list's, not the view's.
    SettingsPick,
    /// The reusable model picker (`e` decider engine+model / `w` worker model).
    ModelPicker,
    Confirm,
    /// The "create this directory?" prompt shown when the create form names a missing folder.
    ConfirmCreateDir,
    /// The full-screen wake-follow view (it draws its own footer, so these chips are
    /// only ever reached through the help overlay — kept so the table stays complete).
    Wake,
    Help,
    /// Real bindings that no bar has room to advertise (paging keys); documented in
    /// the help overlay only.
    HelpOnly,
}

/// When a `Scope::Normal` binding actually DOES something on the SELECTED row.
///
/// The keybar hides chips whose precondition fails, because pressing them could only
/// produce a refusal status — offering them is a lie, and the columns are better spent
/// on the actions that do apply. The help overlay ignores this and lists them all.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Applies {
    /// Always — needs no selection.
    Always,
    /// Needs at least one row to move between.
    AnyRow,
    // There is deliberately no bare `Selected` any more. `g` was its last user, and `g` became
    // `Autopilot` when the goal turned out to steer nothing on a Standard row; every gate below
    // already implies a selection via `sel.is_some_and(..)`, so a variant that ONLY asked for one
    // had no remaining caller.
    /// Needs a selected row that is NOT a native autonomous (`Mode::Auto`) row: pmd
    /// owns those, and both `Enter` (`request_attach`) and `d` (`begin_delete`)
    /// refuse them outright.
    NotNative,
    /// Needs a selected row whose mode `pmd` actually DRIVES (`m` — flipping the
    /// autonomy dial on a row the daemon skips changes nothing observable).
    Driven,
    /// Needs a selected [`Mode::AgentLoop`] row (`r` — only an agent-loop row has a
    /// daemon-owned agent to cycle. An interactive row's pane IS the human's terminal,
    /// and a native `Auto` row is pmd's business end to end).
    AgentLoop,
    /// Needs a selected row pmd is DRIVING RIGHT NOW — mode AND tier, via
    /// `daemon::pmd_drives_row` (`c` — the cadence is the heartbeat pmd nudges on, so on a row it
    /// does not drive the dial moves a number nothing reads).
    ///
    /// Tier-aware, unlike [`Applies::Driven`], which is mode-only. User: *"make the bottom menu to
    /// only display cadence if we change to autopilot"*.
    Autopilot,
    /// Needs a selected JOB row that committed something in its own worktree (`a` — there is nothing
    /// to apply from a chat row, from a job that ran outside a repository, or from one that committed
    /// nothing, and a chip offering it there would be a promise the handler could only refuse).
    JobCommit,
    /// Needs a selected row where `s` leads somewhere ([`message_route`]). An open stop on a
    /// row nothing drives routes `s` to an Answer gate that can only refuse, and the ordinary
    /// composer stays closed until the stop clears. A row a failed fork left behind has no
    /// conversation to reach, and a staged spawn row's first Message is its spawn request's.
    ReachesAgent,
}

/// One row of [`BINDINGS`].
pub(crate) struct Binding {
    /// Display form of the key(s), for both the chip badge and the help key column.
    pub(crate) key: &'static str,
    /// Keybar chip label. Empty = this binding has no chip (help-only).
    pub(crate) label: &'static str,
    /// Help-overlay description. Empty = omitted from the help, because another row
    /// already documents the same key (e.g. Esc closes every overlay).
    pub(crate) help: &'static str,
    pub(crate) group: KeyGroup,
    pub(crate) scope: Scope,
    /// Only consulted for [`Scope::Normal`].
    pub(crate) applies: Applies,
    /// Keybar shed order: as the bar narrows, the HIGHEST rank is dropped first.
    /// Rank 0 is the honest minimum (`?` and `q`) and is never shed.
    pub(crate) rank: u8,
}

pub(crate) const BINDINGS: &[Binding] = &[
    // ── Normal mode: the dashboard's adaptive bar ──
    Binding {
        key: "↑↓/jk",
        label: "Move",
        help: "Move selection (PgUp/PgDn a page, Home/End the ends); mouse wheel scrolls a panel",
        group: KeyGroup::Navigation,
        scope: Scope::HelpOnly,
        applies: Applies::AnyRow,
        rank: 3,
    },
    Binding {
        key: "Enter",
        label: "Attach",
        help: "Attach the selected session (agent-loop: its live chat/wake)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::NotNative,
        rank: 2,
    },
    Binding {
        key: "/",
        label: "Switch",
        help: "Find and select an existing session by name or project directory",
        group: KeyGroup::Navigation,
        scope: Scope::HelpOnly,
        applies: Applies::AnyRow,
        rank: 2,
    },
    // ONE KEY PER VIEW. The status bar leads with the `1 Sessions` / `2 Tasks` TABS, so the keys
    // that switch views are exactly the numbers on screen. `Tab` is not among them: it used to
    // toggle the first two, which only duplicated `2` and `1`.
    Binding {
        key: "1/2/0",
        label: "Views",
        help: "Select a view: 1 Sessions, 2 Tasks, 0 Settings",
        group: KeyGroup::Navigation,
        scope: Scope::HelpOnly,
        applies: Applies::Always,
        rank: 10,
    },
    Binding {
        key: "n",
        label: "New",
        help: "New session — opens the create form",
        group: KeyGroup::Session,
        scope: Scope::HelpOnly,
        applies: Applies::Always,
        rank: 4,
    },
    Binding {
        key: "f",
        label: "Fork",
        help: "Fork the selected conversation into a new Standard session",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::AgentLoop,
        rank: 10,
    },
    Binding {
        key: "R",
        label: "Rename",
        help: "Set or clear the selected session's human-readable display name",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::NotNative,
        rank: 9,
    },
    Binding {
        key: "d",
        label: "Delete",
        help: "Delete the selected session (confirms while it is live)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::NotNative,
        rank: 8,
    },
    // `a` for APPLY, not `i`: `i` is the session's directive and this is not a second meaning for it.
    // The chip appears only on a job row that committed, because that is the only row where there is
    // something to apply — isolation is the dashboard's business, integration is the human's.
    Binding {
        key: "a",
        label: "Apply",
        help: "Apply the selected job's commit to this project's checkout (cherry-pick; confirms first)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::JobCommit,
        rank: 7,
    },
    // `r` outranks `d` (8) in survival on a narrow bar: restart is the RECOVERY you reach
    // for when an agent is wedged, which is exactly when you are least likely to be at a
    // comfortable width, while remove is housekeeping that can wait for one.
    // `p` sits beside `r`: both change what a session is doing RIGHT NOW, and they are the pair
    // a human reaches for when a session is misbehaving — restart it, or stop it. Rank 7 with
    // `r`'s, one ahead of `d`.
    Binding {
        key: "p",
        label: "Pause",
        help: "Pause: autopilot off + kill the agent (Enter resumes it; m starts autopilot again)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::AgentLoop,
        rank: 9,
    },
    Binding {
        key: "r",
        label: "Restart",
        help: "Restart the selected agent — stops pmd, ends its pane, starts pmd again",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::AgentLoop,
        rank: 7,
    },
    // `s` sits beside `Enter` at rank 2 because it is the OTHER way to reach the agent,
    // and the cheaper one: Enter takes over the terminal, `s` says one thing and returns.
    // On an autopilot-OFF row it is the only key that reaches the agent at all — `a`
    // refuses (nothing would deliver an answer), `g` only rewrites a brief nothing is
    // going to re-read, and pmd never touches that pane, which also makes `s` race-free
    // there. It must therefore survive a narrow tmux split.
    //
    // The letter is the user's: *"s Send (instead of o)"*. agent-deck's `s` was where this
    // key came from; a dashboard whose own word for the action is "Send" should not make
    // you remember someone else's letter for it.
    //
    // `Applies` is computed from `ProjectView` fields only, so the chip CANNOT be gated
    // on live-pane liveness (that needs a tmux probe, which the render path must not do).
    // The handler's refusals carry that instead, and each one names the way forward.
    Binding {
        key: "s",
        label: "Send",
        help: "Send a message; with an open stop, show its Answer surface instead",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::ReachesAgent,
        rank: 1,
    },
    // There is no `p` (pause) row: the key was removed in m15. `m` is the one on/off
    // switch, and `registry.enabled` survives only as the PROGRAMMATIC disable.
    //
    // `m` for MODE — the autonomy dial's one-key toggle. The user's call: *"if autopilot a
    // conflicts with answer, we can change the tier to mode, so we can use letter m for
    // that."* Only the WORD moved: the `Tier` type and the on-disk
    // `autonomy: standard|autopilot` field are untouched, because renaming a persisted
    // field to match a keybar label would be a migration bought with nothing.
    Binding {
        key: "m",
        label: "Mode",
        help: "Autopilot on/off — on: pmd drives it, off: you do",
        group: KeyGroup::Autonomy,
        scope: Scope::Normal,
        applies: Applies::Driven,
        rank: 5,
    },
    // ONE goal key, mirroring the create form's Goal field: type inline, `^E` for the real
    // editor. Both routes write the same `brief.md` through the same atomic
    // `apply_goal_edit`, and the shifted `G` twin is gone — the distinction it drew (which
    // surface you want) is one you can only make after seeing the goal, which is exactly
    // what the field shows you.
    Binding {
        key: "g",
        label: "Goal",
        help: "Edit an autopilot session's goal inline (^E opens $EDITOR)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        // AUTOPILOT, not merely selected — the same rule `c` follows below, and for the same reason.
        // `brief.md` is read by exactly one thing, the nudge prompt pmd injects each heartbeat, and
        // that does not run on a row pmd does not drive. On Standard the human IS the steering, so
        // offering this key there advertises an edit nothing would consult.
        applies: Applies::Autopilot,
        rank: 6,
    },
    // The RESTRICTIVE counterpart to `g`, one letter along: `g` says what the session is FOR,
    // `i` sets its standing DIRECTIVE — the one rule its decider may only ever FORBID by (e.g.
    // "never auto-approve a test edit"). Same AUTOPILOT gate as `g`/`c`, and for the same reason:
    // `directive.md` is read only by the decider consult, which never runs on a row pmd does not
    // drive. `^X` inside the field RESCINDS it; an empty save keeps it. Set-once like `c`, so it
    // shares `c`'s high shed rank (9) — a shed chip hides little, since `?` still lists it.
    Binding {
        key: "i",
        label: "Directive",
        help: "Set an autopilot session's standing directive (^E $EDITOR, ^X rescinds)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::Autopilot,
        rank: 9,
    },
    // WHAT the session is for (`g`) and HOW OFTEN it is asked (`c`) are the two halves of a
    // session's standing instruction, so they sit together. `c` ranks last of the action
    // keys: it is the one you set once and rarely revisit — and unlike the others, its
    // current value is already visible on the row, so a shed chip hides less here.
    Binding {
        key: "c",
        label: "Cadence",
        help: "Set how often pmd nudges the selected AUTOPILOT session (60s..24h)",
        group: KeyGroup::Autonomy,
        scope: Scope::Normal,
        // AUTOPILOT, not merely agent-loop: the cadence is the heartbeat pmd nudges on, so on a
        // Standard row this dial moves a number nothing reads.
        applies: Applies::Autopilot,
        rank: 9,
    },
    // The decider ENGINE + MODEL, autopilot-only like `c`/`g`/`i`: the supervisor consult that `e`
    // retargets never runs on a row pmd does not drive. Opens a two-stage select panel (engine, then
    // that engine's models). High shed rank — set-once, and the current value is not otherwise on the
    // row.
    Binding {
        key: "e",
        label: "Decider",
        help: "Pick the decider engine + model (autopilot)",
        group: KeyGroup::Autonomy,
        scope: Scope::Normal,
        applies: Applies::Autopilot,
        rank: 9,
    },
    // The WORKER model (`w`) — which `--model` the session's agent launches with. Unlike the
    // decider, this is NOT autopilot-gated: the worker launch carries `--model` on both the driven
    // and the human-driven paths, so it applies to any agent-loop row. Opens the SAME picker on its
    // Model stage (the worker's engine is fixed at create time). Set-once, so it shares the high shed
    // rank; a restart applies it, which the status names.
    Binding {
        key: "w",
        label: "Worker",
        help: "Set the worker model for the selected session (restart to apply)",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::AgentLoop,
        rank: 9,
    },
    // `v` — the per-session AUDIT: heartbeat turns plus the existing decider decision view.
    // Full-screen and read-only; Tab switches views. AUTOPILOT-gated like `g`/`c`/`i`.
    Binding {
        key: "v",
        label: "Audit",
        help: "Review heartbeat turns and decider decisions for the selected session",
        group: KeyGroup::Session,
        scope: Scope::Normal,
        applies: Applies::Autopilot,
        rank: 6,
    },
    Binding {
        key: "?",
        label: "Help",
        help: "Show this key reference (j/k scroll, any other key closes)",
        group: KeyGroup::Other,
        scope: Scope::Normal,
        applies: Applies::Always,
        rank: 0,
    },
    Binding {
        key: "q",
        label: "Quit",
        help: "Quit pmtui (Esc also quits from the dashboard)",
        group: KeyGroup::Other,
        scope: Scope::Normal,
        applies: Applies::Always,
        rank: 0,
    },
    // Board actions use the ordinary keybar renderer so every visible chip is
    // clickable through the same key handler as its keyboard equivalent. Board `1` and `2` are the
    // status bar's view TABS and `/` and `n` its top-right controls, documented by the rows above.
    Binding {
        key: "hjkl",
        label: "Move",
        help: "Task Board: move between cards and status columns (inert in an open card detail)",
        group: KeyGroup::Overlays,
        scope: Scope::HelpOnly,
        applies: Applies::Always,
        rank: 0,
    },
    Binding {
        key: "Enter",
        label: "Open",
        help: "Task Board: inspect the selected task; Enter again attaches",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::AnyRow,
        rank: 1,
    },
    Binding {
        key: "Enter",
        label: "Change",
        help: "Settings: open the selected setting's list of values",
        group: KeyGroup::Overlays,
        scope: Scope::Settings,
        applies: Applies::Always,
        rank: 0,
    },
    Binding {
        key: "↑↓/jk",
        label: "Move",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Settings,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "Enter",
        label: "Select",
        help: "Settings list: use the value under the cursor and remember it",
        group: KeyGroup::Overlays,
        scope: Scope::SettingsPick,
        applies: Applies::Always,
        rank: 0,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "Settings list: close it, changing nothing",
        group: KeyGroup::Overlays,
        scope: Scope::SettingsPick,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "↑↓/jk",
        label: "Move",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::SettingsPick,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "s",
        label: "Send",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::ReachesAgent,
        rank: 2,
    },
    Binding {
        key: "m",
        label: "Mode",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::Driven,
        rank: 4,
    },
    Binding {
        key: "p",
        label: "Pause",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::AgentLoop,
        rank: 4,
    },
    Binding {
        key: "r",
        label: "Restart",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::AgentLoop,
        rank: 5,
    },
    Binding {
        key: "d",
        label: "Delete",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::NotNative,
        rank: 5,
    },
    Binding {
        key: "f",
        label: "Fork",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::AgentLoop,
        rank: 3,
    },
    Binding {
        key: "R",
        label: "Rename",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::AnyRow,
        rank: 2,
    },
    Binding {
        key: "v",
        label: "Audit",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::Autopilot,
        rank: 4,
    },
    Binding {
        key: "Esc",
        label: "Back",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Board,
        applies: Applies::Always,
        rank: 0,
    },
    // ── The other modes' bars. Their `help` is filled in once per KEY, so the
    //    overlay lists `Esc` (say) a single time rather than once per overlay.
    Binding {
        key: "Enter",
        label: "Submit",
        help: "Answer overlay: send the typed answer to the agent",
        group: KeyGroup::Overlays,
        scope: Scope::Answer,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "Close any overlay without acting",
        group: KeyGroup::Overlays,
        scope: Scope::Answer,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Tab",
        label: "Field",
        help: "Create form: next field (↑↓ also move, Shift+Tab goes back)",
        group: KeyGroup::Overlays,
        scope: Scope::Create,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "Enter",
        label: "Save",
        help: "Rename field: save the display name (empty restores the stable id)",
        group: KeyGroup::Overlays,
        scope: Scope::Rename,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Rename,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "←/→",
        label: "Toggle",
        help: "Create form: change a toggle (engine, autonomy, cadence, decider)",
        group: KeyGroup::Overlays,
        scope: Scope::Create,
        applies: Applies::Always,
        rank: 4,
    },
    Binding {
        key: "^E",
        label: "Edit goal",
        help: "Create form: compose the Standard Message or Autopilot Goal in $EDITOR",
        group: KeyGroup::Overlays,
        scope: Scope::Create,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Enter",
        label: "Create",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Create,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Create,
        applies: Applies::Always,
        rank: 5,
    },
    // The cadence field's own bar. `Esc` carries no `help` (documented once, by the answer
    // overlay's row) and there is no `^E` row at all — a duration has no multi-line form.
    Binding {
        key: "Enter",
        label: "Save",
        help: "Cadence field: save the typed interval (an empty save keeps the current one)",
        group: KeyGroup::Overlays,
        scope: Scope::Cadence,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Cadence,
        applies: Applies::Always,
        rank: 2,
    },
    // The inline goal field's own bar. `Esc` and `^X^E` carry no `help` — both keys are
    // already documented once (by the answer overlay's Esc row and the Message field's `^X^E` row
    // respectively), and the overlay lists each key a single time. The readline map this field shares
    // with the Message and directive buffers is documented once, on the `^J` row.
    Binding {
        key: "Enter",
        label: "Save",
        help: "Goal field: save the typed goal (an empty save keeps the current one)",
        group: KeyGroup::Overlays,
        scope: Scope::Goal,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "^X^E",
        label: "Editor",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Goal,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Goal,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "Enter",
        label: "Select",
        help: "Session switcher: select the highlighted existing session",
        group: KeyGroup::Overlays,
        scope: Scope::Switcher,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "↑↓",
        label: "Move",
        help: "Session switcher: move between filtered results (Page keys also work)",
        group: KeyGroup::Overlays,
        scope: Scope::Switcher,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Switcher,
        applies: Applies::Always,
        rank: 3,
    },
    // The inline directive field's own bar. `^X^E` and `Esc` carry no `help` (each key is documented
    // exactly once — by the Message field's `^X^E` row and the answer overlay's `Esc` row). `^X^R` is
    // UNIQUE to this field, so it carries its own row: rescind is the one action no other overlay has,
    // and the whole point is that it is deliberate and discoverable.
    Binding {
        key: "Enter",
        label: "Save",
        help: "Directive field: save the typed directive (an empty save keeps the current one)",
        group: KeyGroup::Overlays,
        scope: Scope::Directive,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "^X^E",
        label: "Editor",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Directive,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "^X^R",
        label: "Rescind",
        help: "Directive field: rescind the standing directive (removes directive.md) — a chord, because \
               bare ^X is the prefix ^X^E starts with, and rescinding should be deliberate anyway",
        group: KeyGroup::Overlays,
        scope: Scope::Directive,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc",
        label: "Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Directive,
        applies: Applies::Always,
        rank: 4,
    },
    // The send field's own bar. As with the goal field, `^E` and `Esc` carry no `help` —
    // each key is documented exactly once in the overlay.
    Binding {
        key: "Enter",
        label: "Send",
        help: "Send field: deliver the typed message to the agent's pane",
        group: KeyGroup::Overlays,
        scope: Scope::Send,
        applies: Applies::Always,
        rank: 1,
    },
    // HELP ONLY, deliberately. Four chips do not fit 80 columns, and the keybar then shed `Esc Keep
    // draft` — the one key a human needs to get OUT of the composer. The newline is the chip to drop,
    // because the EMPTY COMPOSER ITSELF teaches it: its placeholder reads "message this session ·
    // Ctrl+J for a new line", right where a human is looking when they need it. The readline map
    // behind the composer is documented once, here.
    Binding {
        key: "^J",
        label: "Newline",
        help: "Message, goal and directive: open a new line (Alt+Enter too, and Shift+Enter where your \
               terminal reports it). Those three fields are full readline buffers: alt+b/alt+f by word, \
               ^w/alt+backspace kill a word, alt+d kills forward, ^a/^e line ends, ^k to end of line, \
               ^u/^r undo and redo",
        group: KeyGroup::Overlays,
        scope: Scope::HelpOnly,
        applies: Applies::Always,
        rank: 2,
    },
    // The composer is a real editor (`ratatui-textarea`), and nothing on screen hints that the draft
    // can leave for `$EDITOR`, so THIS is the chip worth a column.
    Binding {
        key: "^X^E",
        label: "Editor",
        help: "Message, goal and directive: edit the buffer in $EDITOR — readline's own chord, so bare ^E \
               is end-of-line in all three",
        group: KeyGroup::Overlays,
        scope: Scope::Send,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "Esc",
        label: "Keep draft",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Send,
        applies: Applies::Always,
        rank: 4,
    },
    // The model picker's own bar (shared by the `e` decider and `w` worker pickers). `Esc` carries
    // no `help` (documented once, by the answer overlay's Esc row); Enter and the move keys are
    // unique to this list. `Esc` here steps BACK a stage on the decider's Model stage before it
    // cancels, hence the "Back/Cancel" label.
    Binding {
        key: "Enter",
        label: "Select",
        help: "Model picker: select the highlighted engine or model",
        group: KeyGroup::Overlays,
        scope: Scope::ModelPicker,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "↑↓/jk",
        label: "Move",
        help: "Model picker: move between choices",
        group: KeyGroup::Overlays,
        scope: Scope::ModelPicker,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc",
        label: "Back/Cancel",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::ModelPicker,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "y",
        // Overridden per action by `scope_chips` — see there for why a static label here
        // would be a lie half the time.
        label: "Delete",
        help: "Confirm the pending delete/restart — any other key cancels",
        group: KeyGroup::Overlays,
        scope: Scope::Confirm,
        applies: Applies::Always,
        rank: 1,
    },
    // The "create this directory?" prompt (shown when the create form names a missing folder).
    Binding {
        key: "y/enter",
        label: "Create",
        help: "Create-directory prompt: make the folder and set up the session — any other key goes back",
        group: KeyGroup::Overlays,
        scope: Scope::ConfirmCreateDir,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "Esc",
        label: "Back",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::ConfirmCreateDir,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc/q",
        label: "Return",
        help: "Wake view: back to the dashboard",
        group: KeyGroup::Overlays,
        scope: Scope::Wake,
        applies: Applies::Always,
        rank: 1,
    },
    Binding {
        key: "↑↓/jk",
        label: "Scroll",
        help: "Wake view / this help: scroll a row",
        group: KeyGroup::Overlays,
        scope: Scope::Wake,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "End",
        label: "Follow",
        help: "Wake view: jump back to following the tail",
        group: KeyGroup::Overlays,
        scope: Scope::Wake,
        applies: Applies::Always,
        rank: 3,
    },
    Binding {
        key: "PgUp/PgDn",
        label: "",
        help: "Wake view / this help: scroll a page",
        group: KeyGroup::Overlays,
        scope: Scope::HelpOnly,
        applies: Applies::Always,
        rank: 9,
    },
    Binding {
        key: "↑↓/jk",
        label: "Scroll",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Help,
        applies: Applies::Always,
        rank: 2,
    },
    Binding {
        key: "Esc",
        label: "Close",
        help: "",
        group: KeyGroup::Overlays,
        scope: Scope::Help,
        applies: Applies::Always,
        rank: 1,
    },
];
