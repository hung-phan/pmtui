# Make codex sessions start like claude: hand-start on Enter + skip the first-launch trust dialog

**Date:** 2026-08-24
**Status:** implemented @ feat/codex-first-class-start-and-trust (user: "check why claude works well but not codex" → chose **Both fixes**)

## Problem (diagnosed, code-grounded)

Two codex-specific gaps make codex "not work" where claude does:

1. **A fresh codex session can't be hand-started.** claude gets a caller-chosen id
   (`claude --session-id <uuid>`), so Enter on a fresh session opens a live REPL instantly
   (`first_wake_action(Claude, free) → CreateAndChat`). codex has **no caller-chosen id**, so
   `build_chat_create` is `unreachable!()` for codex and `first_wake_action(Codex, free)` returns a
   `Status` that just says *"no daemon is driving this session — start pmd for the first codex wake."*
   So a fresh **standard** codex session (e.g. `agent-manager-2`, which has no tier config) can be
   started by neither pmtui (can't create codex) nor pmd (not autopilot) — it's stuck. This is what
   the user hit.

2. **codex's first launch in an unseen directory wedges on a trust dialog.** Interactive `codex`
   opens *"Do you trust the contents of this directory? 1. Yes 2. No"* before any prompt. Neither
   `--ask-for-approval never` nor `--sandbox workspace-write` skips it (measured). Unattended (pmd)
   there's no human to answer, so `tmux::classify_pane` reads the pane as Busy → the session re-parks
   → a bogus "wedged" Stuck. Documented in `worker/launch.rs` as measured-and-unfixed. claude has no
   such dialog (`--permission-mode auto`).

## Verified facts (this is the "verified escaping rule" the code asked for)

Measured live against `codex 0.146.1.378`:
- With no trust, a fresh dir shows the blocking trust dialog.
- Writing `[projects."<abs-dir>"]\ntrust_level = "trusted"` into a **profile file**
  `$CODEX_HOME/<name>.config.toml` and launching `codex -p <name> …` goes **straight to the prompt,
  no dialog**, using the user's real auth/model (profiles LAYER on the base config). The user's base
  `~/.codex/config.toml` is never touched.
- The dotted-path bug the old comment feared is a `-c` **CLI-arg** parser problem; a quoted TOML key
  in a **file** (`[projects."/a/.b/c"]`) is standard TOML and parses correctly. So the file/profile
  approach is robust for dotted paths (worktrees under `.claude/…`) where `-c` was not.

## Decisions (with rationale)

### Fix 1 — a fresh STANDARD codex session opens a live REPL on Enter (like claude)

- **`session.rs`: add a fresh-interactive codex chat argv.** As built: a NEW
  `build_codex_fresh_chat(model, trust_profile) -> Vec<String>` = `["codex", ("-p", name)?, ("-m", model)?]`
  — interactive (no `resume`, no id, and **no** unattended `--ask-for-approval never`/`--sandbox`: a
  human is present and answers approvals, matching `build_chat`'s codex-resume posture). *Why:* codex
  can't be given a caller id, but a human doesn't need one — they just want a live REPL. NOTE:
  `build_chat_create`'s codex `unreachable!()` is deliberately **left intact** — the ghost path never
  routes codex there (`claude_conversation_exists` returns `None` for codex), so a separate builder is
  added rather than making the create arm reachable.
- **`decide.rs`: `first_wake_action(Codex, free)` → create, not nudge.** Replace the
  `Status("start pmd…")` arm with a new `FirstWakeAction::CreateChatNoSeed(status)` (no mint, no
  registry seed — codex has no id to seed). Keep `(Codex, held)` → `Arm` unchanged (pmd is driving;
  the auto-open still applies). `(Claude, *)` unchanged.
- **`enter.rs`: handle `CreateChatNoSeed`** by queuing `pending_chat` (the existing `ChatReq` path,
  `reattach = false`) with the fresh-codex argv, **dropping the lease** (standard ⇒ pmd won't drive,
  so nothing races). Status: *"creating a codex chat for {id} — a live REPL opens now (Ctrl+q
  returns; codex creates the conversation)."*
- **Limitation (documented, accepted):** a human-created codex conversation has no id pmtui/pmd can
  capture, so after the REPL EXITS there's nothing to `--resume` — a later Enter opens a *fresh*
  codex (while the `pmchat-` session stays alive, Enter re-attaches it, unchanged). This is strictly
  better than today (stuck), and pmd-adoption of a human codex conversation stays out of scope.

### Fix 2 — trust the work_dir so codex never wedges on the dialog

- **New `worker::ensure_codex_trust(work_dir) -> io::Result<String>`.** Writes
  `$CODEX_HOME/<name>.config.toml` (`$CODEX_HOME` or `~/.codex`) containing exactly
  `[projects."<abs work_dir>"]\ntrust_level = "trusted"\n`, and returns the profile `<name>`. Name is
  `pmd-trust-<hash>` where `<hash>` is a short stable hex of the absolute work_dir (filename-safe, no
  cross-dir clobber). The path in the TOML key is escaped (`\` → `\\`, `"` → `\"`). Best-effort:
  creates `$CODEX_HOME` if missing; the file is agent-manager-owned and rewritten deterministically
  each call (idempotent, no merge with user content). *Why a profile, not the base config:* it never
  touches the user's `~/.codex/config.toml`; a profile layers cleanly (verified).
- **`worker/launch.rs::build_loop_command`: add `codex_trust_profile: Option<&str>`.** For codex,
  when `Some(name)`, push `-p <name>` right after `codex` (global option, before `-m`/`-c`/`resume`).
  claude ignores it. Pure argv builder stays pure; the file-write is the caller's job.
- **Wire it at the codex launch sites (the I/O):** `job_engine/session.rs` (the pmd loop launch — the
  wedge) calls `ensure_codex_trust(work_dir)` for codex before `launch_interactive` and threads the
  profile name into `build_loop_command`. Apply the SAME `-p <name>` to the human codex chat launches
  (`build_chat` resume + the fresh-chat from Fix 1) so the first hand-started codex is seamless too —
  one shared helper, one behavior. claude launches are untouched.
- **Failure = fail-open:** if the profile write fails, launch codex without `-p` (the human sees the
  dialog, as today) rather than refusing to launch. Log once.

## Testing

- **Unit (pure):**
  - `build_codex_fresh_chat(model, trust_profile)` returns `["codex"]` (+ `-p <name>` / `-m <model>`
    when set, `-p` before `-m`); no `resume`, no id, no unattended flags. `build_chat`/`build_chat_create`
    codex arms unchanged (the latter still `unreachable!()`, never reached for codex).
  - `first_wake_action(Codex, true)` → `CreateChatNoSeed`; `(Codex, false)` → `Arm` (unchanged);
    claude arms unchanged. Update the tests pinning the old `Status` message.
  - `build_loop_command(Codex, …, Some("pmd-trust-x"))` inserts `-p pmd-trust-x` right after `codex`
    and before `resume`/`-m`; `None` and claude are byte-unchanged (pin the existing argv).
  - `ensure_codex_trust`: with a temp `$CODEX_HOME`, writes `<home>/pmd-trust-<hash>.config.toml`
    containing the exact `[projects."<dir>"]` block; a dir with a `.`/space in the path is quoted
    correctly; a second call is idempotent (same bytes); returns a filename-safe name.
- **Render/interaction:** the codex `WaitingFirstWake` Enter no longer dead-ends — it queues a chat
  (assert `pending_chat` is set with a codex fresh argv, lease dropped) rather than only a status.
- **Live (pmtui-ui-testing + a scratch `$CODEX_HOME`):** create a standard codex session, press
  Enter → a live codex REPL opens with **no** trust dialog (profile applied) and no "start pmd"
  message. (Use a scratch `$CODEX_HOME` seeded with the user's auth path or accept a login prompt in
  the isolated home; do NOT touch the real `~/.codex/config.toml`.)
- **Real-tmux acceptance (MANDATORY — launch/enter change):** `--ignored`, must stay green; fix any
  test asserting the old codex `unreachable!`/`Status` behavior (test-only).
- Standing: `cargo test`, clippy `--all-targets`, `fmt --check`.

## Out of scope

- pmd adopting/resuming a human-created codex conversation (no capturable id) — codex stays
  second-class for the autopilot-adopt flow.
- Reasoning-effort selection; any claude behavior; the create-form/model-picker UI (already done).
- Editing the user's base `~/.codex/config.toml` (the profile file is used precisely to avoid it).
