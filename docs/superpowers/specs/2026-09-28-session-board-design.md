# Adaptive Session Board Design

> **Superseded** by `2026-09-28-board-native-workspace-design.md` and
> `docs/SPEC.md` (section 3), which define the shipped Task view. Only two rules
> here remain in force: columns are derived from runtime truth (no manual
> status, no Done column, no drag-and-drop between states), and there is no
> separate Issue/Run hierarchy above managed sessions. The rest describes the
> first Board; each section that no longer matches the code is marked.

## Historical Product Decision

A later human decision introduced a separate Task Board UI with human-owned task
titles on managed-session entries. pmtui still does not add an independent
Issue/Run hierarchy above managed sessions in this slice.

The original decision was:

pmtui will not add a Task, Issue, Run, or Ticket entity above managed sessions.
A managed session already owns the user-visible identity, project directory,
conversation, terminal, goal, Autopilot state, decisions, and audit history.
Adding another work object would duplicate identity and force users to decide
whether an action applies to a task or its session.

The board is a second view over existing sessions. Cards move because their real
runtime state changes, not because pmtui persists a separate board status.

## Influences

- Orca demonstrates that fleet state is quickly understood as status columns.
- Herdr and Agent Deck keep the native terminal central and make session actions
  available from keyboard and mouse.
- Multica shows the value of creating ad hoc work directly from a board, but its
  Issue/Run split is unnecessary when one pmtui session already carries both.
- YoanWai agent-manager and Agent Deck treat forks as related normal sessions,
  which maps directly onto pmtui's one-session/one-terminal invariant.

## Entry And Exit

> Superseded: `Tab`/`Shift+Tab` switch between the Session and Task views, and
> Enter opens Task detail inside Task view rather than returning to the
> workbench.

- `b` toggles between the existing workbench and Board.
- Board is read from `App.projects`; it writes no board state.
- Escape or `b` returns to the workbench with the same selected session.
- Selecting a card and pressing Enter opens that session in the existing
  workbench detail. Enter there retains its current attach meaning, avoiding a
  surprising one-key terminal takeover from the overview.

## Columns

> Superseded: the shipped Task view derives five lanes, ordered Paused, Needs
> You, Pending, Autopilot, and Working. There is no Waiting column; idle Standard
> work is Pending, and Autopilot work between turns is Autopilot.

Every enabled session appears in exactly one derived column:

1. **Needs You**: open human decision, capability gap, worker stuck, or
   confirm-done request. A confirm-done card reads `Ready to close`; it is not
   machine-completed.
2. **Working**: a live session with confirmed active work, including a driven
   running wake.
3. **Waiting**: enabled and not working; includes monitoring countdowns, idle
   prompts, never-started rows, and unavailable terminals. The card says why it
   is waiting rather than pretending it is active.
4. **Paused**: explicitly disabled sessions.

There is no Done column. Only the human removes/closes a session.

## Card Content

> Superseded: shipped cards lead with the human task title, goal, or initial
> Message, and show session id, project, engine, and mode as secondary metadata.

Cards are compact and stable-height. They show:

- status glyph and session id;
- Standard or Autopilot ownership;
- one wrapped current/next summary;
- waiting age when a decision is open;
- `fork of <parent>` for forked sessions; and
- an offline label when the terminal is unavailable.

Cards do not repeat explanatory help text or expose raw state-file fields.

## Responsive Layout

> Superseded: the shipped breakpoints are 150+ columns for all five lanes, 105+
> for three, 72+ for two, and one lane below that, with no `< n/m >` position
> indicator.

- At 140 columns and above, show all four columns.
- At laptop widths, show two adjacent columns at a time. Left/Right changes the
  visible pair while preserving the selected card by stable session id.
- At very narrow widths, Board falls back to one column with an explicit
  `< 2/4 >` position indicator.
- Column and card dimensions are fixed for the frame so changing status text
  cannot shift navigation targets.

The existing grouped list remains the default first screen. Board is an explicit
overview, not a hidden responsive mode or a Tab-controlled concept.

## Interaction

> Superseded: a card click selects it in place and exposes its actions in the
> bottom menu; Enter opens Task detail; lane actions route through the ordinary
> handlers. In lane mode `s` opens Answer when the selected session has an open
> stop, opens Task detail with its composer on a Needs You card without one, and
> otherwise waits for Enter to open Task detail.

- Left/Right or `h`/`l`: move across columns.
- Up/Down or `j`/`k`: move among cards in the selected column.
- Enter or single click: select the card and return to its existing workbench
  detail.
- `n`: open the normal New Session form. Creating from Board creates the same
  session as creating from the workbench.
- `f`: fork the selected card through the normal fork handler.
- `s`, `m`, `p`, `r`, `a`, and remove remain workbench actions. Board can expose
  them in a context action row later, but must route through the existing
  handlers and preconditions.
- Mouse hit regions are rebuilt from every frame. A click commits by stable
  session id, never by a stale vector index.

Drag-and-drop between status columns is intentionally absent: the columns reflect
runtime truth, and dropping a card onto Working or Needs You has no honest single
state transition. Explicit Mode, Pause, Answer, and Message actions remain the
controls that change behavior.

## Ad Hoc Work

> Superseded: Task New also persists the request's first line as a human-owned
> task title and returns to Task view with the new card selected.

The Board's New action creates a normal session with the existing optional
initial Message. This is the ad hoc work path: describe the work, choose the
project/engine, and the resulting card enters Working or Waiting from observed
state. Autopilot remains opt-in through the existing control.

A fork is the context-preserving ad hoc path. It creates another Standard card in
the same project with lineage, then moves independently as its session runs.

## Acceptance

- Board column membership is a pure exhaustive projection over `ProjectView`.
- Every session appears once; no card exists without a registry session.
- Refresh and status changes preserve selection by session id.
- Keyboard and click card selection use one handler.
- New and Fork invoked from Board create ordinary managed sessions.
- `80x24`, `100x28`, `120x32`, and responsive boundaries render without hidden
  selected cards, overlap, or clipped column indicators.
- The pmtui dogfood gallery captures Board in normal, Needs You, fork-lineage,
  and narrow viewport states.
