# Fleet oversight: what happened without you

Date: 2026-11-10
Status: design, not implemented

## The ask

Oversee anything the fleet did, and check any of it when you want to. The constraint is coherence:
this must fall out of what pmtui already has, not arrive as a second paradigm beside it.

## The question no existing surface answers

| Surface | Answers |
|---|---|
| status bar tallies | how many rows are running, idle, needing you — **now** |
| `NEEDS YOU (n)` group | which sessions need you — **now** |
| decision lane (`v`) | one session's decider history, full screen, on demand |
| `pmtui.log` | a flat text tail of this dashboard's own statuses |

Every one of them is about the present, or about one session. None answers *"across my fleet, what
was decided without me?"* That is the oversight question for a fleet, and it is the complement of
everything already on screen.

**The complement is the whole design.** A surface that instead showed *what needs you* would
re-create the VERDICT row, which was deleted for restating the `NEEDS YOU` group it sat above —
with one needy session the same fact appeared three times. Oversight is not a fourth way to say
"look here"; it is the record of everything that did **not** stop to ask.

## Prior art: flightdeck's gate

`scasella/claude-flightdeck` is a Claude Code mod with the closest thing to this. Its `gate` panel
is one cell per permission check, coloured by **how it was decided** — green allowed without
asking, blue decided by the auto-mode classifier or by you and then run, amber pending, red denied,
dim when the call was made inside a subagent — with totals, a drill-down per tool family, and
credentials masked.

Three of its disciplines carry over, and one is new to us:

1. **The oversight layer only watches.** Stated as a boundary: "Every hook passes its event on
   unchanged: it never denies, rewrites or delays a tool call, a prompt or a subagent." This is
   already our rule for hooks; it is now confirmed by something shipping.
2. **Every number names the event it came from.** Their docs carry a literal `From` column
   (`turn.step`, `agent.spawn`, `tool.check`). "Every number comes from a real session event."
3. **A capability manifest, not a sandbox.** `claude plugin validate .` prints exactly what the
   plugin hooks and calls. That is the answer to "can I trust this hook" once hooks exist, and
   `pmtui doctor` is where it belongs here.
4. **NEW: "What is inferred, not measured."** A whole documented section. They do not try to make
   inference better — they **mark** it. An estimated compaction count renders `⟲~1`, the log says
   `context compacted (estimated)`, inferred architect moments are labelled `(inferred)`, and each
   case gets a paragraph saying why it cannot be known.

## Mark what is inferred

Point 4 lands on this repo's worst recurring defect. The status glyph means working, not merely
alive — and that rule has been broken repeatedly by conflating liveness with activity.

We now know the conflation is not a bug to out-engineer. OSC 7501 would give a **reported** state
from the agent itself, and tmux 3.6a swallows it (measured: a report emitted in a pane leaves
nothing in `capture-pane -e`, no format and no option). So pmtui has no choice but to infer
"working" from a pane until it owns the pty.

The honest response is therefore not a better predicate but a **truthful glyph**: one mark when the
agent told us (a turn signal fired, a marker was written, a receipt landed) and a visibly different
mark when the pane merely looks busy. A confident glyph over an inference is a lie; a tilde is not.

This is not a new invariant. It is the existing one — the glyph means working — rendered honestly
for the case where we cannot know. It ships first because it is small, independent of everything
below, and it corrects something users already see.

## The gate, in pmtui's own vocabulary

Nothing here is a new mechanism. Each piece is an existing one used one level up.

**The record is the counters that already exist.** Every session's state carries
`DecisionCounters` (`disposed`, `working`, `monitoring`, `auto_flow`, `escalated`, `stalled`). The
gate sums them over the rows on screen, following the status bar's own rule: "the tallies are
counted here rather than cached on `App`, so the number on screen is always over the rows on
screen." No new state, no cache, no new writer, and single-writer stays intact.

**A cell is an action, coloured by who decided it.** Not by severity — by authority: the human, the
decider, a deterministic policy, or nothing yet. That axis is what makes it oversight rather than a
second status display.

**Drill-down already exists.** Opening a cell opens the decision lane (`v`) for that session, which
already expands the question, the deterministic policy, the result, the reason and the duration.
No new full-screen view is built.

**Colour is the theme's.** Every hue comes from a role in `src/theme.rs`, and severity is carried by
glyph, word and badge as well as hue — the existing chrome invariants cover the gate for free.

## Where it lives

This is where the verdict row died, so it matters.

- **Not a new always-on row.** That is the verdict row's grave.
- **Not a fourth body panel.** The body already collapses three ways (list-only, stacked,
  two-column) and a fourth region competes with SESSIONS and PREVIEW at every width.
- **A third view**, reached as `Sessions` and `Tasks` already are. A view is "check if necessary" by
  construction: you go to it, so it costs no always-on space and it matches how `v` and the board
  already work.
- **Plus one status-bar chip** carrying a number the existing chips do not: decisions taken without
  you since you last looked. That is the "oversee anything" half — always visible, one number,
  and it is not the verdict-row mistake because the listing is not in the same frame.

## Grouping: the two views want opposite axes

SESSIONS already groups, by one axis: `RowGroup` is `NeedsYou → Autopilot → Standard → Paused`,
and `row_group` is pure over the view so the sort and the render cannot disagree. That axis is
URGENCY, and its ordering is load-bearing — "human-owned work outranks operating mode and terminal
availability", "an explicit pause remains authoritative even if the frozen posture still needs
attention."

The gate has no urgency axis **by construction**: everything in it is something that did not stop
to ask. Remove urgency and nothing organizes the list. So the gate groups by LINEAGE — a root
session and everything whose `spawned_by` chain reaches it — which is also how the work was
actually structured. Grouping is not an addition to oversight; it is what oversight needs instead
of urgency. That is why the two arrive together.

**Urgency stays primary in SESSIONS, and lineage must never become a container there.** A needy
child inside a collapsed parent group is less visible than a needy child at the top of the list,
and the product's one job is telling a human which agent needs them. Lineage is already a cue in
that view — `from <parent>` appears in the preview head and on the Task card — and a cue is the
right weight for it.

**For fleet focus in SESSIONS, filter rather than regroup.** Scoping the list to one coordinator's
fleet keeps the urgency ordering intact inside the scope, where regrouping would fight it. The `/`
switcher is the existing vocabulary for choosing what the list is about.

**The orphan group is real, and is itself an oversight finding.** Removing a row deletes only that
session's own state subtree, so deleting a coordinator leaves its children running with
`spawned_by` pointing at a row that is gone — `spawned_by_label` already falls back to the raw id
for exactly this case. Those rows need a home in the lineage grouping, and "these ran for someone
who is gone" is something a human should be shown rather than left to infer from a label that
stopped resolving.

## At fleet scale

Flightdeck changes representation rather than letting cards shrink: "cards side by side while they
fit… Beyond that, swimlanes on one time axis." Our Task board has the same problem at five-plus
children and no answer yet. The gate should take the same approach — dense cells while they fit, a
collapsed per-session summary beyond that — and the board is worth revisiting with it.

## What this must not become

- **A second place to find what needs you.** The complement, or nothing.
- **An authority.** The gate watches. It never answers a question, approves an action, or retires a
  row. If oversight could act, a human reading it could not trust it as a record.
- **A new writer.** It reads session state and registry rows. `pmd` remains the sole writer of
  `state.json`, the worker of `needs-you.json`, and pmtui of the human-owned files.
- **A durable archive.** The counters live as long as the session's row, like everything else under
  `.project-state/sessions/<seg>/`. Removing a row takes its history with it, by the same rule
  already written down.

## Order, and why

1. **Mark what is inferred.** Smallest, independent, and it corrects a visible lie.
2. **`pmtui ls --json`.** The read spine. A manager agent needs it, a status line wants it, and the
   gate's fleet aggregation is the same projection rendered instead of printed.
3. **The gate view and its chip.** Needs 2 for its aggregation and benefits from 1 for honesty.
4. **The capability manifest in `doctor`.** Only once hooks exist; nothing to declare before that.

## Verification this needs

- An inferred glyph and a measured glyph must be distinguishable on a real pane, captured. A unit
  test asserting two different enum values proves nothing a human sees.
- The gate must be shown NOT to restate the `NEEDS YOU` group: a test with one needy session that
  asserts the fact appears once, which is the regression the verdict row was deleted for.
- Aggregation must be counted over rows on screen, with a test that a removed row's counts leave
  the total — the status bar's rule, held to.
- The real-tmux acceptance must drive the view at five-plus sessions, because legibility at scale
  is the whole question and no unit test answers it.
