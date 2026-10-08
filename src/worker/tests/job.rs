//! The argv of a spawned JOB child: the one-shot run that reports a result and exits.
//!
//! What is asserted here is the SHAPE the two CLIs require — the flags that make a run unattended, the
//! schema that makes its final payload readable, and the prompt's place as the last element — plus the
//! one thing a job must NOT carry: the managed-session environment.

use std::path::PathBuf;

use crate::registry::Engine;
use crate::tmux::{ENV_BIN, ENV_SESSION, ENV_STATE_DIR};
use crate::worker::build_job_command;

const SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";
const PROMPT: &str = "do the task\n";

fn argv(engine: Engine, model: Option<&str>) -> Vec<String> {
    build_job_command(
        engine,
        PROMPT,
        model,
        SESSION_ID,
        &PathBuf::from("/s/job-schema.json"),
        &PathBuf::from("/s/job.last"),
    )
}

/// The flag pair `name value`, when `name` is in the argv.
fn flag(argv: &[String], name: &str) -> Option<String> {
    let at = argv.iter().position(|arg| arg == name)?;
    argv.get(at + 1).cloned()
}

/// A JOB MUST NOT INHERIT A MANAGED SESSION. `spawn_step` passes no `-e` pairs, so the pane inherits
/// the tmux server's environment — the DASHBOARD's. Unsetting the three variables in the argv is what
/// makes `pmtui spawn` inside a job unable to name a calling session, for both engines.
#[test]
fn a_job_runs_without_the_managed_session_env() {
    for engine in [Engine::Claude, Engine::Codex] {
        let argv = argv(engine, None);
        assert_eq!(argv[0], "env", "{argv:?}");
        let bin = argv
            .iter()
            .position(|arg| arg == engine.bin())
            .expect("the engine's executable");
        for name in [ENV_SESSION, ENV_STATE_DIR, ENV_BIN] {
            let at = argv
                .iter()
                .position(|arg| arg == name)
                .unwrap_or_else(|| panic!("{name} is never unset: {argv:?}"));
            assert_eq!(
                argv[at - 1],
                "-u",
                "{name} must be unset, not set: {argv:?}"
            );
            // Before the executable, or `env` would pass it to the command instead.
            assert!(at < bin, "{name} is unset after {}: {argv:?}", engine.bin());
        }
    }
}

/// One `env`, not two: claude's own `-u CLAUDECODE` prefix is reused rather than nested.
#[test]
fn the_claude_job_keeps_a_single_env_prefix() {
    let argv = argv(Engine::Claude, None);
    assert_eq!(
        argv.iter().filter(|arg| *arg == "env").count(),
        1,
        "{argv:?}"
    );
    assert!(
        argv.contains(&"CLAUDECODE".to_string()),
        "the nested-session unset is still there: {argv:?}"
    );
}

/// The prompt stays the LAST element, after `--`: everything past the separator is the prompt, which is
/// what lets a Message start with a dash.
#[test]
fn the_prompt_is_the_last_element_after_the_separator() {
    for engine in [Engine::Claude, Engine::Codex] {
        let argv = argv(engine, None);
        assert_eq!(argv.last().map(String::as_str), Some(PROMPT), "{argv:?}");
        assert_eq!(
            argv[argv.len() - 2],
            "--",
            "the prompt must follow the separator: {argv:?}"
        );
    }
}

/// claude's unattended shape: a denied permission prompt rather than a wedged one, our schema for the
/// final payload, and a pinned conversation id a human can resume afterwards.
#[test]
fn the_claude_job_is_unattended_schema_bound_and_resumable() {
    let argv = argv(Engine::Claude, None);
    // AUTO, not acceptEdits: a job must be able to RUN things. Under `acceptEdits` every Bash call was
    // denied, so a child asked to build or test reported failure having run nothing. This is the same
    // posture the persistent autopilot agent already has in the same folder.
    assert_eq!(flag(&argv, "--permission-mode").as_deref(), Some("auto"));
    assert_eq!(flag(&argv, "--permission-prompts").as_deref(), Some("none"));
    // THE DOCUMENT, NOT THE PATH: claude parses this argument as JSON, so a path makes the run die
    // before the prompt is read. (codex is the opposite; see the codex test.)
    let schema = flag(&argv, "--json-schema").expect("a schema");
    assert_eq!(schema, crate::spawn::result_schema_json());
    serde_json::from_str::<serde_json::Value>(&schema).expect("claude parses it as JSON");
    assert!(
        !schema.contains("job-schema.json"),
        "the path form is what a real claude rejected: {schema}"
    );
    assert_eq!(flag(&argv, "--session-id").as_deref(), Some(SESSION_ID));
    assert_eq!(
        flag(&argv, "--output-format").as_deref(),
        Some("stream-json")
    );
    assert!(
        !argv.iter().any(|arg| arg == "--bare"),
        "a job needs the project's own CLAUDE.md and skills: {argv:?}"
    );
}

/// codex's shape: the same schema, plus the `-o` file its final message is written to. It has no
/// caller-chosen conversation id, so none is passed.
#[test]
fn the_codex_job_writes_its_last_message_to_a_file() {
    let argv = argv(Engine::Codex, None);
    // THE PATH, NOT THE DOCUMENT: `codex exec --output-schema <FILE>`.
    assert_eq!(
        flag(&argv, "--output-schema").as_deref(),
        Some("/s/job-schema.json")
    );
    assert_eq!(flag(&argv, "-o").as_deref(), Some("/s/job.last"));
    assert!(
        !argv.iter().any(|arg| arg == "--session-id"),
        "codex has no caller-chosen id: {argv:?}"
    );
    assert_eq!(flag(&argv, "-s").as_deref(), Some("workspace-write"));
}

/// A chosen model reaches each CLI under its own flag, and nothing is passed when the session has none.
#[test]
fn a_chosen_model_is_passed_per_engine() {
    assert_eq!(
        flag(&argv(Engine::Claude, Some("opus")), "--model").as_deref(),
        Some("opus")
    );
    assert_eq!(
        flag(&argv(Engine::Codex, Some("gpt-5")), "-m").as_deref(),
        Some("gpt-5")
    );
    assert!(
        !argv(Engine::Claude, None)
            .iter()
            .any(|arg| arg == "--model")
    );
    assert!(!argv(Engine::Codex, None).iter().any(|arg| arg == "-m"));
}
