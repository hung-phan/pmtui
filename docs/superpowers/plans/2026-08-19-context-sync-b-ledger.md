# Milestone B — the decider-lane ledger — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the single-writer `state.json` (`AgentLoopState`) with monotonic digest counters, a bounded/coalesced `decisions` slice, and a `situation` snapshot, plus two side files written on the dispose path — a size-rotated full-fidelity `raw.jsonl` and a sparse, source-attributed `decisions.md` — so the deciding (pmd/autopilot) side has a durable, machine-readable record of what it has done.

**Architecture:** All new state rides the existing single-writer ledger; the unconditional part of the record (the `disposed` bump + `situation` snapshot + the `raw.jsonl` line) is emitted from ONE seam at the top of `dispose_report` (`src/job_engine/marker.rs`) so the two paths that skip the four `WakeState` arms — auto-flow→`spawn_advice`'s `parked` clone and blocked-no-stop→`park_stuck`'s clone — still carry it. The kind-specific fields (per-kind counter, summary, `stop_ids`, and the sparse `decisions.md` line) are set per-arm because only the arm knows the outcome. "The write IS the compaction" — there is no separate `digest.json`.

**Tech Stack:** Rust (edition 2024), `serde`/`serde_json`, `anyhow`; unit tests with the crate's `FakeDriver`/`FakeClock` fixtures under `src/job_engine/tests/`.

## Global Constraints

Every task's requirements implicitly include this section (values copied verbatim from `docs/superpowers/specs/2026-08-19-context-sync-bcde-design.md`, Milestone B + LOCKED DECISIONS):

- All new types are `#[serde(deny_unknown_fields)]`; the three new `AgentLoopState` fields are `#[serde(default)]` (+ `skip_serializing_if`) so old ledgers and the minimal-load test (`src/job.rs:551`) still parse (`deny_unknown_fields` rejects EXTRA keys, not MISSING ones).
- The single-writer atomic `state.json` is unchanged — the harness stays its sole writer; the agent stays the sole writer of `needs-you.json`. No new atomic primitive.
- Invariant: `digest.disposed == number of accepted marker bumps` — structurally guaranteed by the single top-of-`dispose_report` seam (bumped ONCE, before any `WakeState` branch).
- Side-file writes (`raw.jsonl`, `decisions.md`) are error-isolated (`if let Err(e) = … { eprintln!(…) }`) and never fail the `state.json` write. There is no append-atomic primitive (confirmed in `src/state/atomic.rs`), so `raw.jsonl` is size-check-then-rename-then-append; a torn final line is documented and tolerated (machine-only, lenient reads).
- `policy::decide_kind` (`src/job_engine/marker.rs:255`) stays byte-for-byte pure — B MUST NOT touch it.
- Counters are lifetime-monotonic (`saturating_add`); NEVER reset by `on_blocked`/`retime`. `disposed` is exact; per-kind counters are best-effort.
- Non-dispose park paths (`lenient_working` `marker.rs:356`, `max_wakes`, budget backstop) are NOT accepted bumps and MUST NOT bump `digest.disposed`.
- No separate `digest.json`.
- `raw.jsonl` ~2 MB + ONE rotated generation (`raw.jsonl.1`); `raw.jsonl` = full-fidelity machine JSONL.
- `decisions.md` notable set = `Escalated` + `Stalled` ONLY (AutoFlow/Working/Monitoring → `raw.jsonl` only); single-generation rotation (`decisions.md.1`); line format `- {epoch_seconds} pmd {kind}: {summary}`.
- The worker nudge is unaffected: `loop_nudge_prompt` is a pure function of agent-authored inputs; no B field may reach it (pinned by the extended firewall test).
- Tests are run with the `ECC_GATEGUARD=off` prefix (per project memory: `ECC_GATEGUARD=off cargo test`).

---

## Task 1: New ledger types + `AgentLoopState` fields + back-compat round-trip

**Files:**
- Modify: `src/job.rs:47-162` (add the three fields to `AgentLoopState`), `src/job.rs:440-460` (`fresh`), `src/job.rs:243-256` (add `de_decisions_lenient` beside `de_events_lenient`), `src/job.rs:196-256` (add the new types + consts near `AutopilotEvent`).
- Test: `src/job.rs` `#[cfg(test)] mod tests` (extend `agent_loop_state_round_trips_full_and_minimal` at `src/job.rs:547`; add `a_fresh_ledger_omits_the_decider_fields` and `a_bogus_decision_is_dropped_and_the_operational_ledger_loads`).

**Interfaces:**
- Produces:
  - `pub struct DecisionCounters { pub disposed: u64, pub working: u64, pub monitoring: u64, pub auto_flow: u64, pub escalated: u64, pub stalled: u64 }` with `pub fn is_zero(&self) -> bool`.
  - `pub enum DecisionKind { Working, Monitoring, AutoFlow, Escalated, Stalled }` (`snake_case`) with `pub fn as_str(&self) -> &'static str`.
  - `pub struct DecisionRecord { pub seq: Option<u64>, pub at: Epoch, pub count: u32, pub kind: DecisionKind, pub summary: Option<String>, pub stop_ids: Vec<String> }` with `pub fn at(now: Epoch, seq: Option<u64>, kind: DecisionKind, summary: Option<String>, stop_ids: Vec<String>) -> Self`.
  - `pub struct LedgerSituation { pub state: WakeState, pub status: Option<String>, pub open_stops: Vec<String>, pub seq: u64, pub at: Epoch }` with `pub fn from_report(report: &WakeReport, open_stops: &[String], now: Epoch) -> Self`.
  - `pub const DECISIONS_SLICE_MAX: usize = 64;`
  - `AgentLoopState.digest: DecisionCounters`, `.decisions: Vec<DecisionRecord>`, `.situation: Option<LedgerSituation>`.
- Consumes: `crate::clock::Epoch` (= `i64`), the existing `WakeState` (`src/job.rs:530`), `one_count` (`src/job.rs:214`), `cap_detail` (`src/job.rs:229`).

- [ ] **Step 1: Write the failing tests**

Extend the existing full/minimal round-trip test and add two new tests to `src/job.rs`'s `mod tests`. In `agent_loop_state_round_trips_full_and_minimal`, add these assertions to the MINIMAL block (after `src/job.rs:568`):

```rust
        // A pre-B ledger has none of the decider-lane fields; all default (empty/zero/None).
        assert!(st.digest.is_zero());
        assert!(st.decisions.is_empty());
        assert!(st.situation.is_none());
```

and add these three fields to the `full` struct literal (anywhere inside the `AgentLoopState { … }` at `src/job.rs:572`, e.g. right before `updated_at: 42,`):

```rust
            digest: DecisionCounters {
                disposed: 5,
                working: 3,
                monitoring: 1,
                auto_flow: 0,
                escalated: 1,
                stalled: 0,
            },
            decisions: vec![DecisionRecord::at(
                41,
                Some(100),
                DecisionKind::Escalated,
                Some("ship it?".into()),
                vec!["stop-a".into()],
            )],
            situation: Some(LedgerSituation {
                state: WakeState::Blocked,
                status: Some("need a decision".into()),
                open_stops: vec!["stop-a".into()],
                seq: 100,
                at: 42,
            }),
```

Then add two new tests:

```rust
    #[test]
    fn a_fresh_ledger_omits_the_decider_fields() {
        // skip_serializing_if keeps a fresh ledger loadable by an older daemon (the ledger
        // is deny_unknown_fields), and keeps an idle session's state.json unbloated.
        let json = serde_json::to_string(&AgentLoopState::fresh(Engine::Claude, None, 1)).unwrap();
        assert!(!json.contains("digest"), "{json}");
        assert!(!json.contains("decisions"), "{json}");
        assert!(!json.contains("situation"), "{json}");
    }

    #[test]
    fn a_bogus_decision_is_dropped_and_the_operational_ledger_loads() {
        // The decisions slice is machine-cosmetic: one unparseable entry (a kind a newer
        // pmd added, a hand-edit) must never take the operational ledger down with it.
        let mut st = AgentLoopState::fresh(Engine::Claude, Some(60), 1);
        st.last_marker_seq = 9;
        st.decisions.push(DecisionRecord::at(
            3,
            Some(7),
            DecisionKind::Escalated,
            Some("ship it?".into()),
            vec!["stop-1".into()],
        ));
        let mut v = serde_json::to_value(&st).unwrap();
        v.get_mut("decisions")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"at": 4, "kind": {"teleported": "mars"}}));
        let round: AgentLoopState = serde_json::from_str(&serde_json::to_string(&v).unwrap())
            .expect("the ledger still loads despite a bogus decision");
        assert_eq!(round.last_marker_seq, 9, "operational field intact");
        assert_eq!(round.decisions.len(), 1, "only the bad decision is dropped");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `ECC_GATEGUARD=off cargo test --lib job::tests`
Expected: FAIL — the crate does not compile (`DecisionCounters`, `DecisionRecord`, `DecisionKind`, `LedgerSituation`, and the `digest`/`decisions`/`situation` fields do not exist).

- [ ] **Step 3: Write the minimal implementation**

Add the new types + `de_decisions_lenient` + the const near `AutopilotEvent` in `src/job.rs` (e.g. after `AutopilotEvent`/`one_count` at `src/job.rs:216`):

```rust
/// Monotonic, saturating decider-lane counters kept on the ledger. `disposed` is the
/// EXACT count of accepted marker bumps (structurally guaranteed by the single dispose
/// seam); the per-kind counters are best-effort. `is_zero()` drives skip-serialize so a
/// fresh/idle ledger stays byte-identical to a pre-B one. All fields `#[serde(default)]`
/// so a future additive counter loads against an older ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DecisionCounters {
    #[serde(default)]
    pub disposed: u64,
    #[serde(default)]
    pub working: u64,
    #[serde(default)]
    pub monitoring: u64,
    #[serde(default)]
    pub auto_flow: u64,
    #[serde(default)]
    pub escalated: u64,
    #[serde(default)]
    pub stalled: u64,
}

impl DecisionCounters {
    /// True when every counter is 0 — the skip-serialize predicate.
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

/// The disposition class of one decision `pmd` made while driving. Snake-case on the wire
/// (`auto_flow`) so it reads the same in `raw.jsonl` and `decisions.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Working,
    Monitoring,
    AutoFlow,
    Escalated,
    Stalled,
}

impl DecisionKind {
    /// The token written into `decisions.md` (`- {epoch} pmd {kind}: {summary}`).
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionKind::Working => "working",
            DecisionKind::Monitoring => "monitoring",
            DecisionKind::AutoFlow => "auto_flow",
            DecisionKind::Escalated => "escalated",
            DecisionKind::Stalled => "stalled",
        }
    }
}

/// One decision `pmd` disposed, kept in the bounded [`AgentLoopState::decisions`] slice.
/// Consecutive entries with the same `(kind, summary, stop_ids)` coalesce into one with a
/// bumped `count`/`at` (see [`AgentLoopState::record_decision`]).
///
/// `seq` is `Option<u64>`: `Some` = an accepted marker bump; `None` is reserved for a
/// `Stalled` record raised on a NON-dispose park path (never a row with a bumped seq but
/// stale ids). B only ever writes `Some` (the non-dispose park paths do not record here
/// yet — documented as future).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub at: Epoch,
    #[serde(default = "one_count")]
    pub count: u32,
    pub kind: DecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_ids: Vec<String>,
}

impl DecisionRecord {
    /// One decision from an accepted marker bump (`seq` present), starting at `count == 1`.
    pub fn at(
        now: Epoch,
        seq: Option<u64>,
        kind: DecisionKind,
        summary: Option<String>,
        stop_ids: Vec<String>,
    ) -> Self {
        Self {
            seq,
            at: now,
            count: one_count(),
            kind,
            summary,
            stop_ids,
        }
    }
}

/// A snapshot of the last report `pmd` disposed — the current "situation" the supervisor
/// projection (Milestone C) reads. Projected fresh at spawn, but persisted here so it
/// never goes stale across a park (set by the single dispose seam on EVERY accepted bump).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSituation {
    pub state: WakeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_stops: Vec<String>,
    pub seq: u64,
    pub at: Epoch,
}

impl LedgerSituation {
    /// Snapshot the just-disposed `report` (its state/status/seq) plus the stop ids the
    /// session is open on at disposition time.
    pub fn from_report(report: &WakeReport, open_stops: &[String], now: Epoch) -> Self {
        Self {
            state: report.state,
            status: report.status.clone(),
            open_stops: open_stops.to_vec(),
            seq: report.seq,
            at: now,
        }
    }
}

/// The bounded length of [`AgentLoopState::decisions`] — the oldest are dropped past this.
pub const DECISIONS_SLICE_MAX: usize = 64;
```

Add `de_decisions_lenient` beside `de_events_lenient` (after `src/job.rs:256`):

```rust
/// Deserialize [`AgentLoopState::decisions`] LENIENTLY, exactly like [`de_events_lenient`]:
/// parse each element on its own and DROP any that fail, so one unparseable decision never
/// fails the operational ledger load.
fn de_decisions_lenient<'de, D>(d: D) -> std::result::Result<Vec<DecisionRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}
```

Add the three fields to `AgentLoopState` (after `events` at `src/job.rs:160`, before `pub updated_at: Epoch`):

```rust
    /// Lifetime-monotonic decider-lane counters (never reset by on_blocked/retime).
    /// `#[serde(default, skip_serializing_if)]` so a pre-B ledger loads (→ zero) and a
    /// fresh/idle ledger is not bloated by an all-zero object.
    #[serde(default, skip_serializing_if = "DecisionCounters::is_zero")]
    pub digest: DecisionCounters,
    /// The bounded, coalesced recent-decisions slice. Deserialized LENIENTLY (a single bad
    /// entry is dropped, never propagated) — this feed is machine-cosmetic, exactly like
    /// [`Self::events`].
    #[serde(
        default,
        deserialize_with = "de_decisions_lenient",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub decisions: Vec<DecisionRecord>,
    /// A snapshot of the last disposed report — the "situation" Milestone C projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub situation: Option<LedgerSituation>,
```

Init them in `fresh` (inside the `Self { … }` at `src/job.rs:441`, e.g. before `events: Vec::new(),`):

```rust
            digest: DecisionCounters::default(),
            decisions: Vec::new(),
            situation: None,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `ECC_GATEGUARD=off cargo test --lib job::tests`
Expected: PASS. Non-vacuity: the minimal-JSON case asserts the three fields DEFAULT (a missing `deny_unknown_fields` default would fail to parse); `a_fresh_ledger_omits_the_decider_fields` fails if `skip_serializing_if` is missing; `a_bogus_decision_is_dropped…` fails to even load if `de_decisions_lenient` is not wired (the whole ledger errors on the bad entry).

- [ ] **Step 5: Commit**

```bash
git add src/job.rs
git commit -m "feat(ledger-b): decider-lane types (DecisionCounters/Kind/Record, LedgerSituation) + back-compat fields"
```

---

## Task 2: `record_decision` — per-kind counter, coalescing, bounded slice

**Files:**
- Modify: `src/job.rs` `impl AgentLoopState` (add `record_decision` beside `record_event` at `src/job.rs:372`).
- Test: `src/job.rs` `mod tests` (add `record_decision_coalesces_and_bounds_the_slice`).

**Interfaces:**
- Produces: `pub fn record_decision(&mut self, entry: DecisionRecord)` — bumps the per-kind `digest` counter (NOT `disposed`; that is the seam's job), caps `summary` via `cap_detail`, coalesces on `(kind, summary, stop_ids)` against the last entry, and drains oldest past `DECISIONS_SLICE_MAX`.
- Consumes: `DecisionRecord`, `DecisionKind`, `DECISIONS_SLICE_MAX`, `cap_detail` (all from Task 1).

- [ ] **Step 1: Write the failing test**

Add to `src/job.rs`'s `mod tests`:

```rust
    #[test]
    fn record_decision_coalesces_and_bounds_the_slice() {
        let mut st = AgentLoopState::fresh(Engine::Claude, None, 0);
        // A RUN of the SAME (kind, summary, stop_ids) is ONE entry: latest time, bumped count.
        for at in 1..=3 {
            st.record_decision(DecisionRecord::at(
                at,
                Some(at as u64),
                DecisionKind::AutoFlow,
                Some("approve dprint".into()),
                vec!["stop-a".into()],
            ));
        }
        assert_eq!(st.decisions.len(), 1);
        assert_eq!(st.decisions[0].count, 3);
        assert_eq!(st.decisions[0].at, 3, "coalescing advances the timestamp");
        // The PER-KIND counter bumps each call; `disposed` is NEVER bumped here (the seam owns it).
        assert_eq!(st.digest.auto_flow, 3);
        assert_eq!(st.digest.disposed, 0, "record_decision must not bump disposed");
        // Divergent stop_ids create a NEW entry — never a coalesced row with stale ids (Missing-2).
        st.record_decision(DecisionRecord::at(
            4,
            Some(4),
            DecisionKind::AutoFlow,
            Some("approve dprint".into()),
            vec!["stop-b".into()],
        ));
        assert_eq!(st.decisions.len(), 2, "different stop_ids never coalesce");
        // Bounded: push > MAX DISTINCT entries → len == MAX, and the NEWEST survives the drain.
        let mut st2 = AgentLoopState::fresh(Engine::Claude, None, 0);
        let n = DECISIONS_SLICE_MAX as u64 + 5;
        for i in 0..n {
            st2.record_decision(DecisionRecord::at(
                i as Epoch,
                Some(i),
                DecisionKind::Working,
                Some(format!("s{i}")),
                Vec::new(),
            ));
        }
        assert_eq!(st2.decisions.len(), DECISIONS_SLICE_MAX);
        assert_eq!(
            st2.decisions.last().unwrap().summary.as_deref(),
            Some(format!("s{}", n - 1).as_str()),
            "the newest decision survives the bound"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test --lib job::tests::record_decision_coalesces_and_bounds_the_slice`
Expected: FAIL — `record_decision` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `impl AgentLoopState` in `src/job.rs` (right after `record_event`, `src/job.rs:391`):

```rust
    /// Record one disposed decision into the bounded [`Self::decisions`] slice, COALESCING a
    /// run of the same `(kind, summary, stop_ids)` into one entry (bump `at`, `count += 1`)
    /// exactly as [`Self::record_event`] does for the feed. Bumps the PER-KIND `digest`
    /// counter (best-effort); the exact `digest.disposed` bump is the dispose seam's job, so
    /// it is deliberately NOT touched here (a `Stalled` record raised on a non-dispose park
    /// path would otherwise over-count `disposed`). The harness is the sole caller.
    pub fn record_decision(&mut self, mut entry: DecisionRecord) {
        match entry.kind {
            DecisionKind::Working => self.digest.working = self.digest.working.saturating_add(1),
            DecisionKind::Monitoring => {
                self.digest.monitoring = self.digest.monitoring.saturating_add(1)
            }
            DecisionKind::AutoFlow => self.digest.auto_flow = self.digest.auto_flow.saturating_add(1),
            DecisionKind::Escalated => {
                self.digest.escalated = self.digest.escalated.saturating_add(1)
            }
            DecisionKind::Stalled => self.digest.stalled = self.digest.stalled.saturating_add(1),
        }
        // Cap the summary BEFORE comparing, so coalescing sees the stored (capped) form.
        entry.summary = entry.summary.map(cap_detail);
        if let Some(last) = self.decisions.last_mut()
            && last.kind == entry.kind
            && last.summary == entry.summary
            && last.stop_ids == entry.stop_ids
        {
            last.at = entry.at;
            last.count = last.count.saturating_add(1);
            // Keep the freshest accepted seq (a coalesced accepted bump overrides an older).
            if entry.seq.is_some() {
                last.seq = entry.seq;
            }
            return;
        }
        self.decisions.push(entry);
        if self.decisions.len() > DECISIONS_SLICE_MAX {
            let drop = self.decisions.len() - DECISIONS_SLICE_MAX;
            self.decisions.drain(0..drop);
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test --lib job::tests::record_decision_coalesces_and_bounds_the_slice`
Expected: PASS. Non-vacuity: if coalescing regressed to always-push, `decisions.len()` would be 3 not 1; if the `(kind,summary,stop_ids)` key ignored `stop_ids`, the divergent-ids case would coalesce to 1; if the bound were dropped, `len > MAX`. The `disposed == 0` assertion fails if someone wires the exact counter here (which the seam owns).

- [ ] **Step 5: Commit**

```bash
git add src/job.rs
git commit -m "feat(ledger-b): record_decision — per-kind counter, (kind,summary,stop_ids) coalesce, bounded slice"
```

---

## Task 3: Side-file paths (`raw.jsonl`, rotated siblings)

**Files:**
- Modify: `src/state/paths.rs` `impl ProjectPaths` (add `raw_jsonl`, `raw_jsonl_rotated`, `decisions_rotated` beside `decisions` at `src/state/paths.rs:76`).
- Test: `src/state/tests.rs` (extend `project_paths_have_expected_suffixes` at `src/state/tests.rs:127`).

**Interfaces:**
- Produces: `pub fn raw_jsonl(&self) -> PathBuf` → `<state_dir>/raw.jsonl`; `pub fn raw_jsonl_rotated(&self) -> PathBuf` → `<state_dir>/raw.jsonl.1`; `pub fn decisions_rotated(&self) -> PathBuf` → `<state_dir>/decisions.md.1`.
- Consumes: existing `state_dir()` (`src/state/paths.rs:43`), `decisions()` (`src/state/paths.rs:76`, reused unchanged for the human file).

- [ ] **Step 1: Write the failing test**

Extend `project_paths_have_expected_suffixes` in `src/state/tests.rs`:

```rust
    assert!(paths.raw_jsonl().ends_with(".project-state/raw.jsonl"));
    assert!(paths.raw_jsonl_rotated().ends_with(".project-state/raw.jsonl.1"));
    assert!(paths.decisions_rotated().ends_with(".project-state/decisions.md.1"));
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ECC_GATEGUARD=off cargo test --lib state::tests::project_paths_have_expected_suffixes`
Expected: FAIL — `raw_jsonl`/`raw_jsonl_rotated`/`decisions_rotated` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `impl ProjectPaths` in `src/state/paths.rs` (after `decisions`, `src/state/paths.rs:78`):

```rust
    /// `raw.jsonl` — the full-fidelity machine JSONL of every disposed decision (Milestone
    /// B). Append-only, size-rotated; the harness is the sole writer. Any reader MUST parse
    /// leniently and tolerate a torn final line (no append-atomic primitive exists).
    pub fn raw_jsonl(&self) -> PathBuf {
        self.state_dir().join("raw.jsonl")
    }
    /// The single rotated generation of [`Self::raw_jsonl`] (`raw.jsonl.1`).
    pub fn raw_jsonl_rotated(&self) -> PathBuf {
        self.state_dir().join("raw.jsonl.1")
    }
    /// The single rotated generation of [`Self::decisions`] (`decisions.md.1`), so even the
    /// sparse human file cannot grow unbounded over a multi-day session.
    pub fn decisions_rotated(&self) -> PathBuf {
        self.state_dir().join("decisions.md.1")
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ECC_GATEGUARD=off cargo test --lib state::tests::project_paths_have_expected_suffixes`
Expected: PASS. Non-vacuity: the suffixes are exact string suffixes, so a wrong filename (e.g. `raw.json` or `.jsonl.rotated`) fails.

- [ ] **Step 5: Commit**

```bash
git add src/state/paths.rs src/state/tests.rs
git commit -m "feat(ledger-b): raw.jsonl + rotated side-file paths"
```

---

## Task 4: The single dispose seam + append helpers + per-arm records

This is the atomic core: the `disposed`/`situation`/`raw.jsonl` seam at the top of `dispose_report`, the two error-isolated append helpers, and the per-arm `DecisionRecord` + `decisions.md` writes. It lands together because the helpers are only meaningful once the seam calls them (no dead code), and the seam is one indivisible change.

**Files:**
- Modify: `src/job_engine/mod.rs` (add `RAW_JSONL_MAX_BYTES`/`DECISIONS_MD_MAX_BYTES` consts; add `append_raw` + `append_decision_md` to `impl JobScheduler`, e.g. after `persist_run` at `src/job_engine/mod.rs:542`).
- Modify: `src/job_engine/marker.rs:15` (imports), `src/job_engine/marker.rs:23-25` (import the new const), `src/job_engine/marker.rs:120-340` (`dispose_report`: the seam + per-arm records + a `RawLine` helper struct).
- Test: `src/job_engine/tests/marker.rs` (add the six integration tests below).

**Interfaces:**
- Produces:
  - `const RAW_JSONL_MAX_BYTES: u64 = 2 * 1024 * 1024;`
  - `const DECISIONS_MD_MAX_BYTES: u64 = 256 * 1024;`
  - `fn append_raw(&self, line: &str, max_bytes: u64) -> Result<()>` (size-check-then-rename-then-append; `create_dir_all`).
  - `fn append_decision_md(&self, now: Epoch, kind: DecisionKind, summary: Option<&str>) -> Result<()>` (no-op unless kind is notable = `Escalated`/`Stalled`; rotates at `DECISIONS_MD_MAX_BYTES`; writes `- {now} pmd {kind}: {summary}`).
- Consumes: `DecisionKind`/`DecisionRecord`/`LedgerSituation`/`AgentLoopState::record_decision` (Tasks 1-2), `ProjectPaths::{raw_jsonl,raw_jsonl_rotated,decisions,decisions_rotated}` (Task 3), the unchanged `save_ledger`/`spawn_advice`/`park_stuck`. `policy::decide_kind` is NOT touched.

- [ ] **Step 1: Write the failing tests**

Add to `src/job_engine/tests/marker.rs` (these use the existing `marker_fx`/`write_marker`/`report_progress`/`backdate_marker`/`start_consult`/`ledger`/`BLOCKED_HARD` fixtures from `src/job_engine/tests/mod.rs`):

```rust
#[test]
fn digest_counters_are_monotonic_and_never_double_count() {
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    // Bump 1: a working report.
    report_progress(&fx, 7, 3);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).digest.disposed, 1);
    assert_eq!(ledger(&fx).digest.working, 1);
    // Bump 2: another working report (distinct, older-backdated mtime so it is re-observed).
    report_progress(&fx, 8, 2);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(ledger(&fx).digest.disposed, 2, "each accepted bump counts once");
    // SABOTAGE / anti-replay: re-observe an EQUAL seq with a fresh mtime — it is <= the
    // watermark, so observe_marker returns before dispose_report (marker.rs:102) and NOTHING
    // is bumped. This fails if the anti-replay guard is removed.
    report_progress(&fx, 8, 1);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        ledger(&fx).digest.disposed,
        2,
        "a stale/equal seq must not bump disposed"
    );
    // Exactly one raw.jsonl line per DISPOSED bump (2), not 3.
    let raw = std::fs::read_to_string(fx.sched.paths.raw_jsonl()).unwrap();
    assert_eq!(raw.lines().count(), 2, "one raw line per accepted bump: {raw:?}");
}

#[test]
fn auto_flow_consult_path_bumps_disposed_and_emits_a_raw_line() {
    // CRITICAL-1: the auto-flow+supervisor sub-path returns Ok(Some(tick)) before the
    // arm-local save; the real save is spawn_advice's `parked = next.clone()`. The seam
    // bumps `disposed`/`situation` and emits the raw line on `next` BEFORE the branch, so
    // the clone carries them. Pre-fix this path recorded nothing.
    let (mut fx, _sess) = marker_fx(Tier::Autopilot, |_| {});
    let _seq = start_consult(&mut fx); // spawns a real consult over AUTOFLOW_ASKS (seq 9)
    let l = ledger(&fx);
    assert_eq!(l.digest.disposed, 1, "the auto-flow accepted bump counts once");
    assert_eq!(l.digest.auto_flow, 1);
    let raw = std::fs::read_to_string(fx.sched.paths.raw_jsonl()).unwrap();
    assert_eq!(raw.lines().count(), 1);
    assert!(raw.contains("\"seq\":9"), "the raw line carries the disposed report: {raw}");
}

#[test]
fn blocked_no_stop_park_bumps_disposed_and_records_stalled() {
    // CRITICAL-2: a blocked report with no routable stop escalates via park_stuck, which
    // never reaches the four arms — but it DOES pass the top-of-dispose seam.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, r#"{"seq":12,"state":"blocked","stops":[]}"#);
    assert!(matches!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    let l = ledger(&fx);
    assert_eq!(l.digest.disposed, 1, "the park is an accepted bump");
    assert_eq!(l.digest.stalled, 1);
    assert!(
        l.decisions
            .iter()
            .any(|d| d.kind == job::DecisionKind::Stalled && d.seq == Some(12)),
        "a Stalled decision is recorded with the accepted seq: {:?}",
        l.decisions
    );
    let md = std::fs::read_to_string(fx.sched.paths.decisions()).unwrap();
    assert!(md.contains("pmd stalled:"), "the stall is source-attributed: {md}");
}

#[test]
fn situation_reflects_the_last_disposed_report_even_on_the_park_path() {
    // A working bump: situation mirrors the report's state/status/seq.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let sit = ledger(&fx).situation.expect("situation set on a working bump");
    assert_eq!(sit.state, job::WakeState::Working);
    assert_eq!(sit.seq, 7);
    assert_eq!(sit.status.as_deref(), Some("indexing"));
    // The blocked-NO-STOP park path (Missing-3): situation must NOT go stale even though
    // this path skips every arm and parks via park_stuck. Guaranteed by the seam.
    let (mut fx2, _s2) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fx2, r#"{"seq":12,"state":"blocked","stops":[]}"#);
    assert!(matches!(
        fx2.sched.tick(&fx2.driver, &fx2.clock).unwrap(),
        JobTick::Stuck(_)
    ));
    let sit2 = ledger(&fx2).situation.expect("situation set on the park path");
    assert_eq!(sit2.state, job::WakeState::Blocked);
    assert_eq!(sit2.seq, 12);
}

#[test]
fn decisions_md_is_sparse_and_source_attributed() {
    // Working → NOT notable: no decisions.md line (raw.jsonl only).
    let (mut fxw, _sw) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fxw, r#"{"seq":7,"state":"working","status":"indexing"}"#);
    fxw.sched.tick(&fxw.driver, &fxw.clock).unwrap();
    let mdw = std::fs::read_to_string(fxw.sched.paths.decisions()).unwrap_or_default();
    assert!(
        !mdw.contains("pmd "),
        "a working decision is raw-only, never on decisions.md: {mdw:?}"
    );
    // Blocked-escalating (hard publish) → notable: a `pmd escalated:` line appears.
    let (mut fxb, _sb) = marker_fx(Tier::Standard, |_| {});
    write_marker(&fxb, BLOCKED_HARD);
    assert!(matches!(
        fxb.sched.tick(&fxb.driver, &fxb.clock).unwrap(),
        JobTick::Escalated(_)
    ));
    let mdb = std::fs::read_to_string(fxb.sched.paths.decisions()).unwrap();
    assert!(mdb.contains("pmd escalated:"), "escalation is attributed: {mdb}");
    // AutoFlow (consultable) → ABSENT from decisions.md but PRESENT in raw.jsonl.
    let (mut fxa, _sa) = marker_fx(Tier::Autopilot, |_| {});
    let _seq = start_consult(&mut fxa);
    let mda = std::fs::read_to_string(fxa.sched.paths.decisions()).unwrap_or_default();
    assert!(!mda.contains("auto_flow"), "auto-flow is raw-only: {mda:?}");
    let rawa = std::fs::read_to_string(fxa.sched.paths.raw_jsonl()).unwrap();
    assert!(rawa.contains("\"seq\":9"), "auto-flow IS in raw.jsonl: {rawa}");
}

#[test]
fn side_file_error_never_fails_the_state_json_write() {
    // Make raw.jsonl a DIRECTORY so the append open() fails (EISDIR). The dispose must
    // still succeed: the side-file failure is logged and swallowed.
    let (mut fx, _sess) = marker_fx(Tier::Standard, |_| {});
    std::fs::create_dir_all(fx.sched.paths.raw_jsonl()).unwrap();
    write_marker(&fx, r#"{"seq":7,"state":"working","status":"x"}"#);
    // tick() must NOT error (unwrap proves it) despite the raw.jsonl append failing.
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let l = ledger(&fx);
    assert_eq!(l.last_marker_seq, 7, "the operational ledger still saved");
    assert_eq!(l.digest.disposed, 1, "dispose ran; only the side file failed");
}

#[test]
fn raw_jsonl_rotates_past_the_ceiling() {
    // A tiny injected ceiling forces a rotation between two short lines: the second append
    // sees the file at/over the ceiling, renames it to `.1`, then writes into a fresh file.
    let (fx, _sess) = marker_fx(Tier::Standard, |_| {});
    fx.sched.append_raw("line-1", 4).unwrap();
    fx.sched.append_raw("line-2", 4).unwrap();
    let raw = std::fs::read_to_string(fx.sched.paths.raw_jsonl()).unwrap();
    let rotated = std::fs::read_to_string(fx.sched.paths.raw_jsonl_rotated()).unwrap();
    assert!(raw.contains("line-2") && !raw.contains("line-1"), "raw holds only the newest");
    assert!(rotated.contains("line-1"), "the ceiling rotated the old generation to .1");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `ECC_GATEGUARD=off cargo test --lib job_engine::tests::marker`
Expected: FAIL — the crate does not compile (`append_raw`, `RAW_JSONL_MAX_BYTES`, `digest`/`situation` handling in `dispose_report` do not exist yet).

- [ ] **Step 3: Write the minimal implementation**

**(3a)** Add consts + helpers to `src/job_engine/mod.rs`. Add the consts near the other module consts (e.g. after the `pub use` block, ~`src/job_engine/mod.rs:79`):

```rust
/// `raw.jsonl` size ceiling (~2 MB) before it rotates to a single `.1` generation
/// (~4 MB/session total). Machine full-fidelity, so the ceiling is generous.
const RAW_JSONL_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// `decisions.md` size ceiling before it rotates to a single `.1` generation. The human
/// file is sparse (Escalated/Stalled only), so this is small — it exists so even the sparse
/// file cannot grow unbounded over a multi-day session.
const DECISIONS_MD_MAX_BYTES: u64 = 256 * 1024;
```

Add the two helpers to `impl JobScheduler` (after `persist_run`, `src/job_engine/mod.rs:542`):

```rust
    /// Append one full-fidelity line to `raw.jsonl`, rotating to a single `.1` generation
    /// FIRST if the file is at/over `max_bytes`. Size-check-then-rename-then-append (the
    /// `append_decision_note` style; there is no append-atomic primitive, so a crash
    /// mid-`writeln` can tear the last line — readers must parse leniently). Returns `Err`
    /// on any I/O failure; the caller error-ISOLATES it (a side-file failure must never fail
    /// the state.json write).
    fn append_raw(&self, line: &str, max_bytes: u64) -> Result<()> {
        use std::io::Write as _;
        let path = self.paths.raw_jsonl();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create_dir_all {}", dir.display()))?;
        }
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= max_bytes {
            // Best-effort rotation: on failure we simply keep appending to the current file.
            let _ = std::fs::rename(&path, self.paths.raw_jsonl_rotated());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        writeln!(f, "{line}").with_context(|| format!("append {}", path.display()))?;
        Ok(())
    }

    /// Append a source-attributed line to `decisions.md` for a NOTABLE decision only
    /// (`Escalated`/`Stalled`; everything else is `raw.jsonl`-only). Rotates to a single
    /// `.1` generation at `DECISIONS_MD_MAX_BYTES`. Line format: `- {epoch} pmd {kind}:
    /// {summary}`. Error-isolated by the caller.
    fn append_decision_md(
        &self,
        now: Epoch,
        kind: crate::job::DecisionKind,
        summary: Option<&str>,
    ) -> Result<()> {
        use crate::job::DecisionKind;
        use std::io::Write as _;
        if !matches!(kind, DecisionKind::Escalated | DecisionKind::Stalled) {
            return Ok(());
        }
        let path = self.paths.decisions();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create_dir_all {}", dir.display()))?;
        }
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= DECISIONS_MD_MAX_BYTES {
            let _ = std::fs::rename(&path, self.paths.decisions_rotated());
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        writeln!(f, "- {now} pmd {}: {}", kind.as_str(), summary.unwrap_or("").trim())
            .with_context(|| format!("append {}", path.display()))?;
        Ok(())
    }
```

**(3b)** Update `src/job_engine/marker.rs`. Extend the imports at `src/job_engine/marker.rs:15`:

```rust
use crate::job::{
    AgentLoopState, AutopilotEventKind, DecisionKind, DecisionRecord, JobRun, LedgerSituation,
    WakeReport, WakeState,
};
```

and add the const import to the `use super::…` group (near `src/job_engine/marker.rs:23`):

```rust
use super::{JobScheduler, JobTick, RAW_JSONL_MAX_BYTES};
```

Add a private `RawLine` serialize helper near the top of `src/job_engine/marker.rs` (e.g. after the imports, before `impl JobScheduler`):

```rust
/// One full-fidelity `raw.jsonl` line: the dispose metadata plus the whole [`WakeReport`]
/// (B-4: full-fidelity machine JSONL). Serialized compactly to a single line.
#[derive(serde::Serialize)]
struct RawLine<'a> {
    at: Epoch,
    disposed: u64,
    report: &'a WakeReport,
}
```

Insert the SEAM into `dispose_report`, immediately after the cadence-note block and BEFORE `match report.state {` (after `src/job_engine/marker.rs:170`):

```rust
        // === Milestone B: the decider-lane ledger — the single accepted-bump seam ===
        // Unconditional, ONCE per accepted marker bump, BEFORE any WakeState branch — so the
        // two paths that skip the four arms (auto-flow→spawn_advice's `parked = next.clone()`,
        // and blocked-no-stop→park_stuck's clone) still carry them. This structurally
        // guarantees `digest.disposed == number of accepted bumps` and keeps `situation` from
        // ever going stale on a park (Missing-3).
        next.digest.disposed = next.digest.disposed.saturating_add(1);
        let open_ids: Vec<String> = next.open_stops.iter().map(|s| s.id.clone()).collect();
        next.situation = Some(LedgerSituation::from_report(&report, &open_ids, now));
        // `raw.jsonl` is full-fidelity and a pure function of the report, so emit it here.
        // ERROR-ISOLATED: a side-file failure is logged and swallowed — it must never fail
        // the tick (raw.jsonl is machine-only, lenient-read, torn-tolerant).
        if let Ok(line) = serde_json::to_string(&RawLine {
            at: now,
            disposed: next.digest.disposed,
            report: &report,
        }) && let Err(e) = self.append_raw(&line, RAW_JSONL_MAX_BYTES)
        {
            eprintln!("pmd: {}: raw.jsonl append failed: {e}", self.project_id);
        }
```

In the **Working** arm (`src/job_engine/marker.rs:177-182`), add a `record_decision` call before `self.save_ledger(&mut next)?;`:

```rust
            WakeState::Working => {
                next.continuations = 0;
                next.record_event(now, AutopilotEventKind::Reported(report.status.clone()));
                next.record_decision(DecisionRecord::at(
                    now,
                    Some(report.seq),
                    DecisionKind::Working,
                    report.status.clone(),
                    Vec::new(),
                ));
                self.save_ledger(&mut next)?;
                Ok(None)
            }
```

In the **Monitoring** arm (`src/job_engine/marker.rs:185-205`), add before `self.save_ledger(&mut next)?;` (at `src/job_engine/marker.rs:202`):

```rust
                next.record_decision(DecisionRecord::at(
                    now,
                    Some(report.seq),
                    DecisionKind::Monitoring,
                    report.status.clone(),
                    Vec::new(),
                ));
```

In the **Blocked-no-stop** sub-path (`src/job_engine/marker.rs:209-215`), record a `Stalled` decision on `next` (so `park_stuck`'s clone carries it) and write the notable `decisions.md` line after the park:

```rust
                if report.stops.is_empty() {
                    // No routable stop → treat as a stall escalation.
                    let reason = "agent reported blocked without a stop".to_string();
                    next.record_decision(DecisionRecord::at(
                        now,
                        Some(report.seq),
                        DecisionKind::Stalled,
                        Some(reason.clone()),
                        Vec::new(),
                    ));
                    let tick = self.park_stuck(now, &next, reason.clone())?;
                    if let Err(e) = self.append_decision_md(now, DecisionKind::Stalled, Some(&reason))
                    {
                        eprintln!("pmd: {}: decisions.md append failed: {e}", self.project_id);
                    }
                    return Ok(Some(tick));
                }
```

In the **Blocked-escalating** arm (`src/job_engine/marker.rs:264-303`), add the `record_decision` + a situation refresh (the escalate arm is the only one that grows `open_stops`) before `self.save_ledger(&mut next)?;` (at `src/job_engine/marker.rs:298`), and the notable `decisions.md` write after it:

```rust
                    next.record_event(now, AutopilotEventKind::Escalated(asked.clone()));
                    next.record_decision(DecisionRecord::at(
                        now,
                        Some(report.seq),
                        DecisionKind::Escalated,
                        asked.clone(),
                        ids.clone(),
                    ));
                    // The seam captured the PRIOR open stops; refresh the snapshot with the
                    // stops the session now blocks on.
                    if let Some(sit) = next.situation.as_mut() {
                        sit.open_stops = ids.clone();
                    }
                    self.save_ledger(&mut next)?;
                    if let Err(e) =
                        self.append_decision_md(now, DecisionKind::Escalated, asked.as_deref())
                    {
                        eprintln!("pmd: {}: decisions.md append failed: {e}", self.project_id);
                    }
                    self.run = JobRun::Blocked {
                        stop_ids: ids.clone(),
                        since: now,
                    };
                    Ok(Some(JobTick::Escalated(ids)))
```

(Note: `asked` is already `Option<String>` at `src/job_engine/marker.rs:290`; `record_event` at `src/job_engine/marker.rs:297` becomes `asked.clone()` because `asked` is now reused below.)

In the **Blocked-auto-flow** arm (`src/job_engine/marker.rs:304-337`), record the `AutoFlow` decision on `next` BEFORE `spawn_advice`, so the consult's `parked = next.clone()` carries it (and the fallback save carries it too when the consult is not spawned):

```rust
                    next.continuations = 0;
                    let auto_summary = auto
                        .first()
                        .map(|(_, d)| d.question.clone())
                        .filter(|q| !q.is_empty());
                    next.record_decision(DecisionRecord::at(
                        now,
                        Some(report.seq),
                        DecisionKind::AutoFlow,
                        auto_summary,
                        auto_ids.clone(),
                    ));
                    if let Some(tick) = self.spawn_advice(driver, now, &next, &auto)? {
                        return Ok(Some(tick));
                    }
```

(AutoFlow is not notable, so no `decisions.md` write; the seam already emitted its `raw.jsonl` line.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `ECC_GATEGUARD=off cargo test --lib job_engine::tests::marker`
Expected: PASS (all seven new tests plus the pre-existing marker tests). Non-vacuity per test:
- `digest_counters_are_monotonic…`: the stale-seq re-observe fails the `disposed == 2` assertion if the anti-replay guard (`marker.rs:102`) is removed; the raw line count fails if the seam double-emits or skips.
- `auto_flow_consult_path…`: `disposed == 1` fails if the seam were placed inside the arms (the consult path returns before the arm save), pinning CRITICAL-1.
- `blocked_no_stop_park…`: `disposed == 1` fails if the seam were inside the arms (park_stuck bypasses them), pinning CRITICAL-2.
- `situation_reflects…`: `.expect()` panics if `situation` is unset on the park path, pinning Missing-3.
- `decisions_md_is_sparse…`: fails if the notable set is wrong (Working leaking in, or AutoFlow being written to the human file).
- `side_file_error_never_fails…`: `tick().unwrap()` panics if the append error is not swallowed.
- `raw_jsonl_rotates…`: `!raw.contains("line-1")` fails if rotation is a no-op (both lines would be in one file).

- [ ] **Step 5: Commit**

```bash
git add src/job_engine/mod.rs src/job_engine/marker.rs src/job_engine/tests/marker.rs
git commit -m "feat(ledger-b): single dispose seam (disposed/situation/raw.jsonl) + per-arm decisions + rotation"
```

---

## Task 5: The nudge firewall stays green under the decider ledger

**Files:**
- Modify: `src/job_engine/tests/nudge.rs:648` (extend `the_nudge_is_a_pure_function_of_agent_authored_inputs`'s noisy edit closure).

**Interfaces:**
- Consumes: `AgentLoopState::record_decision` (Task 2), `DecisionCounters`/`LedgerSituation` (Task 1), the existing `loop_nudge_prompt` (unchanged) and `Fx` fixtures.

- [ ] **Step 1: Extend the failing test**

In `the_nudge_is_a_pure_function_of_agent_authored_inputs` (`src/job_engine/tests/nudge.rs:648`), add the three B fields to the NON-agent-authored `noisy` edit closure (inside `drive_nudge(&|s| { … })`, alongside `s.continuations = 7;` etc.):

```rust
        // Decider-lane ledger fields are NON-agent-authored bookkeeping — they must not leak
        // into the worker nudge any more than the wake counters or the events feed do.
        s.digest.disposed = 123;
        s.digest.escalated = 45;
        s.situation = Some(job::LedgerSituation {
            state: job::WakeState::Blocked,
            status: Some("LEAK-SENTINEL-situation".into()),
            open_stops: vec!["stop-leak".into()],
            seq: 999,
            at: 1,
        });
        s.record_decision(job::DecisionRecord::at(
            1,
            Some(999),
            job::DecisionKind::Escalated,
            Some("LEAK-SENTINEL-decision".into()),
            vec!["stop-leak".into()],
        ));
```

- [ ] **Step 2: Run test to verify it still holds (and fails loudly on a leak)**

Run: `ECC_GATEGUARD=off cargo test --lib job_engine::tests::nudge::the_nudge_is_a_pure_function_of_agent_authored_inputs`
Expected: PASS. The existing `assert_eq!(noisy, expect(&fx_noisy))` already computes the pure `loop_nudge_prompt(agent-authored inputs, marker)` with no ledger in sight, so if any B field reached the delivered nudge the delivered text would exceed the pure output and the assertion would fail. Non-vacuity: the `LEAK-SENTINEL-*` strings are distinctive — were they to appear in the delivered nudge, the equality fails.

- [ ] **Step 3: (no implementation needed — `loop_nudge_prompt` is untouched by B)**

If Step 2 fails, a B field leaked into the nudge path — that is a bug to fix in the leaking code, not the test.

- [ ] **Step 4: Commit**

```bash
git add src/job_engine/tests/nudge.rs
git commit -m "test(ledger-b): firewall proves decider fields never reach the worker nudge"
```

---

## Task 6: Full green — test, clippy, fmt

**Files:** none (verification only).

- [ ] **Step 1: Run the whole suite**

Run: `ECC_GATEGUARD=off cargo test`
Expected: PASS (all crate tests, incl. `job::tests`, `job_engine::tests::marker`, `job_engine::tests::nudge`, `state::tests`).

- [ ] **Step 2: Clippy, all targets**

Run: `ECC_GATEGUARD=off cargo clippy --all-targets`
Expected: no new warnings. If clippy flags the let-chain in the seam or a `needless_borrow` on `&report`, address minimally without changing behavior.

- [ ] **Step 3: Formatting**

Run: `ECC_GATEGUARD=off cargo fmt --all -- --check`
Expected: no diff. If it reports formatting, run `cargo fmt --all` and re-run the check.

- [ ] **Step 4: Commit any fmt/clippy fixups**

```bash
git add -A
git commit -m "chore(ledger-b): rustfmt + clippy clean"
```

---

## Self-Review

**1. Spec coverage** (Milestone B section + LOCKED B-1..B-4):
- Data types `DecisionCounters`/`DecisionKind`/`DecisionRecord(seq: Option<u64>)`/`LedgerSituation` + serde + back-compat + round-trip → Task 1. ✅
- `record_decision` coalesce on `(kind,summary,stop_ids)`, bound to `DECISIONS_SLICE_MAX`, `de_decisions_lenient` → Tasks 1-2. ✅
- Single top-of-`dispose_report` seam bumping `disposed`/`situation`/raw line before any branch; auto-flow+supervisor path (CRITICAL-1); blocked-no-stop→park_stuck (CRITICAL-2); stale-seq sabotage → Task 4. ✅
- Per-arm kind-specific fields (per-kind counter, summary, stop_ids) → Task 4. ✅
- `raw.jsonl` append + rotation (tiny injected ceiling) + error-isolation (path→directory) → Tasks 3-4. ✅
- `decisions.md` sparse+attributed, notable = Escalated+Stalled (AutoFlow ABSENT/PRESENT-in-raw) + rotation → Tasks 3-4. ✅
- Nudge firewall unaffected → Task 5. ✅
- Lifetime-monotonic (saturating, never reset), non-dispose park paths don't bump `disposed`, no `digest.json`, `decide_kind` untouched → Global Constraints, enforced by the seam design. ✅
- `situation_reflects_last_disposed_report` incl. park path (Missing-3) → Task 4. ✅
- Final green (`cargo test`/clippy/fmt) → Task 6. ✅

**2. Placeholder scan:** No "TBD"/"similar to above"/"add appropriate X" — every code and test step carries real code. ✅

**3. Type consistency:** `DecisionCounters`, `DecisionKind`, `DecisionRecord` (constructor `DecisionRecord::at`), `LedgerSituation` (constructor `LedgerSituation::from_report`), `DECISIONS_SLICE_MAX`, `record_decision`, `append_raw`, `append_decision_md`, `raw_jsonl`/`raw_jsonl_rotated`/`decisions_rotated`, `RAW_JSONL_MAX_BYTES`/`DECISIONS_MD_MAX_BYTES` — names are used identically across Tasks 1-6. `DecisionKind::as_str()` returns snake-case tokens matching `#[serde(rename_all="snake_case")]`. ✅

**Two design deviations from the literal spec text, both flagged for the controller:**
1. **`raw.jsonl` is appended AT the seam (before the arms' `save_ledger`), not "after save_ledger".** The corrected single-seam design says to append the raw line "at the top of `dispose_report`"; the older "Side files" paragraph said side-file writes run after `save_ledger`. Only the human file (`decisions.md`) can honor "after save" (it is per-arm, kind-dependent). The raw line is a pure function of the report and must be emitted on the two arm-skipping paths, so it lives at the seam. It stays error-isolated; `raw.jsonl` is documented machine-only/torn-tolerant.
2. **`situation` is set by the seam (not by `record_decision`).** The suggested task spine had `record_decision` set `situation`; putting it at the seam is what structurally guarantees it never goes stale on the park path (Missing-3). `record_decision` therefore does NOT touch `situation` (avoids two writers).
