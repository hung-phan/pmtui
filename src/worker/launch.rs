//! Launching the agent that does the work: the two knobs every launch shares
//! ([`PermissionMode`], [`Resume`]) and the exact argv for the two shapes a worker takes —
//! the ephemeral headless `-p` worker and the persistent interactive REPL the daemon
//! nudges. One module because the two shapes must not drift: they share the `env` prefix,
//! the unattended permission posture, and the session-anchoring rules.

use std::path::{Path, PathBuf};

use crate::registry::Engine;
use crate::tmux::{ENV_BIN, ENV_SESSION, ENV_STATE_DIR};

/// Permission posture for a worker. Read-only phases (research) run in `Plan`;
/// code phases (implement) run in `AcceptEdits`. Agent-loop workers run in
/// `Auto`: `acceptEdits` only auto-approves file edits, so a headless agent-loop
/// worker could never use its OWN tools (e.g. its Slack MCP) — every non-edit
/// tool call is `permission_denied` with no prompt to grant it. `--permission-mode
/// auto` lets Claude Code's auto classifier decide per action (approve safe
/// actions, stop on dangerous ones), which is what an unattended assistant needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Plan,
    AcceptEdits,
    Auto,
}

impl PermissionMode {
    /// The value passed to `claude --permission-mode`.
    pub(super) fn claude_arg(self) -> &'static str {
        match self {
            PermissionMode::Plan => "plan",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Auto => "auto",
        }
    }
}

/// How a worker's conversation is anchored across wakes.
///
/// Phase workers are stateless: `Fresh { session_id: None }` adds
/// `--no-session-persistence` so nothing is kept between steps. Agent-loop / job
/// workers instead persist and resume ONE conversation by id, so the agent keeps
/// its working memory across heartbeat wakes:
/// - `Fresh { session_id: Some(id) }` pins a caller-chosen claude session id and
///   persists it (no `--no-session-persistence`). Codex has no caller-chosen id,
///   so it ignores `session_id` and mints its own (captured from `--json` later).
/// - `Continue(id)` resumes that specific conversation: claude `--resume <id>`,
///   codex `exec resume <id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    Fresh { session_id: Option<String> },
    Continue(String),
}

/// Whether persistent interactive launches should install the turn-completion hook.
/// Shared by pmd and pmtui so a Standard terminal can become Autopilot without
/// changing its instrumentation.
pub fn turn_hook_enabled() -> bool {
    turn_hook_value_enabled(std::env::var("PM_TURN_HOOK").ok().as_deref())
}

pub(super) fn turn_hook_value_enabled(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("off" | "0" | "false"))
}

/// Build the argv to launch a phase worker for `engine`. The worker is told (in
/// `prompt`) to write its machine result to the result path as its final action;
/// this only constructs the invocation. `add_dirs` grants extra tool/write dirs.
/// `resume` selects session anchoring (see [`Resume`]): a stateless fresh worker
/// (`Fresh { session_id: None }`, the phase-worker default), a fresh worker with a
/// pinned + persisted id, or resuming a specific conversation by id.
///
/// The prompt is always the trailing positional after an unconditional `--`
/// separator, with every flag (including claude's **variadic** `--add-dir`)
/// before it. This is verified against both real CLIs and closes three traps at
/// once: a dash-leading prompt parsed as a flag, a prompt equal to a codex
/// subcommand (`review`/`resume`/`help`), and claude's variadic `--add-dir`
/// swallowing a trailing prompt (`--` terminates the variadic collection).
///
/// The claude arm removes only the inherited `CLAUDECODE` nesting marker. Host hooks and
/// security policy remain inherited.
pub fn build_command(
    engine: Engine,
    prompt: &str,
    permission: PermissionMode,
    add_dirs: &[PathBuf],
    resume: Resume,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    match engine {
        Engine::Claude => {
            // `env -u CLAUDECODE` so a worker launched from inside a Claude Code
            // session doesn't inherit the parent's session marker.
            argv.extend(["env", "-u", "CLAUDECODE"].iter().map(|s| s.to_string()));
            argv.push("claude".into());
            argv.push("-p".into());
            argv.push("--permission-mode".into());
            argv.push(permission.claude_arg().into());
            // Session anchoring (this slot keeps a stateless phase worker's argv
            // byte-identical to before): stateless => --no-session-persistence;
            // a pinned id => --session-id (persist so a later Continue resumes it);
            // Continue => --resume <id>. Verified live: --session-id then --resume
            // round-trips full context headlessly (S0).
            match &resume {
                Resume::Fresh { session_id: None } => {
                    argv.push("--no-session-persistence".into());
                }
                Resume::Fresh {
                    session_id: Some(id),
                } => {
                    argv.push("--session-id".into());
                    argv.push(id.clone());
                }
                Resume::Continue(id) => {
                    argv.push("--resume".into());
                    argv.push(id.clone());
                }
            }
            argv.push("--output-format".into());
            argv.push("stream-json".into());
            argv.push("--verbose".into());
            argv.push("--forward-subagent-text".into());
            for d in add_dirs {
                argv.push("--add-dir".into());
                argv.push(d.to_string_lossy().into_owned());
            }
        }
        Engine::Codex => {
            argv.push("codex".into());
            argv.push("exec".into());
            match &resume {
                // A fresh codex run: set the sandbox + extra dirs on `exec`. Codex
                // has no caller-chosen id, so a pinned `session_id` is ignored here
                // (we capture the id codex assigns from its `--json` stream later).
                Resume::Fresh { .. } => {
                    argv.push("-s".into());
                    argv.push("workspace-write".into());
                    argv.push("--skip-git-repo-check".into());
                    argv.push("--json".into());
                    for d in add_dirs {
                        argv.push("--add-dir".into());
                        argv.push(d.to_string_lossy().into_owned());
                    }
                }
                // Resume a specific session. NOTE: `codex exec resume` does NOT
                // accept `-s/--sandbox` or `--add-dir` (they live only on the base
                // `codex exec`); the resumed session carries its own recorded
                // sandbox. So we pass only the flags `exec resume` actually
                // accepts. `<id>` is the SESSION_ID positional, before the flags.
                Resume::Continue(id) => {
                    argv.push("resume".into());
                    argv.push(id.clone());
                    argv.push("--skip-git-repo-check".into());
                    argv.push("--json".into());
                }
            }
        }
    }
    // The prompt is the trailing positional after `--` for BOTH engines: `--`
    // ends option parsing (so a dash-leading prompt is positional), ends codex
    // subcommand matching, and terminates claude's variadic `--add-dir`.
    argv.push("--".into());
    argv.push(prompt.to_string());
    argv
}

/// Build the argv for a spawned JOB child: ONE headless run that does the task, reports a result and
/// EXITS.
///
/// This is [`build_command`]'s one-shot shape plus the flags an UNATTENDED run needs, so the argv
/// knowledge verified against both real CLIs stays in one place. The extra flags are spliced in ahead
/// of the trailing `--` + prompt, which must remain the last two elements for the reasons
/// [`build_command`] documents.
///
/// - [`PermissionMode::Auto`] is the only posture that WORKS unattended, and the one the persistent
///   autopilot agent already runs under: claude's auto classifier decides per action, approving safe
///   work itself. Under `acceptEdits` a job could write files but every Bash call was DENIED, so a child
///   asked to build or test anything reported failure having run nothing — seen in a real job's own
///   transcript ("Permission for this tool use was denied"). A job is no more trusted than the agent
///   pmd drives in the same folder; it is the same posture, for the same reason.
/// - `--permission-prompts none` (claude) is why a job cannot wedge on a prompt nobody will answer:
///   the request is denied, the model is told not to retry, and the run continues. Codex's `exec` is
///   already non-interactive under `-s workspace-write`.
/// - `--json-schema` / `--output-schema` ask for a final payload shaped like
///   [`crate::spawn::result_schema_json`], so the outcome a receipt reports is the HARNESS's output
///   rather than a file the agent has to remember to write.
/// - codex also gets `-o`, which writes that final message to a file; claude's equivalent arrives in
///   the `result` event of the tee'd stream.
/// - A pinned `--session-id` (claude) leaves a conversation a human or the parent can resume, so a job
///   that ends `needs_human` or `failed` need not be restarted from nothing. Codex has no
///   caller-chosen id.
///
/// Deliberately NOT passed: `--bare`. The child is doing project work and needs the project's
/// `CLAUDE.md` and skills; its directory is already allowlisted before any request is staged.
///
/// The argv also UNSETS the managed-session variables. A job is handed no [`ManagedEnv`], but "not
/// handed" is not "absent": that env reaches a pane only through `tmux new-session -e`, which
/// [`Driver::spawn_step`] does not use, so a job's pane inherits the tmux server's environment — and
/// that server inherited the DASHBOARD's. A dashboard run from inside a managed terminal would
/// otherwise give every job its own `PMTUI_SESSION`, and `pmtui spawn` inside that job would publish
/// requests as the dashboard's session. Found on a real terminal, not by a unit test: the job child in
/// `a_child_cannot_spawn` created a grandchild row in the developer's own live dashboard.
///
/// [`ManagedEnv`]: crate::tmux::ManagedEnv
/// [`Driver::spawn_step`]: crate::tmux::Driver::spawn_step
pub fn build_job_command(
    engine: Engine,
    prompt: &str,
    model: Option<&str>,
    session_id: &str,
    schema: &Path,
    last_message: &Path,
) -> Vec<String> {
    let resume = match engine {
        Engine::Claude => Resume::Fresh {
            session_id: Some(session_id.to_string()),
        },
        // Codex assigns its own id; it is captured from the `--json` stream, never chosen here.
        Engine::Codex => Resume::Fresh { session_id: None },
    };
    let mut argv = build_command(engine, prompt, PermissionMode::Auto, &[], resume);
    let mut extra: Vec<String> = Vec::new();
    match engine {
        Engine::Claude => {
            extra.push("--permission-prompts".into());
            extra.push("none".into());
            // THE TWO CLIs TAKE THE SAME SCHEMA TWO WAYS, and getting it wrong kills the run before
            // the model ever sees the prompt: `claude --json-schema <schema>` wants the DOCUMENT
            // (handed a path it dies with "--json-schema is not valid JSON: Unrecognized token '/'"),
            // while `codex --output-schema <FILE>` wants the path. Verified against both `--help` and
            // against a real `claude -p` job, which the path form failed outright.
            extra.push("--json-schema".into());
            extra.push(crate::spawn::result_schema_json());
            if let Some(model) = model {
                extra.push("--model".into());
                extra.push(model.to_string());
            }
        }
        Engine::Codex => {
            extra.push("--output-schema".into());
            extra.push(schema.to_string_lossy().into_owned());
            extra.push("-o".into());
            extra.push(last_message.to_string_lossy().into_owned());
            if let Some(model) = model {
                extra.push("-m".into());
                extra.push(model.to_string());
            }
        }
    }
    // Ahead of `--`, never after it: everything past the separator is the prompt.
    let at = argv
        .iter()
        .position(|a| a == "--")
        .unwrap_or(argv.len().saturating_sub(1));
    argv.splice(at..at, extra);
    strip_managed_env(&mut argv);
    argv
}

/// Prefix `argv` with the `env -u` of every managed-session variable, reusing a leading `env` when
/// [`build_command`] already added one. See [`build_job_command`] for why a job must not inherit them.
fn strip_managed_env(argv: &mut Vec<String>) {
    let unsets = [ENV_SESSION, ENV_STATE_DIR, ENV_BIN]
        .into_iter()
        .flat_map(|name| ["-u".to_string(), name.to_string()]);
    if argv.first().is_some_and(|first| first == "env") {
        argv.splice(1..1, unsets);
    } else {
        argv.splice(0..0, std::iter::once("env".to_string()).chain(unsets));
    }
}

/// Build the argv for a `Mode::AgentLoop` session's ONE **persistent interactive**
/// agent — the long-lived engine REPL the daemon nudges via `tmux send-keys` and a
/// human attaches to (NOT the ephemeral headless `-p`/`stream-json` worker
/// [`build_command`] builds). Unlike a phase/wake worker it takes NO prompt: the
/// nudge is typed in later; this only launches the resumable REPL.
///
/// Both engines are prefixed with `env -u CLAUDECODE` so a child launched from Claude
/// Code is not rejected as nested. No host hook or guard is overridden.
///
/// The claude session runs in [`PermissionMode::Auto`] (`--permission-mode auto`),
/// the same posture the ephemeral agent-loop worker uses and the only one that
/// works UNATTENDED: Claude Code's auto classifier decides per action, approving
/// safe actions itself and stopping only on genuinely dangerous ones. Without it
/// the session falls back to the interactive default and the agent's FIRST tool
/// call raises an in-pane approval dialog that nobody is there to answer — and
/// because a dialog is not a bare prompt, `tmux::classify_pane` reads the pane as
/// `Busy`, so the scheduler re-parks the session until the stall backstop fires a
/// bogus "wedged" `Stuck`. (It only *appeared* to work on one box because that
/// environment exported `CLAUDE_CODE_ENABLE_AUTO_MODE=1` globally.) The value
/// comes from [`PermissionMode::Auto`] rather than a literal so this path and
/// [`build_command`] cannot drift; the argv is otherwise unchanged.
///
/// `resume` distinguishes CREATE (a freshly minted id) from RESUME (an existing
/// ledger id or an adopted chat seed) — mirroring the ephemeral path's [`Resume`].
/// `--session-id` CREATES a pinned conversation and ERRORS if it already exists, so a
/// relaunch (session died / pmd restart) and the adopted registry seed MUST resume:
/// - **claude Create** (`Fresh{session_id: Some(id)}`): `… claude --permission-mode auto --session-id <id> [--add-dir …]`.
/// - **claude Resume** (`Continue(id)`): `… claude --permission-mode auto --resume <id> [--add-dir …]`.
/// - **codex Resume** (`Continue(id)`): `… codex resume <id> --ask-for-approval never
///   --sandbox workspace-write` (codex has no caller-chosen id; it resumes the id codex
///   assigned earlier).
/// - **codex Create/fresh** (`Fresh{..}`): `… codex --ask-for-approval never --sandbox
///   workspace-write`.
///
/// Codex's unattended posture is the MEASURED analogue of claude's `--permission-mode
/// auto` (`--ask-for-approval never --sandbox workspace-write`, verified against
/// `codex 0.146.1.355`); `add_dirs` is intentionally not forwarded.
///
/// Directory trust is never injected. Codex's native prompt is surfaced for a human.
pub fn build_loop_command(
    engine: Engine,
    resume: &Resume,
    add_dirs: &[PathBuf],
    turn_signal: Option<&Path>,
    model: Option<&str>,
) -> Vec<String> {
    let mut argv: Vec<String> = ["env", "-u", "CLAUDECODE"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    match engine {
        Engine::Claude => {
            argv.push("claude".into());
            // Unattended posture, BEFORE the session-anchoring slot (mirrors the
            // flag order `build_command` uses). Sourced from the enum, never a
            // literal, so the ephemeral and persistent paths cannot drift.
            argv.push("--permission-mode".into());
            argv.push(PermissionMode::Auto.claude_arg().into());
            // Per-session worker model, sourced from the pmtui-owned registry
            // (`ProjectEntry.worker_model`). AFTER `--permission-mode auto` and BEFORE the
            // session-anchoring flags (`--session-id`/`--resume`), so it applies to the launch
            // itself. `None` OR an empty/whitespace value ⇒ no flag at all (claude uses its own
            // default) — a blank model is "no model set", not a launch with an empty id.
            if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
                argv.push("--model".into());
                argv.push(m.to_string());
            }
            // TURN-END EVENT (M71): a `Stop` hook, injected per-launch via `--settings` so
            // the user's own config is untouched, appends one byte to the per-session
            // turn-complete signal every time claude finishes a turn (goes idle at its
            // prompt). The daemon reads that file's size as a definitive "the agent went
            // idle since I nudged it" — the signal `classify_pane` cannot give while an
            // answer streams. `--settings` OVERRIDES only these keys, layering over the
            // user's settings for everything else.
            if let Some(sig) = turn_signal {
                argv.push("--settings".into());
                argv.push(claude_turn_hook_settings(sig));
            }
            match resume {
                // A freshly-minted id CREATES the pinned conversation.
                Resume::Fresh {
                    session_id: Some(id),
                } => {
                    argv.push("--session-id".into());
                    argv.push(id.clone());
                }
                // An existing id (persisted ledger cid, or an adopted chat seed) RESUMES.
                Resume::Continue(id) => {
                    argv.push("--resume".into());
                    argv.push(id.clone());
                }
                // Claude always has an id (mint → Fresh{Some}, or Continue); a bare
                // fresh session with no caller-chosen id is a defensive fallback only.
                Resume::Fresh { session_id: None } => {}
            }
            for d in add_dirs {
                argv.push("--add-dir".into());
                argv.push(d.to_string_lossy().into_owned());
            }
        }
        Engine::Codex => {
            argv.push("codex".into());
            // Per-session worker model (`ProjectEntry.worker_model`). `-m` is a GLOBAL option
            // (like `-c`), so it sits right after `codex` and BEFORE the optional `-c`/`resume`.
            // `None` OR an empty/whitespace value ⇒ no flag at all (codex uses its own default).
            if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
                argv.push("-m".into());
                argv.push(m.to_string());
            }
            // TURN-END EVENT (M71), the codex analogue of claude's `Stop` hook: codex's
            // top-level `notify` program is invoked on `agent-turn-complete` (the true idle
            // edge, once per completed turn), injected per-launch via the `-c key=value`
            // config override (value parsed as TOML) so the user's `~/.codex/config.toml` is
            // untouched. `-c` is a global option → placed right after `codex`, before the
            // `resume` subcommand. codex appends the event JSON as a FINAL argv (ignored by
            // our one-byte-append `sh -c`); the file's size is the completed-turn count the
            // daemon reads. Confirmed against codex 0.146.1.359.
            if let Some(sig) = turn_signal {
                argv.push("-c".into());
                argv.push(codex_turn_notify_config(sig));
            }
            // Codex has no caller-chosen id: resume an assigned one, else fresh.
            if let Resume::Continue(id) = resume {
                argv.push("resume".into());
                argv.push(id.clone());
            }
            // The unattended posture, MEASURED on the installed `codex 0.146.1.355`
            // (this replaces an "UNVERIFIED / none is invented" gap comment):
            //
            // - `--ask-for-approval never` and `--sandbox workspace-write` BOTH exist on
            //   the interactive form (`codex [OPTIONS] [PROMPT]`) AND on `codex resume`
            //   — checked in `--help` for each, which is why the same pair is appended
            //   after the `resume <id>` positional rather than only on the fresh arm.
            //   (Note this is NOT true of the headless `codex exec resume` that
            //   [`build_command`] drives — see that arm.)
            // - `never` is confirmed to TAKE EFFECT, not just parse: the running TUI's
            //   status line reports `· never ·`, and a live session then ran multiple
            //   shell commands and wrote a file with ZERO approval prompts. That is the
            //   behaviour claude gets from `--permission-mode auto`, so this pair is the
            //   analogue — the whole point being that an unattended agent must be able to
            //   use its own tools without a dialog nobody is there to answer.
            // - `workspace-write` is a REAL sandbox, not decoration: under codex's
            //   sandbox `$HOME` is mounted read-only (measured via `codex sandbox`),
            //   while the workspace and `/tmp` are writable.
            // - `--dangerously-bypass-approvals-and-sandbox` is the WRONG choice and is
            //   deliberately not used. Per its own `--help` it runs "without sandboxing"
            //   (i.e. the `danger-full-access` end of the ladder
            //   `read-only | workspace-write | danger-full-access`), so it is strictly
            //   stronger than needed — and it buys nothing: measured, it does not even
            //   skip the trust dialog below, which was the only plausible reason to want
            //   it.
            //
            // A first-run directory-trust dialog remains in the pane. The dialog classifier
            // escalates it so a human can attach and decide; agent-manager never approves it.
            argv.push("--ask-for-approval".into());
            argv.push("never".into());
            argv.push("--sandbox".into());
            argv.push("workspace-write".into());
            // `add_dirs` is deliberately NOT forwarded. `--add-dir` DOES exist on both
            // interactive forms on this version (an older comment here claimed the
            // resume form had no such flag — that is false now), but it would be
            // redundant: the only caller passes `[work_dir]`, and `work_dir` is also the
            // cwd `tmux::Driver::launch_interactive` starts the session in, so it is
            // already codex's primary writable workspace under `workspace-write`.
        }
    }
    argv
}

/// Build a persistent interactive session that remains human-controlled while the
/// row is Standard. It carries the same turn-completion hook as the unattended
/// [`build_loop_command`] path because switching the existing terminal to Autopilot
/// must not require a restart.
pub fn build_standard_command(
    engine: Engine,
    resume: &Resume,
    turn_signal: Option<&Path>,
    model: Option<&str>,
) -> Vec<String> {
    let mut argv = Vec::new();
    match engine {
        Engine::Claude => {
            argv.extend(["env", "-u", "CLAUDECODE"].iter().map(|s| s.to_string()));
            argv.push("claude".into());
            if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
                argv.push("--model".into());
                argv.push(m.to_string());
            }
            if let Some(sig) = turn_signal {
                argv.push("--settings".into());
                argv.push(claude_turn_hook_settings(sig));
            }
            match resume {
                Resume::Fresh {
                    session_id: Some(id),
                } => {
                    argv.push("--session-id".into());
                    argv.push(id.clone());
                }
                Resume::Continue(id) => {
                    argv.push("--resume".into());
                    argv.push(id.clone());
                }
                Resume::Fresh { session_id: None } => {}
            }
        }
        Engine::Codex => {
            argv.push("codex".into());
            if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
                argv.push("-m".into());
                argv.push(m.to_string());
            }
            if let Some(sig) = turn_signal {
                argv.push("-c".into());
                argv.push(codex_turn_notify_config(sig));
            }
            if let Resume::Continue(id) = resume {
                argv.push("resume".into());
                argv.push(id.clone());
            }
        }
    }
    argv
}

/// Build a persistent interactive session that branches from an existing
/// conversation without modifying it. Forked sessions start human-controlled;
/// switching the resulting row to Autopilot reuses the ordinary resume path.
///
/// Both engines own the child identity. Claude reports it through an injected
/// `SessionStart` hook; Codex callers capture it from the exact live process.
pub fn build_fork_command(
    engine: Engine,
    source_id: &str,
    turn_signal: Option<&Path>,
    identity_sink: Option<&Path>,
    model: Option<&str>,
) -> Vec<String> {
    match engine {
        Engine::Claude => {
            let mut argv = vec![
                "env".into(),
                "-u".into(),
                "CLAUDECODE".into(),
                "claude".into(),
            ];
            if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
                argv.push("--model".into());
                argv.push(model.to_string());
            }
            if turn_signal.is_some() || identity_sink.is_some() {
                argv.push("--settings".into());
                argv.push(claude_fork_settings(turn_signal, identity_sink));
            }
            argv.push("--resume".into());
            argv.push(source_id.to_string());
            argv.push("--fork-session".into());
            argv
        }
        Engine::Codex => {
            let mut argv = build_standard_command(
                engine,
                &Resume::Fresh { session_id: None },
                turn_signal,
                model,
            );
            argv.push("fork".into());
            argv.push(source_id.to_string());
            argv
        }
    }
}

/// The shell command both turn-end hooks run: append ONE byte to the per-session
/// turn-complete signal (its SIZE is the completed-turn count the daemon reads),
/// creating the `.daemon` directory first so it works before any chat has created it.
/// `printf .` writes a single byte, no newline. Both paths are POSIX-escaped with
/// [`crate::tmux::shq`] rather than wrapped in literal single quotes: the session tree is
/// sanitized today, but a project root carrying a `'` (or any shell metacharacter) would
/// otherwise break the hook command apart — degrading every turn-end signal to the
/// fingerprint gate for that session. If the hook's `mkdir`/append ever fails, the file
/// simply never appears — which the daemon reads as "no event" and falls back to the
/// fingerprint gate, the fail-safe direction.
fn turn_hook_shell(turn_signal: &Path) -> String {
    let file = turn_signal.to_string_lossy();
    let dir = turn_signal
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "mkdir -p {} && printf . >> {}",
        crate::tmux::shq(&dir),
        crate::tmux::shq(&file)
    )
}

/// A `--settings` JSON value registering [`turn_hook_shell`] as claude's `Stop` hook.
/// Built with `serde_json` so the command string (quotes, `&&`, `>>`) is escaped into the
/// JSON correctly. Shape:
/// `{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"<cmd>"}]}]}}`.
fn claude_turn_hook_settings(turn_signal: &Path) -> String {
    serde_json::json!({
        "hooks": {
            "Stop": [ { "hooks": [ { "type": "command", "command": turn_hook_shell(turn_signal) } ] } ]
        }
    })
    .to_string()
}

fn claude_fork_settings(turn_signal: Option<&Path>, identity_sink: Option<&Path>) -> String {
    let mut hooks = serde_json::Map::new();
    if let Some(turn_signal) = turn_signal {
        hooks.insert(
            "Stop".into(),
            serde_json::json!([{
                "hooks": [{"type": "command", "command": turn_hook_shell(turn_signal)}]
            }]),
        );
    }
    if let Some(identity_sink) = identity_sink {
        hooks.insert(
            "SessionStart".into(),
            serde_json::json!([{
                "hooks": [{"type": "command", "command": session_id_hook_shell(identity_sink)}]
            }]),
        );
    }
    serde_json::json!({"hooks": hooks}).to_string()
}

fn session_id_hook_shell(identity_sink: &Path) -> String {
    let file = identity_sink.to_string_lossy();
    let temp = format!("{file}.tmp");
    let dir = identity_sink
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "mkdir -p {} && printf '%s' \"$CLAUDE_CODE_SESSION_ID\" > {} && mv {} {}",
        crate::tmux::shq(&dir),
        crate::tmux::shq(&temp),
        crate::tmux::shq(&temp),
        crate::tmux::shq(&file),
    )
}

/// The codex half of [`turn_hook_shell`]: append the turn byte, then ALSO record the conversation
/// id out of the event payload.
///
/// Codex exposes no caller-chosen id, so its `agent-turn-complete` payload is the only place a
/// launcher can learn one without reading the live process's open files out of `/proc` — Linux-only,
/// and racing codex's own startup. codex runs `sh -c '<cmd>' '<event-json>'`, so the payload is `$0`.
///
/// Three things make reading that payload with `grep` safe, given it also carries `input_messages`
/// and `last_assistant_message` — text the AGENT controls:
///
/// 1. The pattern matches a UUIDv7 SHAPE (`…-7xxx-…`), not merely `"thread_id":"…"`, so prose that
///    just mentions the field name cannot match.
/// 2. `head -n1` takes the FIRST match. `thread_id` serializes before both agent-controlled fields,
///    so the real id comes first — a shape-matching forgery would have to appear earlier, which
///    needs serde to reorder the payload.
/// 3. The capture is sequenced with `;`, not `&&`, and guarded on a non-empty result. A payload
///    that yields nothing leaves any previous id in place and NEVER breaks the turn byte, which is
///    the load-bearing half; the id is the additive one.
fn codex_turn_hook_shell(turn_signal: &Path) -> String {
    // SIBLING paths in one directory, so `turn_hook_shell`'s `mkdir -p` already made it. Derived
    // rather than passed, so the six launch call sites need no second argument;
    // `codex_identity_sink_is_the_paths_sibling` pins it to `ProjectPaths::codex_conversation_id`.
    let sink = identity_sink_beside(turn_signal);
    let sink = sink.to_string_lossy();
    let temp = format!("{sink}.tmp");
    format!(
        "{}; id=$(printf '%s' \"$0\" | grep -oE '\"thread_id\":\"[0-9a-f]{{8}}-[0-9a-f]{{4}}-7[0-9a-f]{{3}}-[0-9a-f]{{4}}-[0-9a-f]{{12}}\"' | head -n1 | cut -d'\"' -f4); [ -n \"$id\" ] && printf '%s' \"$id\" > {} && mv {} {}",
        turn_hook_shell(turn_signal),
        crate::tmux::shq(&temp),
        crate::tmux::shq(&temp),
        crate::tmux::shq(&sink),
    )
}

/// [`ProjectPaths::codex_conversation_id`](crate::state::ProjectPaths::codex_conversation_id) for
/// whichever session owns `turn_signal`. The two are siblings by definition — both live in one
/// session's `daemon_dir` — so the hook builder derives one from the other instead of every caller
/// threading a second path it would read off the same `ProjectPaths`.
fn identity_sink_beside(turn_signal: &Path) -> PathBuf {
    turn_signal.with_file_name("conversation-id")
}

/// A `-c notify=[...]` config override registering [`codex_turn_hook_shell`] as codex's `notify`
/// program (a TOML array of argv).
fn codex_turn_notify_config(turn_signal: &Path) -> String {
    // Escape the command for a TOML basic string (the claude side gets this for free via
    // serde_json). A project-root path can legally contain `"` or `\`; left unescaped either
    // would malform the `-c` TOML and codex would REJECT it at startup, failing the launch
    // itself — worse than the busy-detection bug the hook fixes. Backslash first, so the
    // backslashes introduced by escaping `"` are not doubled. Single quotes and spaces need
    // no escaping (safe in a TOML basic string and, single-quoted, in the inner shell).
    let cmd = codex_turn_hook_shell(turn_signal)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("notify=[\"sh\", \"-c\", \"{cmd}\"]")
}
