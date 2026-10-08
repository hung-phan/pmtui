# Board-Native Session Workspace Design

## Problem

The first Board implementation groups managed sessions by live state, but it is
still an overview layered beside the real workbench. Cards lead with process
identity, card clicks leave Board, and New opens a generic provisioning form.
The result is useful monitoring but does not feel like Multica's board, where a
person can recognize work, create it, inspect it, and steer it without changing
surfaces.

This revision deliberately exposes two user interfaces over the same runtime:

1. **Session workbench**: the current terminal-first UI remains unchanged for
   direct conversation and detailed control.
2. **Task Board**: a task-first, full-width UI for creating, scanning,
   inspecting, and steering ad hoc work.

A Board-created task persists a human title on its managed-session registry
entry. The linked managed session remains the execution record and owns the
goal, conversation, terminal, Autopilot state, decisions, and audit. This first
task model is intentionally one task to one managed session; it does not yet
add a separate Issue/Run hierarchy or manipulate git branches/worktrees.

## Product Contract

### Work-Led Cards

Each task card leads with human intent:

1. the persisted task title for work created from Task Board;
2. otherwise the first non-empty line of `brief.md`;
3. otherwise the Standard session's initial Message; and
4. otherwise the managed-session id for pre-task sessions.

The session id, project-directory basename, engine, and Standard/Autopilot mode
remain visible as secondary metadata. Runtime state and next action remain
wrapped below the title. Fork lineage and terminal availability remain visible.

The task title is human-owned registry metadata and never instructs the worker
independently. The initial Message or Goal remains the execution instruction.

### Derived Lifecycle

Task columns remain projections of runtime truth, ordered Paused, Needs You,
Pending, Autopilot, then Working:

- **Paused**: explicitly disabled;
- **Needs You**: human decisions, capability gaps, stuck work, and completion
  confirmation;
- **Pending**: an enabled Standard item that is idle or not started;
- **Autopilot**: an enabled Autopilot item between turns or waiting for its next
  heartbeat; and
- **Working**: a live agent confirmed to be actively producing a turn.

There is no manually assigned status and no machine-owned Done column. Cards
move when the underlying session or Autopilot state changes. They cannot be
dragged between observational states. Answer, Mode, Pause, Resume, and Close are
the explicit operations that change runtime truth.

The persistent top-right controls own `/` switching, `Tab` projection changes,
and `n` creation in both views. The bottom menu contains selected-item actions
only, except that where a narrow terminal hides those controls the lane menu
keeps `Esc` back to the Session view. A switcher opened from Task view returns
to Task view on selection or cancel.

### Task-Board Inspector

Clicking a card selects it and reveals its state-specific primary action plus
Restart, Delete, applicable Fork, and applicable Audit in the bottom menu
without leaving the lanes. A lane chip appears only where its key acts on that
card. Enter replaces the lanes with a full-width Task detail view. It reuses the
ordinary selected-session preview, transcript, stop surface, and Message shelf;
it does not create a second detail implementation or duplicate the Board's
lifecycle controls.

Message is intentionally composed in detail. Lane mode cannot show the target
transcript or delivery result, so lane `s` never opens the composer in place. An
open stop routes lane `s` to Answer from any lane (the Needs You card's primary
action). A Needs You card with no open stop (a Blocked Standard or Stuck
session) follows the Session view's `s` route and opens Task detail with its
composer, so the lane's main action is never dead. Other lanes neither advertise
nor handle `s` until Enter opens Task detail. In detail, an open stop changes
the shelf action to Answer while retaining the simple Message title; `s` or a
shelf click uses the Answer entry rather than an ordinary Message composer.
Existing Autopilot deliverability checks remain authoritative.

The existing wrapped Status history sits below Board content at the same
adaptive height as the Session view. It keeps action failures visible and
scrollable without duplicating status state. Entered Task detail omits the
Status band and returns those rows to the transcript; the footer then carries
the latest status so refusals stay visible.
Fork creates and selects the child card but remains in lane view; Enter alone
opens Task detail.

### Session Names

`R` edits an optional human display name for the selected managed session. The
stable session id remains immutable because it owns tmux names, state paths,
locks, and conversation identity. A blank rename clears the display name and
restores the stable id as the label.

The New Session and New Task forms expose the same optional Name field while
keeping Message/Goal as their initial focus.

The display name is human-owned registry metadata. Session rows, Task cards,
selected preview chrome, attention names, and `/` switch results show and search
it, while every click and action still commits by stable session id. Existing
registries without the field remain valid.

- Click selects a card and updates its bottom-menu controls; Enter opens detail.
- Escape closes the inspector but keeps Board and selection.
- Enter while the inspector is open attaches through the ordinary terminal
  route.
- Once detail is open, contextual `s` and `m` reuse their existing handlers and
  preconditions. Lane controls route `p`, `r`, `d`, `f`, and applicable `v`
  through those same handlers.
- `j`/`k` and `h`/`l` continue moving between cards while the inspector follows
  the stable selected session id.
- Clicking the inspector's preview title and Message shelf routes through the
  same pointer handlers as the main workbench.

The inspector is a viewport, not persisted state. Leaving and reopening Board
does not alter any managed session.

### Intent-First Quick Create

New from Task Board opens the existing create flow over Board with task-specific
defaults:

- persist the first non-empty request line as the task title;
- prefill the selected session's project directory;
- inherit its worker engine and model;
- start on Standard;
- focus Message first; and
- show intent before runtime configuration.

Typing a Message and pressing Enter is enough to create and launch ordinary ad
hoc work in the current project. Advanced engine, model, directory, and
Autonomy fields remain available in the same form. Cancel returns to Board;
success returns to Board, selects the new managed session, and opens its
inspector. Generic `n` from the Session workbench keeps the current unprefilled,
session-oriented behavior and does not create task metadata.

### Discoverability And Responsive Layout

Task lanes are framed with horizontal gutters and semantic terminal colors:
Paused dim, Needs You yellow, Pending blue, Autopilot cyan, and Working green.
Task cards inherit their lane/status color and are individually bordered, with
the work title visually dominant and session/runtime identity secondary. The
selected card adds a raised theme surface and bold border without changing the
card's dimensions. Enabled Autopilot Session-list and Task-card IDs use a bright
repeating TachyonFX hue sweep. Preview/detail titles and panel borders remain on
their semantic colors. The effect runs on the existing Autopilot
redraw cadence and cached preview, so animation frames add no tmux capture work.

Task view and New must survive the main keybar's `100x28` fitting so the feature is
discoverable without prior key knowledge.

- `150+` columns: all five operational columns;
- `105+` columns: three adjacent columns;
- `72+` columns: two adjacent columns;
- narrower: one column.

`Tab` and `Shift+Tab` switch between the top-level Session and Task views. The
Board footer exposes only clickable selected-item actions. Column range and
detail labels are not repeated in a separate row. Mouse wheel over a Board column moves its card selection
and does nothing over an empty column; the lane and card windows scroll only when
the selection leaves them, so the column under the pointer stays put. Wheel over an
open inspector scrolls the reused detail pane.

## Failure And Authority Rules

- Quick Create writes only the ordinary registry entry (including task title)
  and per-session human-owned files through the existing create path.
- Inspector actions never bypass confirmation, input locking, terminal
  liveness rechecks, or Autopilot authority gates.
- Missing or unreadable Goal/Message data falls back to the session id; it does
  not hide the card or block lifecycle control.
- Board remains usable when pmd or a project terminal is unavailable.

## Acceptance

- At `100x28`, the main screen visibly offers Board and New.
- A twelve-session gallery shows three Board columns without empty-canvas
  domination or clipped selected cards.
- Cards lead with human-authored work intent and show project/runtime metadata
  second.
- Card click selects the card and exposes its bottom-menu actions without
  leaving Board, and Enter opens Task detail; click targets commit by stable
  session id after reorder.
- Inspector contextual Send/Answer, Mode, Pause, Restart, Fork, and Attach routes use
  existing handlers.
- Board New is Message-first, prefilled from the selected project, and returns
  to Board on cancel and success.
- Needs You contains every human decision, including `confirm_done`; no machine
  creates a Done state.
- `80x24`, `100x28`, and `120x32` dogfood captures include dense Board,
  inspector, quick-create, and Board Answer states.
- Real tmux acceptance proves the Board workflow without launching real model
  work.
