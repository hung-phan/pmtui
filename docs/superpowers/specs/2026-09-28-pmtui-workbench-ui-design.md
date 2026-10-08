# pmtui Workbench UI Design

> **Superseded.** This is the historical design note for the browser prototype
> (`docs/PMTUI-WORKBENCH-PROTOTYPE.html`). The shipped terminal dashboard is specified by
> `docs/SPEC.md` (section 3) and `2026-09-28-board-native-workspace-design.md`; where they differ,
> they win. Two decisions below did not ship: `/` is a quick switcher that selects existing
> sessions only (no command palette, no action results), and a Task view with human-owned task
> titles shipped as a second projection over managed sessions. Statements that contradict the
> shipped contract are marked inline.

## Purpose

pmtui is an operator console for people supervising several long-running Claude and Codex
sessions. The redesign must make repeated scanning, intervention, and return-to-work faster while
preserving the product's central model: one managed session owns one persistent terminal, and
Autopilot interrupts the human only when policy or missing context requires it.

The HTML artifact is an interaction prototype. It validates hierarchy, density, responsive
behavior, and visual language before those decisions are translated into Ratatui constraints.

It is not a pixel-for-cell port. Ratatui should compress fleet health plus selected-session state to
at most three terminal rows, keep rail entries to one row with whole-field shedding, and retain the
existing full-screen audit rather than spending terminal width on a permanent drawer. The browser
drawer prototypes layering and continuity, not the final terminal geometry.

## Direction

- **Tone:** calm, technical, precise, and active without looking noisy.
- **Audience:** an engineer repeatedly checking progress, answering one decision, and returning to
  other work.
- **Memorable detail:** the selected session reads as one continuous workspace from status through
  transcript to composer, while session attention remains visible in a compact rail.
- **Palette:** neutral charcoal surfaces with semantic emerald, amber, coral, cyan, and steel-blue.
  Color communicates state; it is not decoration.
- **Density:** information-rich rows and bands, no marketing hero, oversized cards, nested cards,
  gradient decoration, or ornamental illustration.

## Desktop Layout

The viewport is one fixed workbench with three horizontal bands.

1. **Fleet bar:** prominent pmtui identity, fleet counts, pmd health, and one command-palette entry.
   *(Superseded: the shipped top-right controls are `/` Switch, `Tab` view, and `n` New.)*
2. **Work area:** session rail on the left and the selected session workspace on the right.
3. **Action bar:** compact global actions and current keyboard equivalents.

### Session Rail

The rail contains:

- an `All / Attention / Autopilot` segmented view;
- grouped rows for `Needs you`, `Autopilot`, `Standard`, and `Paused`;
- one-column semantic state markers;
- session id, concise current status, engine, elapsed time, and optional pin;
- a selected treatment that remains visible without replacing the state color; and
- click selection through the same conceptual route as keyboard selection.

Rows remain sessions, never tasks or tickets. Grouping and pins are views over existing sessions.
*(The Session rail still lists sessions only; the shipped Task view is a separate projection whose
cards are the same managed sessions.)*

### Selected Workspace

The workspace is visually dominant and unframed inside its column. It contains:

- session identity, project path, engine/model, and mode control;
- contextual attach, pause, restart, and overflow actions;
- one compact status band for goal, current result, next action, and elapsed time;
- an optional attention decision band with fully wrapped question and radio choices;
- the live transcript as the largest region; and
- a persistent composer anchored above the global action bar.

The transcript is the subject, not a preview card. Terminal output uses restrained syntax colors and
tabular counters. Empty, loading, disconnected, and waiting states state the truth directly.

### Persistent Composer

The selected session's composer is always visible. In its resting state it is compact; click or `s`
focuses the field. It preserves the existing per-session draft behavior and exposes send plus editor
handoff without covering the transcript.

## Interaction Model

- Clicking a session selects it; selection does not attach or alter its runtime.
- `/` and the fleet-bar search control open one command palette.
- The palette searches sessions first and contextual actions second.
  *(Superseded: shipped `/` opens a quick switcher that filters and selects existing sessions
  only; it lists no actions and changes no registry or task state.)*
- Context action buttons call the same conceptual commands as the existing keys.
- The mode control changes Standard/Autopilot only after the existing goal/cadence requirements.
- Decision options are radio controls; submitting follows the existing answer path.
- The HTML audit opens as a right-side drawer to test layered continuity; the Ratatui view remains
  full-screen with a compact global-attention indicator.
- Motion is limited to 120-180ms selection, palette, and drawer transitions and is disabled by
  reduced-motion preferences.

## Narrow Layout

The Ratatui port retains its established responsive fallback: very narrow terminals show the
Sessions list, while medium widths stack Sessions above the selected detail panel. The browser-only
Sessions/Workspace switch is not part of the terminal product.

## Prototype States

The artifact includes realistic session data for:

- a hard `Needs you` decision with wrapped radio choices;
- an Autopilot session actively working;
- an Autopilot session reviewing;
- a Standard idle session;
- a paused session; and
- a disconnected or stale session.

It supports session selection, view filters, command-palette search, mode toggling, contextual action
feedback, decision submission, audit drawer toggling, composer focus, and simulated message send.

## Safety and Accessibility

- State is never communicated by color alone.
- Focus rings remain visible, and controls are reachable by keyboard.
- Updating counters use tabular numerals and fixed dimensions.
- Hit areas are at least 40px in the HTML prototype and map to stable terminal cells later.
- The prototype performs no filesystem, process, tmux, registry, or network action.
- No control claims an action succeeded without visible simulated feedback.

## Non-Goals

- Introducing Task, Ticket, Issue, queue, worktree, or multi-pane lifecycle concepts.
  *(Superseded for Task: a later decision shipped the Task view, where each task is one managed
  session with a human-owned title. Ticket, Issue, queue, worktree, and multi-pane concepts remain
  out of scope.)*
- Replacing the real agent terminal with a custom transcript protocol.
- Adding decorative dashboards, analytics charts, avatars, illustrations, or marketing content.
- Porting the prototype directly to Ratatui before desktop and narrow screenshots are reviewed.

## Acceptance

- The first viewport is the usable pmtui workbench and makes brand, attention, selected work, and
  composer visible.
- Session state can be scanned without reading every row.
- The transcript owns most workspace area at common desktop dimensions.
- The 820px and 390px layouts expose both Sessions and Workspace with no overlap or hidden text.
- Every displayed command has a working prototype interaction or is visibly disabled.
- Keyboard selection, `/`, Escape, composer send, and decision submit work in the artifact.
- Browser screenshots show no clipped text, horizontal scrolling, accidental nested cards, or
  decorative one-note color treatment.
