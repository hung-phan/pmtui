# pmd-owned report identity and progress design

## Problem

Worker-authored timestamps were used as the durable freshness watermark. One future timestamp could
make every later valid marker look stale. Separately, a healthy foreground turn could produce
changing tool output for longer than the Busy timeout but still be escalated because only a marker
reset the inactivity window.

## Decision

Report identity and activity progress are independent signals:

- pmd assigns a monotonic generation to each newly observed atomic marker revision;
- file metadata plus a stable SHA-256 semantic fingerprint deduplicates that exact revision across
  daemon restarts while still counting a deliberate identical rewrite as a new wake;
- worker `seq` remains full-fidelity audit data and has no scheduling or authority role;
- nudge debt, turn correlation, and decider correlation use pmd generation; and
- normalized pane transcript changes restart only the Busy inactivity window.

The pane fingerprint excludes the composer/footer and recognized live spinner or interrupt rows.
Elapsed seconds, token counters, rotating placeholders, and footer clocks therefore cannot simulate
progress. New command output, tool results, and response text remain observable.

## Recovery

An older ledger without a recorded revision treats its currently represented marker as consumed.
A different current marker is accepted regardless of worker timestamp direction. The first accepted
report seeds pmd generation from the existing disposed-report counter, so a poisoned session recovers
after its human-owned stop is answered without editing the ledger.

## Safety

Pane progress does not clear report debt, resolve decisions, change cadence, or authorize input. It
only postpones the inactivity escalation. A completed-turn signal without a marker must still pass
the two-observation stable-idle gate before a corrective nudge can be sent.

Rejected alternatives:

- asking the model to write timestamps more carefully, because correctness cannot depend on prompt
  compliance;
- increasing the timeout, because it delays detection without distinguishing work from a wedge;
- using raw spinner/footer changes as progress, because their clocks advance while work is stuck;
- using Git changes or external monitoring, because they do not describe terminal activity and add
  unrelated authority.
