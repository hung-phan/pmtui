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
