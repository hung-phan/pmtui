# Direct Dispatch and Pointer Parity Design

## Problem

pmtui already has the correct runtime model: one managed session owns one persistent interactive
Claude or Codex terminal. Adding a separate Task entity made the create flow harder to explain while
duplicating none of the behavior users actually wanted. The missing capability is interaction:
start a session with work already queued, then select, enter, and operate it naturally with either
keyboard or mouse.

## Research Synthesis

The design was compared against Orca, Herdr, and Multica.

- **Herdr:** terminals remain the primary object; keyboard and mouse are both first-class, and a
  click focuses the real pane without changing its lifecycle. pmtui adopts pointer parity over its
  existing session and preview surfaces, not Herdr's general-purpose split-pane manager.
- **Orca:** status groups and direct contextual actions reduce navigation cost. pmtui already has
  stronger automation status and adopts direct visible actions through clickable keybar chips and
  a `/` quick switcher over the same session rows.
- **Multica:** work begins from a composer inside a durable conversation, while projects and issues
  are separate product entities. pmtui adopts the composer lesson for both the optional initial
  Message and a non-modal selected-session composer; it does not pretend to have an issue tracker
  or introduce Task versus Session terminology.

## Product Shape

There remains exactly one workload: a managed session.

### Direct Dispatch

The Standard create form exposes and initially focuses an optional `Message` using the existing
text/editor surface. Configuration remains visible but does not precede intent.

- Empty: launch the normal interactive terminal at its prompt.
- Non-empty: append the message as the fresh CLI's initial positional after `--`.
- Claude and Codex use their existing interactive create commands; no `-p`, `exec`, or detached
  helper is introduced.
- The registry records the original message for audit, but only the create path submits it.
- An immediate launch error may retain one in-memory first-launch retry; successful launch clears
  it, and persisted provenance never makes a later fresh Codex terminal replay the message.
- Restart and resume continue the conversation without the message.
- Autopilot continues to use Goal and the protocol-framed first wake, so Message is hidden there.

### Pointer Parity

The render pass publishes hit regions from the same layout objects it draws:

1. session-row regions select rows;
2. the selected preview title region invokes the existing Enter/attach route, in the Session view
   and in open Task detail, while the transcript body stays inert;
3. the dormant composer shelf routes a click through the ordinary `s` handler, so it opens Message,
   or Answer when the selected session has an open stop;
4. create-form summary rows focus their field only;
5. fitted keybar chip regions carry a typed key action and call the existing key handler;
6. the persistent top-right `/` Switch, `Tab` view, and `n` New controls carry the same typed key
   actions in both the Session and Task views (below 72 columns as key-only badges while they fit
   beside the whole daemon chip);
7. quick-switcher result rows commit through the same method as Enter;
8. Task cards, keyed by stable session id, select their session without leaving the lanes; and
9. Task columns are wheel targets that move the card selection, not click targets.

Elsewhere the wheel acts on the pane under the pointer: over the sessions list it moves the selection
while no composer is open, and over the Status log or detail pane it scrolls. With the composer open
a sessions-list wheel does nothing, so the composer stays bound to its session. The Answer overlay
and the audit view keep their own wheel scrolling.

No pointer handler reimplements business behavior. Confirmations, disabled actions, paused resume,
Autopilot ownership, and input locking remain in the keyboard-owned action methods.

### Inline Composer

`s` activates the composer panel at the bottom of the selected detail pane, below the transcript,
while leaving the session list and transcript visible. On roomy screens it is eight rows, matching the
Status log (two borders and six text rows); compact layouts shrink the active composer, down to three
rows, so the preview keeps six rows where it can, and a very narrow list-only body gives way to it as
`docs/SPEC.md` describes. When the detail pane has room for the full composer plus a sixteen-row
preview, a dormant shelf for the selected session stays visible between sends and routes a click
through `s`. It still targets the one existing project terminal and converges on the existing send
planner and delivery method.

- The open composer stays bound to the session it opened on: no key and no sessions-list wheel
  moves the selection, while the detail and Status panes still scroll under the wheel.
- Draft state is keyed by session id in pmtui memory.
- Escape parks the current field, including its caret; reopening restores it.
- A successful delivery removes the draft.
- Refusal and delivery errors keep the field open and unchanged.
- Cancelling or failing `$EDITOR` restores the seed as that session's draft.
- Removing a session removes its draft. Exiting pmtui does not persist or replay drafts.

The composer is a dashboard interaction surface, not worker state. It writes no registry, ledger,
marker, brief, directive, or checkpoint data.

### Quick Switcher

`/` opens one search overlay over existing managed sessions. Filtering is case-insensitive over the
session id, its human display name, and the registered project directory, while result ordering
stays identical to the current attention-first dashboard order. A switch opened from Task view
returns there after selection or cancel.

- The current session is highlighted when the query is empty.
- Typing narrows results; Up/Down and Page keys move the result cursor.
- Enter or a result click selects that same existing dashboard row and closes the overlay.
- Escape closes without changing selection.
- No match is an honest empty state; Enter is a no-op.
- The query is ephemeral and causes no filesystem or tmux mutation.

## Safety and Responsive Rules

- Hit regions are cleared at the beginning of every frame and rebuilt only for rendered controls.
- Hidden create fields and model-list detail rows publish no focus target.
- A click never cycles a toggle or selects a model; it first focuses the summary row.
- Composite hints such as `↑↓/jk`, separators, clipped chips, and status text are inert.
- Clicking an empty preview uses the normal honest Enter refusal.
- Clicking through an overlay cannot reach the dashboard behind it.
- Initial messages travel in the terminal's launch command, which tmux sends to its server as one
  16 KiB message. The whole shell-quoted launch command is therefore bounded at 12 KiB
  (`LAUNCH_COMMAND_MAX_BYTES`), leaving room for the session name and working directory, and intake
  refuses a longer Message before any launch. Control bytes continue through the existing
  field/paste sanitization.

## Acceptance

- Claude and Codex Standard sessions receive a non-empty initial Message exactly once.
- Empty Standard creation is byte-compatible with the existing launch; Autopilot never receives a
  Message argument.
- Restart and resume never replay the initial Message.
- Row, preview, create-field, and keybar clicks match the corresponding keyboard outcomes.
- Composer drafts survive cancel, selection changes, and editor cancellation per session, then clear
  only after successful delivery or session removal.
- `/` filters by session id, display name, and project directory, selects by stable session id after
  refresh, and leaves the old selection unchanged on cancel or no match.
- Hidden, shed, stale, and overlay-covered hit regions are inert across narrow, wide, and resized
  layouts.
- Real tmux proves create with Message, selected preview, click-to-attach, detach, and continued send
  all use one persistent project terminal.

## Deferred

Task/project entities, ticket lifecycle, detached execution, queues, worktrees, and multi-pane
terminal management are outside this change. (A later change added the Task view as a second
projection whose cards are these same managed sessions; see
`2026-09-28-board-native-workspace-design.md`.) They require independent user problems and must not
be inferred from the desire to start, find, and interact with a normal session more easily.
