# Unified turn trace

**Date:** 2026-08-30
**Status:** implemented

## Goal

Make `v` answer why Autopilot acted, what the worker reported, and how the wake ended without
weakening or duplicating the existing decider audit.

## Decisions

- Keep `v` per-session and read-only.
- Preserve **Decisions** as the default tab; **Tab** switches to **Turns**.
- Record turns only after successful terminal input. Failed sends and ordinary polling never create
  phantom turns.
- Correlate a heartbeat to the next accepted marker using the marker sequence baseline.
- Keep held wakes in the existing coalesced event feed; they explain why pmd withheld input without
  changing the consume-no-state defer contract.
- Store capped status and next-step text, but no prompt bytes, hidden reasoning, or unrestricted
  transcript data.
- Bound the pmd-owned trace to 64 entries and load both the container and individual entries
  leniently so audit corruption cannot prevent the operational ledger from loading.
- Keep scheduling and authority independent of trace state.

## State

Each trace entry has a monotonic pmd-owned ID, start time, marker baseline, heartbeat trigger flags,
and one outcome:

- awaiting report;
- reported with marker sequence, worker state, disposition, status, next step, and end time; or
- no report with a bounded reason.

Old ledgers default to an empty trace. A new heartbeat supersedes an older unreported trace entry.
A report awaiting decider review is initially `Reviewing`. It becomes `AutoFlow` only after every
queued decision from that report resolves, or `Escalated` when any reviewed decision reaches the
human. A newer report, human answer, or terminal relaunch that cancels the consult records
`Interrupted`. The report timestamp remains the worker response time; review latency stays in
Decisions.

## UI

The audit view keeps independent scroll positions for Turns and Decisions. Turns render oldest to
newest and follow the tail by default. Decisions retains the existing structured consult display and
queue. Both tabs use the same per-session "new since last review" watermark.
Legacy nudge/report events older than the first structured trace remain visible after upgrade;
mirrored events at or after the first trace are hidden to avoid duplicate rows.

## Verification

- state round-trip, lenient loading, text caps, monotonic IDs, and ring bounds;
- no trace before a successful nudge;
- Working, Monitoring, Reviewing, AutoFlow, Escalated, Interrupted, and Stalled report
  dispositions;
- view projection, tab switching, independent scrolling, tiny terminals, and existing decision
  rendering;
- isolated real-tmux acceptance that opens `v`, switches tabs, and reads a correlated report.
