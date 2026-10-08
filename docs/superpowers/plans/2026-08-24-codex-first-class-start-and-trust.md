# Codex: hand-start on Enter + skip the trust-dialog wedge — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** implemented @ feat/codex-first-class-start-and-trust

**Goal:** (Fix 2) Stop unattended codex launches wedging on the "Do you trust this directory?" dialog by trusting the work_dir via an agent-manager-owned codex **profile** (`-p`), never the user's base config. (Fix 1) Let a fresh **standard** codex session open a live REPL on Enter (like claude), instead of the "start pmd for the first codex wake" dead-end.

**Architecture:** A new `worker::ensure_codex_trust(work_dir)` writes `$CODEX_HOME/pmd-trust-<hash>.config.toml` (`[projects."<dir>"] trust_level="trusted"`) and returns the profile name; `worker::build_loop_command` gains an optional `codex_trust_profile` that inserts `-p <name>` for codex. pmtui's fresh-codex Enter path (`decide.rs` new `CreateChatNoSeed` + `enter.rs`) launches `codex [-p <name>] [-m <model>]` via the existing `pending_chat` path. Verified live: a profile-supplied `[projects…] trust_level="trusted"` skips the dialog (`codex 0.146.1.378`).

**Tech Stack:** Rust (std only — no new deps; `std::fs`, `DefaultHasher`), the tmux `Driver` seam, real-tmux acceptance + `pmtui-ui-testing`.

## Global Constraints

- **No new crates.** Write the 2-line profile TOML by hand (`std::fs::write`); a quoted TOML key handles dotted paths — do NOT use codex's `-c` (its CLI splits dotted keys). Never read/modify the user's base `~/.codex/config.toml`; only write `$CODEX_HOME/pmd-trust-*.config.toml`.
- **Fail-open.** A trust-profile write error must NOT block the launch — launch codex without `-p` (human sees the dialog, as today) and log once.
- **claude is byte-unchanged.** Every claude argv (`build_loop_command`, `build_chat`, `build_chat_create`) stays identical; new params default to no-op for claude.
- **Single-writer / invariants untouched:** no `state.json` writes added; `pmd_drives_row`, tiers, the decider, and the create-form UI are untouched.
- **Full suite after every change:** `cargo test`, `cargo clippy --all-targets` (clean), `cargo fmt --all -- --check`.
- **Real-tmux acceptance MANDATORY before merge** (launch/enter change): `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1`.

## Current state (verified anchors)

- `worker/launch.rs::build_loop_command(engine, resume: &Resume, add_dirs, turn_signal, model)` — codex arm: `["env","-u","CLAUDECODE","ECC_GATEGUARD=off","codex", (-m model)?, (-c notify)?, ("resume" id)?, "--ask-for-approval","never","--sandbox","workspace-write"]`. `-p` is a codex GLOBAL option (before the `resume` subcommand), confirmed in `codex --help`.
- Only caller of `build_loop_command`: `src/job_engine/session.rs:92` (`worker::build_loop_command(...)` then `.launch_interactive(&session, &self.work_dir, &argv)` at :100). The work_dir is `self.work_dir`.
- `src/bin/pmtui/session.rs::build_chat(engine, cid, model)` — codex arm `["codex", (-m)?, "resume", id]`. `build_chat_create(engine, cid, model)` — codex arm `unreachable!()` (stays; the ghost path never routes codex there — `claude_conversation_exists` returns `None` for codex).
- `src/bin/pmtui/decide.rs::first_wake_action(engine, lease_free, id) -> FirstWakeAction` — `(Codex,true) → Status("{id}: no daemon is driving this session — start pmd for the first codex wake")`; `(Codex,false) → Arm(...)`; `(Claude,true) → CreateAndChat`; `(Claude,false) → Arm(...)`. Variants: `CreateAndChat`, `Arm(String)`, `Status(String)`. Tests in decide.rs pin these.
- `src/bin/pmtui/app/enter.rs` `WaitingFirstWake` → `LeaseKeyed` → `try_acquire(&lock)` → `first_wake_action(...)`: `CreateAndChat` mints+seeds+`pending_create_chat`(holds lease); `Arm(status)` sets `pending_first_chat`; `Status(status)` `drop(lease_opt)` + sets status. `ChatReq { argv, session_paths, root, label, socket, session, reattach }` → `self.pending_chat`.
- `worker` is a directory module (`src/worker/{launch.rs,tests.rs,mod.rs}`); `Engine` from `crate::registry`.

---

### Task 1: Trust profile + unattended (pmd loop) codex launch

**Files:** Create `src/worker/trust.rs`; Modify `src/worker/mod.rs`, `src/worker/launch.rs`, `src/job_engine/session.rs`; Test `src/worker/tests.rs`.

- [ ] **Step 1: `ensure_codex_trust`** in a new `src/worker/trust.rs`, re-exported from `worker/mod.rs`:

```rust
use std::io;
use std::path::{Path, PathBuf};

/// Ensure codex TRUSTS `work_dir`, so an unattended (pmd-driven) codex launch never blocks on the
/// interactive "Do you trust the contents of this directory?" dialog (which no `--ask-for-approval`/
/// `--sandbox` flag skips, and which wedges a headless pane). Writes an agent-manager-OWNED codex
/// PROFILE file — never the user's base `config.toml` — and returns the profile name to pass as
/// `codex -p <name>`. A profile layers `[projects."<dir>"] trust_level="trusted"` on top of the
/// user's real config/auth (verified live against codex 0.146.1.378). Rewritten deterministically
/// each call (idempotent); one file per work_dir (hash-named), so distinct dirs never clobber.
pub fn ensure_codex_trust(work_dir: &Path) -> io::Result<String> {
    let home = codex_home();
    std::fs::create_dir_all(&home)?;
    let abs = work_dir.to_string_lossy();
    let name = format!("pmd-trust-{}", short_hash(&abs));
    // Quote the path as a TOML basic-string key: escape backslash first, then quote. Dotted paths
    // are fine in a FILE key (the `-c` CLI splitter bug does not apply here).
    let key = abs.replace('\\', "\\\\").replace('"', "\\\"");
    let body = format!("[projects.\"{key}\"]\ntrust_level = \"trusted\"\n");
    std::fs::write(home.join(format!("{name}.config.toml")), body)?;
    Ok(name)
}

/// `$CODEX_HOME` if set and non-empty, else `$HOME/.codex`.
fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex"),
    }
}

/// Stable, filename-safe 16-hex digest of a string (per-dir uniqueness within the codex home).
fn short_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}
```

- [ ] **Step 2: `build_loop_command` gains `codex_trust_profile: Option<&str>`** (new LAST param). In the `Engine::Codex` arm, immediately after `argv.push("codex".into());` and BEFORE the `-m`/`-c`/`resume` pushes:

```rust
            if let Some(name) = codex_trust_profile {
                argv.push("-p".into());
                argv.push(name.to_string());
            }
```

  The `Engine::Claude` arm ignores it (no change to claude argv). Update the doc comment (the codex arm now takes an optional trust profile via `-p`, a global option before the subcommand).

- [ ] **Step 3: Wire the pmd loop launch** in `src/job_engine/session.rs` (around the `build_loop_command` call at :92). For codex, compute the trust profile before building; claude passes `None`; fail-open:

```rust
        let codex_trust = match engine {
            Engine::Codex => match worker::ensure_codex_trust(&self.work_dir) {
                Ok(name) => Some(name),
                Err(e) => {
                    eprintln!("pmd: could not write codex trust profile for {}: {e} — launching without it", self.id);
                    None
                }
            },
            Engine::Claude => None,
        };
        let argv = worker::build_loop_command(engine, &resume, &add_dirs, turn_signal, model, codex_trust.as_deref());
```

  (Match the real local names for `engine`/`self.work_dir`/`resume`/`add_dirs`/`turn_signal`/`model` in that function — read the file; only ADD the trust arg.)

- [ ] **Step 4: Unit tests** (`src/worker/tests.rs`):
  - `ensure_codex_trust` with `CODEX_HOME` set to a `tempfile::tempdir()`: returns a `pmd-trust-<16hex>` name; the file `<home>/<name>.config.toml` exists and equals `[projects."<dir>"]\ntrust_level = "trusted"\n` for the given dir; a dir path containing `.` and a space is quoted verbatim in the key; a second call writes byte-identical content (idempotent). (Set/restore `CODEX_HOME` within the test; keep it serial-safe — read the env once.)
  - `build_loop_command(Engine::Codex, &Resume::Continue("cid".into()), &[], None, None, Some("pmd-trust-x"))` contains `["codex","-p","pmd-trust-x", …,"resume","cid",…]` with `-p pmd-trust-x` immediately after `codex` and before `resume`; the SAME call with `None` is byte-identical to the pre-change codex argv (pin it); `Engine::Claude` with `Some(...)` is byte-identical to claude-without-it (the profile is ignored for claude).

- [ ] **Step 5: Green + hygiene + commit.** `cargo test`, `cargo clippy --all-targets`, `cargo fmt --all -- --check`.
```bash
git commit -am "feat(codex): trust the work_dir via an owned profile so pmd's codex launch skips the trust dialog"
```

---

### Task 2: Hand-start a fresh standard codex session on Enter

**Files:** Modify `src/bin/pmtui/session.rs`, `src/bin/pmtui/decide.rs`, `src/bin/pmtui/app/enter.rs`; Test `src/bin/pmtui/decide.rs` (+ an enter/session test).

- [ ] **Step 1: Fresh-codex chat argv** in `src/bin/pmtui/session.rs` — a NEW function (leave `build_chat_create`'s codex `unreachable!` intact; it is never reached for codex):

```rust
/// Build the argv for a FRESH interactive codex chat — the hand-started (Enter) create for codex,
/// which has no caller-chosen id (so no `--session-id`/`resume`). Interactive, human-answered
/// approvals (NO unattended `--ask-for-approval never`/`--sandbox` — a human is present, mirroring
/// `build_chat`'s codex-resume posture). `trust_profile` (from `worker::ensure_codex_trust`) is
/// added as `codex -p <name>` so the first launch in an unseen dir skips the trust dialog.
pub(crate) fn build_codex_fresh_chat(model: Option<&str>, trust_profile: Option<&str>) -> Vec<String> {
    let mut argv = vec!["codex".to_string()];
    if let Some(name) = trust_profile {
        argv.push("-p".to_string());
        argv.push(name.to_string());
    }
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        argv.push("-m".to_string());
        argv.push(m.to_string());
    }
    argv
}
```

- [ ] **Step 2: `decide.rs` — replace the codex-free dead-end.** Add variant `CreateChatNoSeed(String)` to `FirstWakeAction` (doc: codex + free lock ⇒ open a FRESH codex chat now — no id to mint/seed). Change the `(Engine::Codex, true)` arm of `first_wake_action` from `Status(...)` to:

```rust
        (Engine::Codex, true) => FirstWakeAction::CreateChatNoSeed(format!(
            "creating a codex chat for {id} — a live REPL opens now (Ctrl+q returns; codex creates the conversation)"
        )),
```

  Update the module/enum doc and the router doc bullets. Update/replace the decide.rs tests that pinned the old `(Codex,true) → Status("…start pmd…")` to expect `CreateChatNoSeed`; keep `(Codex,false) → Arm`, both claude arms, byte-identical.

- [ ] **Step 3: `enter.rs` — handle `CreateChatNoSeed`.** In the `LeaseKeyed` `match first_wake_action(...)`, replace the `FirstWakeAction::Status(status)` arm with:

```rust
                                FirstWakeAction::CreateChatNoSeed(status) => {
                                    // codex + free lease: pmtui opens a FRESH codex REPL now (codex
                                    // has no caller-chosen id → nothing to mint/seed). Standard ⇒ pmd
                                    // won't drive this row, so drop the lease; nothing races.
                                    drop(lease_opt);
                                    let trust = worker::ensure_codex_trust(&root).ok();
                                    let chat_session = chat_session_name(&id, &root);
                                    self.pending_chat = Some(ChatReq {
                                        argv: build_codex_fresh_chat(worker_model.as_deref(), trust.as_deref()),
                                        session_paths,
                                        root,
                                        label: id.clone(),
                                        socket: self.socket.clone(),
                                        session: chat_session,
                                        reattach: false,
                                    });
                                    self.status = status;
                                }
```

  (`worker::ensure_codex_trust` is the Task 1 helper; confirm `worker` is importable in the pmtui bin — it is a lib module, used elsewhere in pmtui. Match the exact `ChatReq` field names/order from the file.)

- [ ] **Step 4: Tests.**
  - `build_codex_fresh_chat(None, None) == ["codex"]`; `(Some("gpt-x"), None)` inserts `-m gpt-x`; `(None, Some("pmd-trust-x"))` inserts `-p pmd-trust-x` right after `codex`; `(Some("m"), Some("p"))` = `["codex","-p","p","-m","m"]`.
  - decide.rs `first_wake_action(Engine::Codex, true, "s")` matches `CreateChatNoSeed(_)`; message contains "codex chat"; `(Engine::Codex, false, "s")` still `Arm`.
  - If there is a pmtui test that a fresh codex Enter sets a "start pmd" status / no `pending_chat`, update it: a fresh **standard** codex Enter now sets `pending_chat` (a codex fresh argv) and clears the lease. (Keep any autopilot-codex arm test unchanged — that still arms.)

- [ ] **Step 5: Green + hygiene + commit.** Full suite + clippy + fmt.
```bash
git commit -am "feat(pmtui): hand-start a fresh standard codex session on Enter (live REPL, like claude)"
```

---

### Task 3: Live tmux + mandatory acceptance + docs

**Files:** `README.md`, both design-doc/plan statuses; live tmux, `--ignored` acceptance.

- [ ] **Step 1: Build.** `cargo build` (updates `target/debug/{pmd,pmtui}`).
- [ ] **Step 2: Live tmux** (pmtui-ui-testing skill — scratch sockets ONLY; NEVER `--socket pmd`/the real registry; **also set a scratch `$CODEX_HOME`** so the real `~/.codex/config.toml` is untouched). Seed a scratch registry with an `agent_loop`+`codex` row (no config.json ⇒ standard). Launch pmtui with `ECC_GATEGUARD=off CODEX_HOME=$SCRATCH/codex`. Press Enter on the codex row → VERIFY + paste: a live codex REPL opens (its `>_ OpenAI Codex` box), with **no** "trust this directory?" dialog and **no** "start pmd for the first codex wake" status. Confirm `$SCRATCH/codex/pmd-trust-*.config.toml` was written with the row's root. Clean up BOTH scratch tmux servers, `$SCRATCH` (incl. the scratch codex home); confirm no stray `codex` REPLs. (A scratch `$CODEX_HOME` lacks the user's auth, so codex may show a login screen instead of the prompt — that is FINE: the test is that the trust dialog and the "start pmd" status are gone, not that codex is logged in.)
- [ ] **Step 3: Real-tmux acceptance (MANDATORY).** `ECC_GATEGUARD=off cargo test --test integration -- --ignored --test-threads=1 2>&1 | tail -40`. Must pass. Fix any test asserting the old codex `Status`/`unreachable` behavior (test-only). Report counts verbatim.
- [ ] **Step 4: Docs.** README: in the codex/engine notes, say a fresh **standard** codex session now opens on Enter like claude, and that pmd-driven codex trusts the work_dir automatically (via an owned profile, not your base config) so it doesn't stall on the trust dialog. Flip the design-doc + this plan Status to `implemented`.
- [ ] **Step 5: Commit.** `git commit -am "docs(codex): hand-start on Enter + auto-trust the work_dir — README + spec status"`

---

## Self-Review

**1. Spec coverage:** trust profile + `-p` on the pmd loop launch → T1 (helper, `build_loop_command` param, `session.rs` wiring, tests); hand-start codex on Enter → T2 (`build_codex_fresh_chat`, `CreateChatNoSeed`, `enter.rs` handler, tests); trust applied to the hand-started chat too → T2 Step 3 (`ensure_codex_trust` in the handler); live + mandatory acceptance + docs → T3. Fail-open (T1 Step 3, T2 Step 3 via `.ok()`), claude byte-unchanged, no base-config write, no new deps (Global Constraints).

**2. Placeholder scan:** no TBD; `ensure_codex_trust`, the argv insertions, the decide variant, and the enter handler are exact code; tests are concrete; the codex `unreachable!` is intentionally LEFT (never reached), with a NEW builder added instead.

**3. Type consistency:** `ensure_codex_trust(&Path) -> io::Result<String>`; `build_loop_command(…, codex_trust_profile: Option<&str>)` (new last param, both call sites updated); `build_codex_fresh_chat(Option<&str>, Option<&str>) -> Vec<String>`; `FirstWakeAction::CreateChatNoSeed(String)` handled in `enter.rs`; `ChatReq { argv, session_paths, root, label, socket, session, reattach }`; `worker::ensure_codex_trust` reachable from both `job_engine` and the pmtui bin. `Engine::{Codex,Claude}`, `Resume::{Continue,Fresh}` match the tree.
