# Milestone 5 — Driving the Real `project-manager` Coordinator

**Date:** 2026-08-12
**Status:** SUPERSEDED / WITHDRAWN. The owner redirected M5 away from driving the
installed skill: agent-manager should *be* the harness (own the phase machine in
Rust, borrow the skill's playbook prompts as text, no runtime skill dependency, no
API key — drive the authenticated `claude`/`codex` CLI per phase). See the current
design at [`2026-08-12-m5-native-harness-design.md`](2026-08-12-m5-native-harness-design.md).
This draft (a "projection shim" that spawned the skill and needed a
`PM_EXTERNAL_DRIVER` seam in it) is kept only for decision history.

Follows on from the approved daemon design
([`2026-08-12-agent-manager-daemon-design.md`](2026-08-12-agent-manager-daemon-design.md),
§13 Milestone 5).

## 1. Problem

`pmd` today drives `reference-coordinator.sh` — a toy bash coordinator that proves
the loop. The real coordinator is the installed **`project-manager` skill**: a
mature phase machine (`intake → research → design → plan → implement → review →
cr → confirming → done`) that delegates code work to bounded `claude -p` /
`codex exec` workers, tracks everything in `.project-state/`, and already
distinguishes *in-flight* (`monitoring`) from *needs the human* (`needs_you`).

Two things must be reconciled to let `pmd` drive it end-to-end:

1. **Self-drive vs. external drive.** The skill drives *itself* via an in-session
   `claude-cron` heartbeat (`state.session`, `CronCreate`, `pm-harness.sh`). The
   approved design makes `pmd` the **full external driver** — "there is no
   in-session cron/Stop-hook loop." So the skill must run one pass and exit
   *without* scheduling its own next wake; `pmd`'s next spawn is the heartbeat.
2. **Two state shapes.** `pmd`'s step contract uses a simplified split —
   `step.json` (coordinator intent), `driver.json` (daemon observations),
   `stops.json` (each stop carries a `risk_class`), `answers.json`. The skill's
   native authority is `state.json` (`run.posture`, `phase`, `open_stops[]` with
   no `risk_class`, `session`, approvals, operations). Something must translate.

This milestone also delivers the **autonomy behavior the whole project exists
for**: the tier is injected into each step so the coordinator self-suppresses
defensible low-stakes forks instead of stopping for them.

## 2. Goals / Non-goals

**Goals**
- `pmd` drives a real `project-manager` project through at least one full phase
  transition and one tier-gated fork, with `pmd` as the sole heartbeat.
- The tier reaches the coordinator and demonstrably changes stop behavior
  (autopilot auto-flows a low-stakes fork that guardian escalates).
- Stops the skill raises surface in `pmtui` with a correct `risk_class`, and the
  daemon's hard-kind safety net (§8 of the base design) still holds.
- No regressions to `pmd`'s M1/M3 contract; the reference coordinator keeps
  working (it stays the integration-test fixture).

**Non-goals**
- Rebuilding the skill's phase logic, worker routing, or comms in Rust.
- Multi-worker orchestration changes inside the skill.
- Replacing the skill's Slack/expert paths; `pmtui` answering a stop is enough
  for M5. The skill's existing comms keep working when configured.

## 3. Approaches

### A. Projection shim (recommended)

A small wrapper command — `pm-coordinator-step.sh` — becomes the project's
`coordinator_cmd`. Each step it:

1. **Runs the skill for one pass.** Invokes `claude -p` (or `codex exec`) with a
   fixed prompt that boots the `project-manager` skill, does one bootstrap +
   progress-loop cycle, injects the tier (`--tier`), and **suppresses the
   heartbeat** (§4). Bounded by a hard timeout below `config.step_timeout_s`.
2. **Projects the result.** Reads the skill's `state.json` and writes `pmd`'s
   contract files: `run.posture → step.json.status`
   (`working→awaiting_next`, `monitoring→monitoring` (+ `next_check`),
   `needs_you→needs_you`, `done→done`); `open_stops[] → stops.json`, deriving
   `risk_class` from `kind` (§5). Bumps `step.json.id` only on a real advance.
3. **Feeds answers back.** Before the pass, drains `pmd`'s `answers.json` into the
   form the skill's reply-handling expects (a resolved `open_stops[]` entry +
   `decisions.md` line), so the next skill pass consumes the human's answer.

**Trade-offs.** `pmd` is unchanged; the skill is unchanged except the one
heartbeat hook (§4). The projection is a pure, unit-testable mapping. Cost: two
schemas of record kept in sync by the shim; the shim owns the translation
invariants. This is the smallest honest surface and is recommended.

### B. Native-schema driver

Teach `pmd`'s `state` module to read the skill's `state.json` directly
(`run.posture`, `open_stops[]`), dropping the separate `step.json`; `pmd` writes
answers in the skill's shape.

**Trade-offs.** One schema of record (more faithful, no shim). But it couples
`pmd` to the full skill schema and re-implements the skill's invariants
(checkpoint seal, lease, cursors) on the reader side; a schema change in the skill
breaks `pmd`. Rejected for M5 — too much coupling for the first real integration.

### C. Reimplement a single-step coordinator

Write a fresh tier-aware, single-step coordinator (Rust or bash) from scratch.

**Trade-offs.** Full control, no skill dependency — but throws away the skill's
mature phase machine, worker routing, comms, and crash recovery, and re-earns all
their bugs. Rejected.

## 4. The one decision that needs you: heartbeat ownership

The skill's bootstrap Step 10 registers a `claude-cron` job (or `/loop`
fallback) so it wakes itself. With `pmd` as the driver that must not happen.
Options:

- **(i) Starve the scheduler.** Run the pass in an environment where `CronList`/
  `CronCreate` are absent, so the skill takes its `engine:"loop"` fallback. But
  the fallback still *is* a self-drive mechanism (`/loop`), so this doesn't
  actually cede control to `pmd` — it just changes which self-driver runs.
  Fragile and dishonest. Not recommended.
- **(ii) An additive, opt-in skill hook (recommended).** The skill honors an env
  flag — `PM_EXTERNAL_DRIVER=1` — that makes Step 8/10 **skip** heartbeat
  registration/reconciliation and treat the external spawn as the wake. It's
  additive (default behavior unchanged), tiny, and makes the hand-off explicit.
  The base design scoped "don't edit the skill" to *that* project; M5 is exactly
  where an external driver earns a minimal, well-named seam in the skill.

**Why this needs you:** it crosses the "don't touch the installed skill" line, and
you own that skill. Recommendation: **(ii)**. Please confirm before implementation.

## 5. Risk-class mapping

The skill's `open_stops[].kind` maps to `pmd`'s `RiskClass`, and the daemon's
hard-kind floor (base design §8) still overrides:

| skill `kind` | `risk_class` | Rationale |
|---|---|---|
| `publish`, `confirm_done` | **hard** | External/irreversible or the project-boundary gate — always escalate. |
| `worker_stuck`, `stuck` | medium | A stalled worker/fix loop; escalate unless autopilot. |
| `expert_needed` | medium | Needs a human/domain owner. |
| `ambiguity` | medium by default | The coordinator may self-assess a *defensible* fork as `low` and resolve it under tier, logging to `decisions.md`; only a genuinely consequential ambiguity stays medium. |

The shim writes the mapped `risk_class` onto each projected stop; `pmd`'s existing
`ALWAYS_HARD_KINDS` safety net re-checks it, so a mis-mapped hard stop still
escalates. `publish`/`deploy`/`merge`/`land`/credential/payment/destructive
remain hard regardless of tier.

## 6. Tier injection

- `config.json` gains `autonomy` (already in `pmd`'s `Config` and honored by
  `pmtui`'s tier control). The skill reads it and injects the tier into the
  progress loop.
- The step prompt carries the tier explicitly (belt-and-suspenders with the
  config), so the coordinator's judgment (base design §8) has it in-context:
  autopilot resolves low+medium forks and logs them; standard resolves low;
  guardian escalates anything non-trivial.
- Judgment stays in the coordinator; the daemon stays deterministic and only
  re-checks the risk class.

## 7. Lease & concurrency

The skill already owns a coordinator lease (`pm-lock.sh`). To avoid two lease
schemes fighting:

- **Coordinator exclusivity** stays the skill's `pm-lock.sh` lease (one pass at a
  time within `.project-state/`).
- **`pmd`'s guard** (in-memory `RunState`, plus the M4 cross-process daemon lease)
  only prevents `pmd` from double-*spawning* a step; it does not manage the
  coordinator lease. The two are layered, not competing: `pmd` won't spawn a
  second step while one is in-flight, and even if it did, the skill's lease would
  reject the second pass.
- The step's hard timeout must be shorter than the skill's `coordinator_lease_s`
  so a killed pass can't hold a stale lease past the next spawn.

## 8. Milestones (build order for M5)

1. **Heartbeat seam.** Implement decision §4 (assuming (ii)): the additive
   `PM_EXTERNAL_DRIVER` hook in the skill; prove a one-pass run schedules no cron.
2. **Projection shim — read path.** `pm-coordinator-step.sh` runs a real pass and
   projects `state.json → step.json/stops.json`. Unit-test the pure projection
   with fixture `state.json` files (posture + each stop kind).
3. **Answer path.** Drain `pmd` `answers.json` into the skill's reply-handling;
   prove a `pmtui` answer resolves a real stop and the next pass advances.
4. **Tier behavior.** End-to-end: the same fork auto-flows under autopilot and
   escalates under guardian. This is the project's headline demo.
5. **Integration test.** A gated real test (like M1's) driving a minimal real
   project one transition, self-skipping when `claude`/`tmux` are unavailable.

## 9. Testing

- **Unit** — the projection mapping (`state.json → step/stops`, every posture and
  `kind`), pure and exhaustive; the answer-drain transform.
- **Integration** — one gated end-to-end pass over a real minimal project;
  assert no self-scheduled cron, correct posture projection, and one tier-gated
  fork resolving both ways.
- **Fault injection** — kill the pass mid-flight; assert the skill's lease frees
  and `pmd` re-spawns from the intact checkpoint (no double-drive).

## 10. Open questions (resolve in planning)

- **`claude -p` one-pass contract.** Exact prompt/flags that make the skill do
  *one* bootstrap+progress cycle and exit cleanly, without its Stop-hook
  re-entering. Needs a spike against the installed skill.
- **`codex exec` parity.** Whether the shim supports both engines in M5 or starts
  `claude`-only.
- **Projection of `monitoring` next_check.** The skill's `run.next_check` vs.
  `pmd`'s poll cadence — reconcile so `pmd` doesn't poll faster than the skill
  intends.
- **Where the shim lives.** In this repo (`scripts/`) vs. alongside the skill.
  Leaning: this repo, so `pmd` owns its own integration surface.
