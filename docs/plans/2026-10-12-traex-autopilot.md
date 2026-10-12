# TraeX Autopilot Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable TraeX as an Autopilot worker while preserving pmtui's one-terminal session model.

**Architecture:** Reuse the existing turn-complete signal file. TraeX gets a Stop hook injected with a stable environment-based command, so hook trust is not tied to a session path. Autopilot launches use `--permission-mode auto` without `--sandbox`, because the current TraeX CLI rejects both overrides together.

**Tech Stack:** Rust 2024, tmux, ratatui, TraeX CLI `traex`.

---

### Task 1: Pin The TraeX Launch Contract

**Files:**
- Modify: `src/worker/tests.rs`
- Modify: `src/worker/launch.rs`

**Steps:**
1. Add failing tests for TraeX loop argv.
2. Verify the tests fail on the current panic.
3. Implement the TraeX arm in `build_loop_command`.
4. Run `cargo test worker::tests::loop_command_trae`.

### Task 2: Remove Autopilot UI Gates

**Files:**
- Modify: `src/bin/pmtui/tests/create.rs`
- Modify: `src/bin/pmtui/tests/autopilot.rs`
- Modify: `src/bin/pmtui/app/create.rs`
- Modify: `src/bin/pmtui/app/autopilot.rs`

**Steps:**
1. Replace tests that expect TraeX refusal with tests that expect normal Autopilot flow.
2. Remove the create and mode-toggle refusal branches.
3. Run focused pmtui tests for create and autopilot.

### Task 3: Let pmd Launch TraeX Autopilot

**Files:**
- Modify: `src/job_engine/tests/session.rs`
- Modify: `src/job_engine/session.rs`

**Steps:**
1. Replace the daemon refusal test with a launch test.
2. Remove the daemon-side TraeX bail.
3. Run the focused job-engine session test.

### Task 4: Update Docs And Verify

**Files:**
- Modify: `docs/SPEC.md`
- Modify: `docs/GUIDE.md`

**Steps:**
1. Document TraeX Autopilot as supported with one-time Stop hook trust.
2. Run focused tests, `cargo fmt --all -- --check`, and as much of the required verification list as the environment allows.
