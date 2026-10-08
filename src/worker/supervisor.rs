//! The supervisor consult: one headless, model-pinned, clock-bounded `claude` that is
//! handed a goal plus a worker's question and returns small JSON. Separate from
//! [`super::launch`] because it is a deliberately different shape — one JSON envelope
//! rather than a token stream — with its own model pin and spend knob, and every flag
//! here was measured against the installed CLI rather than assumed.

use super::launch::PermissionMode;

/// The model the SUPERVISOR consult is pinned to (m20). See
/// [`build_supervisor_command`] for why an explicit full id is mandatory and why this
/// particular one; override with `PM_SUPERVISOR_MODEL` for a different deployment.
pub const SUPERVISOR_MODEL: &str = "global.anthropic.claude-sonnet-4-6";

/// Env var overriding [`SUPERVISOR_MODEL`] — the id that resolves is deployment
/// specific (see [`build_supervisor_command`]), so it must be changeable without a
/// rebuild.
pub const SUPERVISOR_MODEL_ENV: &str = "PM_SUPERVISOR_MODEL";

/// Env var setting a per-consult spend ceiling (`claude --max-budget-usd`). UNSET BY DEFAULT,
/// which means no cap is passed at all.
///
/// This started at `0.25`, then `1.00` when the supervisor gained tools. Both were wrong in the
/// same way, and the user's ruling is explicit: *"i don't think you need to worry too much on
/// budget or cost. we can have some settings to config that, but the default shouldn't worry
/// about budget."*
///
/// The reason a default cap is actively harmful here, not merely conservative: hitting it does not
/// produce "too expensive", it produces a TRUNCATED consult, which the harness reads as a
/// missing/unparseable reply, which becomes a `Capability` escalation and a step toward the
/// refusal latch. So a ceiling tuned too low silently converts "the supervisor was thinking" into
/// "wake the human" — the exact outcome autopilot exists to avoid — and no escalation text names
/// it. A budget guess is a decision about how hard the harness is allowed to think, and that is
/// the operator's to make, not a constant's.
///
/// Set it to opt back in (e.g. `PM_SUPERVISOR_BUDGET_USD=2.50`). Runaway spend is still bounded
/// without it, by the shell `timeout` wrapper and the harness's own reap deadline.
pub const SUPERVISOR_BUDGET_USD_ENV: &str = "PM_SUPERVISOR_BUDGET_USD";

/// Env pinning the CODEX decider's model (parallel to [`SUPERVISOR_MODEL_ENV`]). UNSET BY
/// DEFAULT — unlike claude, `codex exec` resolves a model on its own (verified live: it ran on
/// `openai.gpt-5.5` with no `-m`), so we pass `-m` only when the operator names one, rather than
/// inventing a default id that is wrong on another deployment.
pub const SUPERVISOR_CODEX_MODEL_ENV: &str = "PM_SUPERVISOR_CODEX_MODEL";

// There was a `SUPERVISOR_DENIED_TOOLS` here, denying every built-in tool. Its argument was
// that the supervisor is "asked for ONE judgement about text it was handed, so it needs no tools
// at all", and that letting it read the repo was "latency and cost with no upside".
//
// That was wrong about the upside, and the user's instruction is explicit: *"the light session
// on claude for pmd when driving, it can have access to all the tools it needs, we should not
// restrict that."*
//
// A decision-maker that can only see the question text is guessing. The whole point of autopilot
// is that a human is involved ONLY when the goal genuinely needs one — and the difference between
// "escalate this" and "handle it" is almost always a fact in the tree: does that file exist, did
// the test pass, is the thing the worker says it did actually done. Denying `Read`/`Grep`/`Glob`
// guaranteed the supervisor could never check any of it, so decisions went back to the human for
// want of a look.
//
// What is NOT lifted is `--permission-mode plan`: the supervisor still cannot WRITE. That is not
// a restriction on the tools it needs, it is the two-writers problem — the main agent is editing
// this tree right now, and a second process mutating it mid-turn would corrupt work neither of
// them can see. Judgement needs reads; it does not need edits.

/// Build the argv for ONE **supervisor consult** (m20): a headless, tool-less,
/// model-pinned, cost-capped, shell-timeout-bounded `claude` that reads a goal plus a
/// worker's question and returns small JSON.
///
/// Deliberately NOT [`build_command`]: that hardcodes `--output-format stream-json
/// --verbose --forward-subagent-text` (a token stream for a phase worker), which is the
/// wrong shape entirely — we want one JSON object.
///
/// Every flag below was verified against the installed CLI (`claude 2.1.233.660`), and
/// three of those checks changed the design:
///
/// - **`--model` MUST be a full id, and a wrong one fails SILENTLY.** Measured on this
///   deployment (`CLAUDE_CODE_USE_BEDROCK=1`): `--model haiku` and `--model sonnet` were
///   both ignored and the call ran on the ambient `claude-opus-5`; `--model
///   claude-haiku-4-5` fell through to the configured `fallbackModel`; and
///   `--model total-nonsense-model-xyz` produced no error at all. Only the fully
///   qualified [`SUPERVISOR_MODEL`] was confirmed to actually take effect (its id came
///   back in the reply's `modelUsage`). So the pin is a full id, and it is worth
///   re-measuring on any other deployment — hence [`SUPERVISOR_MODEL_ENV`]. **No haiku
///   id resolved here**, so pinning haiku would have been a lie that quietly cost
///   opus-5 rates.
/// - **`--bare` is where the cost actually goes.** Without it a trivial consult
///   measured 45,508 input tokens (CLAUDE.md, hooks, plugin + MCP tool schemas, memory)
///   and $0.291; with it, 1,574 tokens. Combined with the model pin, a trivial consult
///   measured $0.0057 — **51× cheaper**. It also shrinks the injection surface (no
///   auto-discovered CLAUDE.md or MCP tools) and skips hooks. Its auth caveat does not
///   bite here: Bedrock supplies its own credentials, which was confirmed live.
/// - **There is NO `--max-turns`.** `claude --help` has no such flag on this version, so turn
///   count is bounded only indirectly — by the `timeout` wrapper and the harness's own reap
///   deadline. Two things that USED to appear in this list no longer do: "having no tools to
///   call" (the supervisor now has read-only tools, and is told to use them) and
///   `--max-budget-usd` (opt-in via [`SUPERVISOR_BUDGET_USD_ENV`], absent by default). So the
///   clock is the only bound that always applies — which is the right one, because it is the
///   bound whose expiry the harness observes as an honest non-zero exit.
///
/// `env -u CLAUDECODE` removes only the inherited nesting marker. Host hooks remain
/// governed by the user's environment; `--bare` supplies the isolated consult posture.
///
/// `timeout -k <grace> <secs>` bounds the process at the SHELL level. It is only the
/// outer of two bounds: the caller enforces its own reap deadline in Rust, independently
/// of observation, because a headless `claude -p` has been measured hanging for 180
/// seconds — 3× a 60s cadence — and the sweep must never wait on it.
///
/// Claude-only, and unconditionally so even when the WORKER runs codex: only claude's
/// headless flags are verified here for THIS shape (the one-JSON-envelope consult), so no
/// codex equivalent is invented.
///
/// On a box with no `claude` the SPAWN ITSELF SUCCEEDS — this used to claim it "simply
/// fails", which is false and actively misleading: the argv's first word is `env`, not
/// `claude`, so the kernel execs `env` fine and `env` then exits **127** when it cannot
/// find `claude` on `PATH`. There is no spawn error for the caller to latch. The missing
/// binary therefore surfaces as a normal non-zero exit / empty stdout on the consult, and
/// must be diagnosed there. A future reader must not use this comment to dismiss a real
/// bug as "the spawn would have failed".
///
// The two links above point at the sibling module that now holds those builders; written
// as reference definitions so not one word of the prose had to move for the split.
/// [`build_command`]: super::build_command
/// [`build_loop_command`]: super::build_loop_command
pub fn build_supervisor_command(
    model: &str,
    system_prompt: &str,
    schema: &str,
    prompt: &str,
    timeout_s: u64,
    kill_grace_s: u64,
    // `Some` only when the operator set `SUPERVISOR_BUDGET_USD_ENV`; `None` (the default)
    // passes no `--max-budget-usd` at all.
    budget_usd: Option<&str>,
) -> Vec<String> {
    let mut argv: Vec<String> = ["env", "-u", "CLAUDECODE"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.push("timeout".into());
    argv.push("-k".into());
    argv.push(kill_grace_s.to_string());
    argv.push(timeout_s.to_string());
    argv.push("claude".into());
    argv.push("--bare".into());
    argv.push("-p".into());
    argv.push("--model".into());
    argv.push(model.to_string());
    // READ-ONLY, not tool-less. `plan` refuses every write while still permitting reads and
    // searches, which is exactly the posture a decision-maker needs: it can go and check the
    // claim it is being asked about, and it cannot touch the tree the main agent is editing.
    // The explicit deny list that used to sit here is gone — see the note above the constant
    // that held it.
    argv.push("--permission-mode".into());
    argv.push(PermissionMode::Plan.claude_arg().into());
    argv.push("--disable-slash-commands".into());
    // ONE JSON envelope, not a token stream. `--json-schema` additionally returns the
    // reply pre-validated in `structured_output`; the harness's own validator never
    // assumes the schema was honored (see `crate::advise::validate`).
    argv.push("--output-format".into());
    argv.push("json".into());
    argv.push("--json-schema".into());
    argv.push(schema.to_string());
    argv.push("--no-session-persistence".into());
    // OPT-IN. No flag unless the operator asked for one — see `SUPERVISOR_BUDGET_USD_ENV` for
    // why a default ceiling turns thinking into an escalation.
    if let Some(cap) = budget_usd {
        argv.push("--max-budget-usd".into());
        argv.push(cap.to_string());
    }
    argv.push("--system-prompt".into());
    argv.push(system_prompt.to_string());
    // The prompt is the trailing positional after `--`, for the same three reasons as
    // [`build_command`] (a dash-leading prompt, subcommand shadowing, variadic flags).
    argv.push("--".into());
    argv.push(prompt.to_string());
    argv
}

/// Build the argv for ONE supervisor consult run on **codex** instead of claude.
///
/// A different shape from [`build_supervisor_command`], because `codex exec` has none of the
/// claude flags that builder leans on: no `--bare`, `--json-schema`, `--output-format json`,
/// `--system-prompt`, or `--permission-mode`. So:
///
/// - `-s read-only` is codex's equivalent of claude's `--permission-mode plan`: the decider may
///   read/search the tree (the whole point — it verifies the claim it is judging) but may NOT
///   write (the worker owns this tree; two writers corrupt it). Verified against codex 0.146.1.
/// - The system instructions are PREPENDED into the prompt, since there is no `--system-prompt`.
///   `advise::validate` does not care how they were delivered, and the decider's JSON contract is
///   fully described in that prose (it is not carried by any schema flag).
/// - The verdict is captured with `--output-last-message <path>`: codex writes ONLY its final
///   assistant message there, so the reap reads a clean object rather than scraping the log.
/// - `-m` is passed ONLY when `model` is `Some` — see [`SUPERVISOR_CODEX_MODEL_ENV`].
/// - `timeout -k <grace> <secs>` is the identical outer bound as the claude path.
/// - No Claude-specific environment prefix is needed.
///
/// Like the claude builder this is PURE over its arguments (env reads happen at the call site).
/// Safe by construction: whatever codex emits, [`crate::advise::validate`] re-derives every
/// guarantee, so a non-verdict reply escalates to a human rather than being trusted.
pub fn build_supervisor_command_codex(
    model: Option<&str>,
    system_prompt: &str,
    prompt: &str,
    last_message_path: &str,
    timeout_s: u64,
    kill_grace_s: u64,
) -> Vec<String> {
    // Seeded from a literal (rather than `Vec::new()` + a run of `push`) so the fixed prefix reads
    // in one place and clippy's `vec_init_then_push` stays quiet — same posture as the claude
    // builder above, which seeds its `env …` prefix the same way.
    let mut argv: Vec<String> = vec![
        "timeout".into(),
        "-k".into(),
        kill_grace_s.to_string(),
        timeout_s.to_string(),
        "codex".into(),
        "exec".into(),
        "-s".into(),
        "read-only".into(),
        "--skip-git-repo-check".into(),
        "--output-last-message".into(),
        last_message_path.to_string(),
    ];
    if let Some(m) = model {
        argv.push("-m".into());
        argv.push(m.to_string());
    }
    // No --system-prompt on codex: fold it into the prompt head. The prompt is the trailing
    // positional after `--`, same three reasons as the claude builder (dash-leading text,
    // subcommand shadowing, variadic flags).
    argv.push("--".into());
    argv.push(format!("{system_prompt}\n\n{prompt}"));
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_consult_argv_is_read_only_bounded_and_prepends_the_system_prompt() {
        let argv = build_supervisor_command_codex(
            Some("openai.gpt-5.5"),
            "SYS-PROMPT-MARKER",
            "USER-PROMPT-MARKER",
            "/tmp/advice-7.last",
            180,
            5,
        );
        let joined = argv.join(" ");
        // Shell-timeout bound, same outer bound as the claude path.
        assert_eq!(&argv[0], "timeout");
        assert!(argv.windows(3).any(|w| w == ["-k", "5", "180"]));
        // codex, non-interactive, READ-ONLY (mirrors claude's --permission-mode plan: reads yes,
        // writes never — the worker owns the tree), outside-a-git-repo tolerant.
        assert!(argv.contains(&"codex".to_string()));
        assert!(argv.contains(&"exec".to_string()));
        assert!(argv.windows(2).any(|w| w == ["-s", "read-only"]));
        assert!(argv.contains(&"--skip-git-repo-check".to_string()));
        // The verdict is isolated into the last-message file (robust vs. scraping chatter).
        assert!(
            argv.windows(2)
                .any(|w| w == ["--output-last-message", "/tmp/advice-7.last"])
        );
        // Model pin passed when Some.
        assert!(argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.5"]));
        // codex has NO --json-schema / --bare / --system-prompt: the system prompt rides IN the
        // prompt, after `--`.
        assert!(!joined.contains("--json-schema"));
        assert!(!joined.contains("--system-prompt"));
        assert!(!argv.contains(&"--bare".to_string()));
        let dashdash = argv
            .iter()
            .position(|a| a == "--")
            .expect("prompt is the trailing positional");
        let tail = argv[dashdash + 1..].join(" ");
        assert!(tail.contains("SYS-PROMPT-MARKER") && tail.contains("USER-PROMPT-MARKER"));
        // NOT the claude env prefix — that is claude-hook-specific (the codex worker arm omits it too).
        assert_ne!(&argv[0], "env");
    }

    #[test]
    fn codex_consult_omits_model_flag_when_unset() {
        let argv = build_supervisor_command_codex(None, "s", "p", "/tmp/x.last", 180, 5);
        assert!(
            !argv.contains(&"-m".to_string()),
            "no -m when the model pin is unset (codex uses its default)"
        );
    }
}
