//! Tests for phase-worker dispatch. Split out of `worker.rs` when it became
//! `worker/mod.rs`; a child module still sees its ancestors' private items, so this is a
//! pure move. `use super::*` now reaches the re-exports on `mod.rs`; the names that used
//! to arrive through the file's own `use` lines are imported here directly.

use std::path::PathBuf;

use crate::registry::Engine;

use super::*;

mod fork;
mod job;

#[test]
fn turn_hook_kill_switch_parsing_is_shared_and_explicit() {
    for disabled in ["off", "0", "false", " off "] {
        assert!(
            !super::launch::turn_hook_value_enabled(Some(disabled)),
            "{disabled:?}"
        );
    }
    for enabled in [None, Some(""), Some(" "), Some("on"), Some("OFF")] {
        assert!(
            super::launch::turn_hook_value_enabled(enabled),
            "{enabled:?}"
        );
    }
}

#[test]
fn standard_persistent_commands_keep_human_posture_and_install_turn_hook() {
    let signal = PathBuf::from("/tmp/project/.project-state/sessions/a/.daemon/turn-complete");

    let claude = build_standard_command(
        Engine::Claude,
        &Resume::Continue("claude-session".into()),
        Some(&signal),
        None,
    );
    let settings_at = claude.iter().position(|arg| arg == "--settings").unwrap();
    let resume_at = claude.iter().position(|arg| arg == "--resume").unwrap();
    assert!(settings_at < resume_at, "{claude:?}");
    assert!(
        claude[settings_at + 1].contains(signal.to_str().unwrap()),
        "{claude:?}"
    );
    assert!(
        !claude.iter().any(|arg| arg == "--permission-mode"),
        "a Standard session keeps human-controlled permissions: {claude:?}"
    );

    let codex = build_standard_command(
        Engine::Codex,
        &Resume::Continue("codex-session".into()),
        Some(&signal),
        None,
    );
    let config_at = codex.iter().position(|arg| arg == "-c").unwrap();
    let resume_at = codex.iter().position(|arg| arg == "resume").unwrap();
    assert!(config_at < resume_at, "{codex:?}");
    assert!(
        codex[config_at + 1].contains(signal.to_str().unwrap()),
        "{codex:?}"
    );
    assert!(
        !codex.iter().any(|arg| arg == "--ask-for-approval"),
        "a Standard session keeps human-controlled approvals: {codex:?}"
    );
    assert!(
        !codex.iter().any(|arg| arg == "--sandbox"),
        "a Standard session keeps the host's sandbox posture: {codex:?}"
    );
}

#[test]
fn claude_command_has_the_exact_flags_prompt_after_separator() {
    let argv = build_command(
        Engine::Claude,
        "do the design",
        PermissionMode::Plan,
        &[],
        Resume::Fresh { session_id: None },
    );
    // Exact argv pins the nesting-marker removal while preserving host policy.
    assert_eq!(
        argv,
        vec![
            "env",
            "-u",
            "CLAUDECODE",
            "claude",
            "-p",
            "--permission-mode",
            "plan",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
            "--forward-subagent-text",
            "--",
            "do the design", // prompt is the trailing positional after `--`
        ]
    );
}

#[test]
fn child_launches_do_not_override_the_hosts_gateguard_policy() {
    for resume in [
        Resume::Fresh { session_id: None },
        Resume::Fresh {
            session_id: Some("11111111-1111-4111-8111-111111111111".into()),
        },
        Resume::Continue("11111111-1111-4111-8111-111111111111".into()),
    ] {
        for mode in [
            PermissionMode::Plan,
            PermissionMode::AcceptEdits,
            PermissionMode::Auto,
        ] {
            let argv = build_command(Engine::Claude, "work", mode, &[], resume.clone());
            assert!(
                !argv.iter().any(|arg| arg.starts_with("ECC_GATEGUARD=")),
                "agent-manager must inherit host policy: {argv:?}"
            );
        }
    }
    // codex is deliberately unprefixed here, exactly as before: it has no `env`
    // wrapper on this path and the gate hook is a Claude Code hook.
    let codex = build_command(
        Engine::Codex,
        "work",
        PermissionMode::AcceptEdits,
        &[],
        Resume::Fresh { session_id: None },
    );
    assert_eq!(codex[0], "codex", "{codex:?}");
}

#[test]
fn claude_accept_edits_with_add_dirs_before_separator() {
    let argv = build_command(
        Engine::Claude,
        "implement",
        PermissionMode::AcceptEdits,
        &[PathBuf::from("/extra/one"), PathBuf::from("/extra/two")],
        Resume::Fresh { session_id: None },
    );
    // permission-mode is acceptEdits; the variadic --add-dir flags come
    // BEFORE the `--`, and the prompt is the trailing positional after it —
    // so `--` terminates the variadic collection and the prompt survives.
    let joined = argv.join(" ");
    assert!(joined.contains("--permission-mode acceptEdits"), "{joined}");
    assert!(
        joined.ends_with(
            "--add-dir /extra/one --add-dir /extra/two --forward-subagent-text -- implement"
        ) || joined.ends_with(
            "--forward-subagent-text --add-dir /extra/one --add-dir /extra/two -- implement"
        ),
        "{joined}"
    );
    let n = argv.len();
    assert_eq!(argv[n - 2], "--");
    assert_eq!(argv[n - 1], "implement");
}

#[test]
fn claude_auto_permission_mode_for_agent_loop_worker() {
    // Agent-loop workers run in `auto` so the headless worker can use its own
    // tools (Slack MCP, etc.) — acceptEdits denies non-edit tool calls.
    let argv = build_command(
        Engine::Claude,
        "keep working the goal",
        PermissionMode::Auto,
        &[],
        Resume::Continue("sess-1".into()),
    );
    assert!(
        argv.join(" ").contains("--permission-mode auto"),
        "{argv:?}"
    );
}

#[test]
fn claude_dash_leading_prompt_is_positional_after_separator() {
    // A prompt beginning with `-` must not be parsed as a flag: it trails `--`.
    let argv = build_command(
        Engine::Claude,
        "--help me",
        PermissionMode::Plan,
        &[],
        Resume::Fresh { session_id: None },
    );
    let n = argv.len();
    assert_eq!(argv[n - 2], "--");
    assert_eq!(argv[n - 1], "--help me");
}

#[test]
fn codex_command_uses_exec_workspace_write() {
    let argv = build_command(
        Engine::Codex,
        "mechanical fix",
        PermissionMode::AcceptEdits,
        &[],
        Resume::Fresh { session_id: None },
    );
    assert_eq!(
        argv,
        vec![
            "codex",
            "exec",
            "-s",
            "workspace-write",
            "--skip-git-repo-check",
            "--json",
            "--",
            "mechanical fix",
        ]
    );
}

#[test]
fn codex_dash_leading_prompt_gets_separator() {
    // codex's prompt is the trailing positional; `--` (always emitted) keeps a
    // dash-leading prompt from being parsed as a flag.
    let argv = build_command(
        Engine::Codex,
        "-x sneaky",
        PermissionMode::Plan,
        &[],
        Resume::Fresh { session_id: None },
    );
    let n = argv.len();
    assert_eq!(argv[n - 2], "--");
    assert_eq!(argv[n - 1], "-x sneaky");
}

#[test]
fn codex_subcommand_name_prompt_is_guarded() {
    // A prompt equal to a codex subcommand (`review`/`resume`/`help`) must not
    // be hijacked as a subcommand: the unconditional `--` makes it positional.
    for name in ["review", "resume", "help"] {
        let argv = build_command(
            Engine::Codex,
            name,
            PermissionMode::AcceptEdits,
            &[],
            Resume::Fresh { session_id: None },
        );
        let n = argv.len();
        assert_eq!(argv[n - 2], "--", "{name}");
        assert_eq!(argv[n - 1], name, "{name}");
    }
}

#[test]
fn claude_fresh_with_pinned_session_id_persists() {
    // A pinned session id persists the conversation so a later Continue can
    // resume it: --session-id present, --no-session-persistence GONE.
    let argv = build_command(
        Engine::Claude,
        "boot the agent loop",
        PermissionMode::AcceptEdits,
        &[],
        Resume::Fresh {
            session_id: Some("11111111-1111-4111-8111-111111111111".into()),
        },
    );
    let joined = argv.join(" ");
    assert!(
        joined.contains("--session-id 11111111-1111-4111-8111-111111111111"),
        "{joined}"
    );
    assert!(
        !joined.contains("--no-session-persistence"),
        "a pinned session must persist: {joined}"
    );
    assert!(!joined.contains("--resume"), "{joined}");
    let n = argv.len();
    assert_eq!(argv[n - 2], "--");
    assert_eq!(argv[n - 1], "boot the agent loop");
}

#[test]
fn claude_continue_resumes_by_id_not_continue_flag() {
    // Each wake resumes THE SAME conversation by id (verified live, S0):
    // --resume <id>, persisted (no --no-session-persistence), never --continue.
    let argv = build_command(
        Engine::Claude,
        "keep working the goal",
        PermissionMode::AcceptEdits,
        &[],
        Resume::Continue("11111111-1111-4111-8111-111111111111".into()),
    );
    let joined = argv.join(" ");
    assert!(
        joined.contains("--resume 11111111-1111-4111-8111-111111111111"),
        "{joined}"
    );
    assert!(!joined.contains("--no-session-persistence"), "{joined}");
    assert!(!joined.contains("--session-id"), "{joined}");
    assert!(!joined.contains("--continue"), "{joined}");
    let n = argv.len();
    assert_eq!(argv[n - 2], "--");
    assert_eq!(argv[n - 1], "keep working the goal");
}

#[test]
fn codex_continue_uses_exec_resume_without_sandbox_or_add_dir() {
    // `codex exec resume` does NOT accept -s/--sandbox or --add-dir (those are
    // base-`exec` only), so a resumed codex worker must omit them. <id> is the
    // SESSION_ID positional before the flags; the prompt trails `--`.
    let argv = build_command(
        Engine::Codex,
        "keep working the goal",
        PermissionMode::AcceptEdits,
        &[PathBuf::from("/extra/one")],
        Resume::Continue("sess-abc".into()),
    );
    assert_eq!(
        argv,
        vec![
            "codex",
            "exec",
            "resume",
            "sess-abc",
            "--skip-git-repo-check",
            "--json",
            "--",
            "keep working the goal",
        ]
    );
}

#[test]
fn codex_fresh_ignores_pinned_session_id() {
    // Codex has no caller-chosen id, so Fresh{Some} == Fresh{None} argv (the id
    // codex assigns is captured from its --json stream afterward).
    let with_id = build_command(
        Engine::Codex,
        "boot",
        PermissionMode::AcceptEdits,
        &[],
        Resume::Fresh {
            session_id: Some("ignored".into()),
        },
    );
    let without = build_command(
        Engine::Codex,
        "boot",
        PermissionMode::AcceptEdits,
        &[],
        Resume::Fresh { session_id: None },
    );
    assert_eq!(with_id, without);
}

#[test]
fn loop_command_claude_create_uses_session_id_and_add_dir() {
    // A freshly-minted id CREATES the pinned conversation via interactive
    // `claude --permission-mode auto --session-id <id>` (NO `-p`, NO stream-json).
    let argv = build_loop_command(
        Engine::Claude,
        &Resume::Fresh {
            session_id: Some("11111111-1111-4111-8111-111111111111".into()),
        },
        &[PathBuf::from("/proj/root")],
        None,
        None,
    );
    assert_eq!(
        argv,
        vec![
            "env",
            "-u",
            "CLAUDECODE",
            "claude",
            "--permission-mode",
            "auto",
            "--session-id",
            "11111111-1111-4111-8111-111111111111",
            "--add-dir",
            "/proj/root",
        ]
    );
    // Token-wise, not substring-wise: "-p" is a substring of "--permission-mode".
    assert!(!argv.iter().any(|a| a == "-p"), "{argv:?}");
    let joined = argv.join(" ");
    assert!(!joined.contains("stream-json"), "{joined}");
    assert!(!joined.contains("--resume"), "{joined}");
    // Milestone-E CONTROL: the worker skill is discovered NATIVELY from
    // `<work_dir>/.claude/skills/`, so `build_loop_command` gained NO skills `--add-dir` — the
    // only add-dir is the work_dir above (the exact-argv assert already pins this).
    assert!(!joined.contains(".claude/skills"), "{joined}");
}

#[test]
fn loop_command_claude_resume_uses_resume_not_session_id() {
    // An EXISTING id (relaunch / restart / adopted seed) RESUMES — `--session-id`
    // would error ("session already exists").
    let argv = build_loop_command(
        Engine::Claude,
        &Resume::Continue("11111111-1111-4111-8111-111111111111".into()),
        &[],
        None,
        None,
    );
    assert_eq!(
        argv,
        vec![
            "env",
            "-u",
            "CLAUDECODE",
            "claude",
            "--permission-mode",
            "auto",
            "--resume",
            "11111111-1111-4111-8111-111111111111",
        ]
    );
    assert!(
        !argv.join(" ").contains("--session-id"),
        "a resume must NOT --session-id (create): {argv:?}"
    );
}

#[test]
fn loop_command_claude_permission_mode_comes_from_the_auto_enum_not_a_literal() {
    // Drift guard: the persistent session's posture must be whatever
    // `PermissionMode::Auto` renders to, so changing the enum can never leave a
    // stale duplicated literal behind on the path autopilot actually drives.
    let expected = PermissionMode::Auto.claude_arg();
    for resume in [
        Resume::Fresh {
            session_id: Some("11111111-1111-4111-8111-111111111111".into()),
        },
        Resume::Continue("11111111-1111-4111-8111-111111111111".into()),
        Resume::Fresh { session_id: None },
    ] {
        let argv = build_loop_command(Engine::Claude, &resume, &[], None, None);
        let i = argv
            .iter()
            .position(|a| a == "--permission-mode")
            .unwrap_or_else(|| panic!("no --permission-mode in {argv:?} for {resume:?}"));
        assert_eq!(argv[i + 1], expected, "{argv:?}");
        // Immediately after `claude`, before the anchoring slot.
        assert_eq!(argv[i - 1], "claude", "{argv:?}");
    }
}

#[test]
fn loop_command_codex_gets_the_measured_unattended_posture_not_permission_mode() {
    // Codex's unattended posture is now MEASURED (codex 0.146.1.355), not guessed:
    // `--ask-for-approval never --sandbox workspace-write` is the analogue of
    // claude's `--permission-mode auto` — see the `Engine::Codex` arm for the
    // evidence. `--permission-mode` stays claude-only, so its ABSENCE is still
    // asserted: it is not a flag codex has, and inventing one is the failure this
    // test has always guarded.
    //
    // The exact argv is asserted for every `Resume` shape (add_dirs included, and
    // deliberately NOT forwarded — see the arm) so any future edit is caught.
    let dirs = [PathBuf::from("/proj/root")];
    let base = ["env", "-u", "CLAUDECODE", "codex"];
    let posture = [
        "--ask-for-approval",
        "never",
        "--sandbox",
        "workspace-write",
    ];
    let cases: [(Resume, Vec<&str>); 3] = [
        (
            Resume::Continue("codex-sess-9".into()),
            base.iter()
                .copied()
                .chain(["resume", "codex-sess-9"])
                .chain(posture)
                .collect(),
        ),
        (
            Resume::Fresh { session_id: None },
            base.iter().copied().chain(posture).collect(),
        ),
        (
            Resume::Fresh {
                session_id: Some("ignored".into()),
            },
            base.iter().copied().chain(posture).collect(),
        ),
    ];
    for (resume, expected) in cases {
        let argv = build_loop_command(Engine::Codex, &resume, &dirs, None, None);
        assert_eq!(argv, expected, "{resume:?}");
        assert!(
            !argv.iter().any(|a| a == "--permission-mode"),
            "{argv:?} for {resume:?}"
        );
        assert!(
            !argv.iter().any(|arg| arg.starts_with("ECC_GATEGUARD=")),
            "agent-manager must inherit host policy: {argv:?}"
        );
        // The WRONG, strictly stronger choice must never appear: it runs with no
        // sandbox at all, and it buys nothing here (measured — it does not even
        // skip the directory-trust dialog).
        assert!(
            !argv
                .iter()
                .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"),
            "{argv:?} for {resume:?}"
        );
    }
}

#[test]
fn loop_command_codex_resumes_known_id_or_fresh_interactive() {
    let resumed = build_loop_command(
        Engine::Codex,
        &Resume::Continue("codex-sess-9".into()),
        &[],
        None,
        None,
    );
    assert_eq!(
        resumed,
        vec![
            "env",
            "-u",
            "CLAUDECODE",
            "codex",
            "resume",
            "codex-sess-9",
            "--ask-for-approval",
            "never",
            "--sandbox",
            "workspace-write",
        ]
    );
    // No id yet ⇒ a fresh interactive session (no `resume` positional), same posture.
    let fresh = build_loop_command(
        Engine::Codex,
        &Resume::Fresh { session_id: None },
        &[],
        None,
        None,
    );
    assert_eq!(
        fresh,
        vec![
            "env",
            "-u",
            "CLAUDECODE",
            "codex",
            "--ask-for-approval",
            "never",
            "--sandbox",
            "workspace-write",
        ]
    );
}

#[test]
fn loop_command_claude_inserts_model_when_some() {
    // Present-iff-Some AND position: `--model <m>` lands AFTER `--permission-mode auto`
    // and BEFORE the session-anchoring flags (`--session-id`/`--resume`), so it applies to
    // the launch itself.
    let argv = build_loop_command(
        Engine::Claude,
        &Resume::Continue("11111111-1111-4111-8111-111111111111".into()),
        &[],
        None,
        Some("global.anthropic.claude-opus-5"),
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["--model", "global.anthropic.claude-opus-5"]),
        "{argv:?}"
    );
    let model_at = argv.iter().position(|a| a == "--model").unwrap();
    let auto_at = argv.iter().position(|a| a == "auto").unwrap();
    let resume_at = argv.iter().position(|a| a == "--resume").unwrap();
    assert!(
        auto_at < model_at && model_at < resume_at,
        "--model must sit after `--permission-mode auto` and before the anchoring flag: {argv:?}"
    );
}

#[test]
fn loop_command_claude_omits_model_when_none() {
    let argv = build_loop_command(
        Engine::Claude,
        &Resume::Fresh { session_id: None },
        &[],
        None,
        None,
    );
    assert!(!argv.iter().any(|a| a == "--model"), "{argv:?}");
}

#[test]
fn loop_command_codex_inserts_m_when_some() {
    // Present-iff-Some AND position: `-m <m>` is a GLOBAL option, so it sits right after
    // `codex` and BEFORE the `resume` subcommand.
    let argv = build_loop_command(
        Engine::Codex,
        &Resume::Continue("codex-sess-9".into()),
        &[],
        None,
        Some("openai.gpt-5.6-sol"),
    );
    assert!(
        argv.windows(2).any(|w| w == ["-m", "openai.gpt-5.6-sol"]),
        "{argv:?}"
    );
    let m_at = argv.iter().position(|a| a == "-m").unwrap();
    let codex_at = argv.iter().position(|a| a == "codex").unwrap();
    let resume_at = argv.iter().position(|a| a == "resume").unwrap();
    assert!(
        codex_at < m_at && m_at < resume_at,
        "-m must sit right after `codex` and before the `resume` subcommand: {argv:?}"
    );
}

#[test]
fn loop_command_codex_omits_m_when_none() {
    let argv = build_loop_command(
        Engine::Codex,
        &Resume::Fresh { session_id: None },
        &[],
        None,
        None,
    );
    assert!(!argv.iter().any(|a| a == "-m"), "{argv:?}");
}

#[test]
fn loop_command_omits_model_when_empty_or_whitespace() {
    // A blank per-session model is "no model set": `Some("")`/`Some("  ")` emits NO flag on
    // either engine, exactly as `None` does (never a launch with an empty id).
    for m in ["", "  "] {
        let claude = build_loop_command(
            Engine::Claude,
            &Resume::Continue("11111111-1111-4111-8111-111111111111".into()),
            &[],
            None,
            Some(m),
        );
        assert!(
            !claude.iter().any(|a| a == "--model"),
            "claude m={m:?}: {claude:?}"
        );
        let codex = build_loop_command(
            Engine::Codex,
            &Resume::Continue("codex-sess-9".into()),
            &[],
            None,
            Some(m),
        );
        assert!(!codex.iter().any(|a| a == "-m"), "codex m={m:?}: {codex:?}");
    }
}

#[test]
fn loop_command_claude_injects_the_stop_hook_appending_to_the_turn_signal() {
    // M71: given a turn-signal path, the claude launch carries a `--settings` `Stop` hook
    // whose command APPENDS to that exact file (size = completed-turn count) — the daemon's
    // definitive "went idle" signal. `--settings` overrides only these keys, leaving the
    // user's own config intact.
    let sig = PathBuf::from("/proj/.project-state/sessions/s/.daemon/turn-complete");
    let argv = build_loop_command(
        Engine::Claude,
        &Resume::Continue("cid".into()),
        &[],
        Some(sig.as_path()),
        None,
    );
    let i = argv
        .iter()
        .position(|a| a == "--settings")
        .expect("--settings present");
    let settings: serde_json::Value =
        serde_json::from_str(&argv[i + 1]).expect("--settings value is valid JSON");
    let hook = &settings["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(hook["type"], "command", "{settings}");
    let cmd = hook["command"].as_str().expect("a command string");
    assert!(
        cmd.contains(&*sig.to_string_lossy()),
        "the hook targets the turn-signal path: {cmd}"
    );
    assert!(
        cmd.contains(">>"),
        "it APPENDS (so size = turn count): {cmd}"
    );
    // Opt-in: with no path, the launch is byte-identical to the pre-M71 shape (no hook).
    let off = build_loop_command(
        Engine::Claude,
        &Resume::Continue("cid".into()),
        &[],
        None,
        None,
    );
    assert!(
        !off.iter().any(|a| a == "--settings"),
        "no turn-signal ⇒ no injected hook: {off:?}"
    );
}

#[test]
fn loop_command_codex_injects_the_notify_program_appending_to_the_turn_signal() {
    // M71 codex analogue: a `-c notify=[...]` override (parsed as TOML) whose `sh -c` appends
    // to the same turn-signal on `agent-turn-complete`. `-c` is a GLOBAL option, so it must
    // sit before the `resume` subcommand.
    let sig = PathBuf::from("/proj/.project-state/sessions/s/.daemon/turn-complete");
    let argv = build_loop_command(
        Engine::Codex,
        &Resume::Continue("codex-sess-9".into()),
        &[],
        Some(sig.as_path()),
        None,
    );
    let i = argv.iter().position(|a| a == "-c").expect("-c present");
    let cfg = &argv[i + 1];
    assert!(
        cfg.starts_with("notify="),
        "overrides the notify program: {cfg}"
    );
    assert!(
        cfg.contains(&*sig.to_string_lossy()) && cfg.contains(">>"),
        "the notify shell appends to the turn-signal: {cfg}"
    );
    let resume_at = argv
        .iter()
        .position(|a| a == "resume")
        .expect("resume present");
    assert!(
        i < resume_at,
        "the global `-c` must precede the `resume` subcommand: {argv:?}"
    );
    // Opt-in: no path ⇒ no `-c notify`.
    let off = build_loop_command(
        Engine::Codex,
        &Resume::Fresh { session_id: None },
        &[],
        None,
        None,
    );
    assert!(
        !off.iter().any(|a| a == "-c"),
        "no turn-signal ⇒ no injected notify: {off:?}"
    );
}

#[test]
fn codex_notify_config_toml_escapes_a_quote_in_the_path() {
    // Finding-2 guard: a project root can legally contain `"`; left unescaped it closes the
    // TOML basic string early and codex REJECTS `-c` at startup — bricking the launch. It
    // must be escaped as `\"` (the serde_json claude side gets this for free).
    let sig = PathBuf::from("/pr\"oj/.project-state/sessions/s/.daemon/turn-complete");
    let argv = build_loop_command(
        Engine::Codex,
        &Resume::Fresh { session_id: None },
        &[],
        Some(sig.as_path()),
        None,
    );
    let cfg = argv
        .iter()
        .find(|a| a.starts_with("notify="))
        .expect("notify override present");
    // The `"` from the path appears ONLY in its escaped form `\"` (literal `pr\"oj`).
    assert!(
        cfg.contains("pr\\\"oj"),
        "the path's quote must be TOML-escaped as \\\": {cfg}"
    );
}
