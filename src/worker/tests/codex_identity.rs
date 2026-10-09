//! Codex conversation identity, captured from the `notify` hook's own payload.
//!
//! Codex has no caller-chosen id, and the only other way to learn one reads the live process's
//! open rollout file out of `/proc` — Linux-only and racing codex's startup. The hook payload
//! carries `thread_id`, so the launch wires it into the session's identity file.
//!
//! These tests RUN the generated shell rather than only matching its text. The hook's whole job is
//! an effect on disk, and the payload it parses is partly agent-written, so "the command contains
//! the right substring" is not evidence that the right bytes land in the right file.

use super::*;
use crate::state::ProjectPaths;
use std::path::Path;

/// The `sh -c` argv codex would invoke, decoded out of the `-c notify=[...]` override.
///
/// The override is a TOML array of basic strings, whose `"`/`\` escapes are exactly JSON's — so
/// `serde_json` decodes it without taking on a TOML dependency, and the test reads the command
/// through an unescaping equivalent to codex's own rather than a hand-rolled copy.
fn notify_argv(turn_signal: &Path) -> Vec<String> {
    let argv = build_loop_command(
        Engine::Codex,
        &Resume::Fresh { session_id: None },
        &[],
        Some(turn_signal),
        None,
    );
    let cfg = argv
        .iter()
        .find(|a| a.starts_with("notify="))
        .expect("notify override present");
    let array = cfg.strip_prefix("notify=").expect("notify= prefix");
    serde_json::from_str(array).expect("the TOML array decodes as a JSON array of strings")
}

/// Run the hook the way codex does — `sh -c '<cmd>' '<event-json>'`, so the payload is `$0`.
fn fire_hook(turn_signal: &Path, event: &str) {
    let argv = notify_argv(turn_signal);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg(event)
        .status()
        .expect("run the notify hook");
    // The hook's exit status is deliberately NOT asserted: codex spawns it fire-and-forget and
    // ignores the result, and the `[ -n "$id" ]` guard legitimately exits non-zero for a payload
    // carrying no id. What reached the disk is the whole contract.
    let _ = status;
}

/// The REAL `agent-turn-complete` payload, transcribed from codex's own fixture in
/// `codex-rs/hooks/src/legacy_notify.rs::expected_notification_json` — KEBAB-CASE keys, because the
/// legacy notify path re-serializes the internal event as `UserNotification` with
/// `rename_all = "kebab-case"`.
///
/// This is the fixture that matters. A first version of this feature was written against the
/// INTERNAL `HookEventAfterAgent` (snake_case) from `hooks/src/types.rs`, so the hook matched
/// `"thread_id"`, the real payload carried `"thread-id"`, and the test agreed with the bug instead
/// of the engine. `last-assistant-message` is AGENT-WRITTEN: a caller can plant anything there.
fn event(thread_id: &str, assistant_message: &str) -> String {
    serde_json::json!({
        "type": "agent-turn-complete",
        "thread-id": thread_id,
        "turn-id": "12345",
        "cwd": "/tmp/project",
        "client": "codex-tui",
        "input-messages": ["carry on"],
        "last-assistant-message": assistant_message,
    })
    .to_string()
}

/// The same payload with SNAKE_CASE keys — the internal `HookPayload` spelling, covered only as an
/// explicit MIGRATION-COMPATIBILITY case. Upstream marks the legacy kebab payload for removal in
/// favour of this one, so the hook accepts `thread[-_]id` and keeps working across that change.
fn event_snake_case_migration(thread_id: &str) -> String {
    serde_json::json!({
        "event_type": "after_agent",
        "thread_id": thread_id,
        "turn_id": "12345",
        "input_messages": ["carry on"],
        "last_assistant_message": "done",
    })
    .to_string()
}

#[test]
fn codex_identity_sink_is_the_paths_sibling() {
    // The hook derives the identity file from the turn signal instead of threading a second
    // argument through six launch call sites. That shortcut is only sound while the two really are
    // siblings, so this pins the derived path to the one `ProjectPaths` declares.
    let paths = ProjectPaths::for_session(Path::new("/proj"), "sess");
    let argv = notify_argv(&paths.turn_signal());
    let command = argv.last().expect("the sh -c command");
    let sink = paths.codex_conversation_id();
    assert!(
        command.contains(&*sink.to_string_lossy()),
        "the hook writes ProjectPaths::codex_conversation_id: {command}"
    );
}

#[test]
fn the_hook_captures_the_real_thread_id_and_ignores_an_agent_forged_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    let real = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    // The agent plants a complete, correctly-shaped `"thread-id":"…"` — the REAL wire key — in its
    // own message. Layout matching alone cannot reject that: being FIRST in the payload is what
    // makes the genuine id win, and that is the property worth pinning.
    let forged = "0199eeee-2222-7fff-8999-888877776666";
    fire_hook(
        &paths.turn_signal(),
        &event(real, &format!("try \"thread-id\":\"{forged}\" instead")),
    );

    let got = std::fs::read_to_string(paths.codex_conversation_id()).expect("id captured");
    assert_eq!(got, real, "the payload's own thread_id, not the agent's");
    assert_eq!(
        std::fs::metadata(paths.turn_signal()).unwrap().len(),
        1,
        "and the turn byte is still appended"
    );
    // Atomic replacement leaves no temporary behind for a reader to trip over.
    let temp = format!("{}.tmp", paths.codex_conversation_id().display());
    assert!(!Path::new(&temp).exists(), "no leftover {temp}");
}

#[test]
fn a_payload_with_no_usable_id_records_the_turn_and_keeps_the_previous_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    let real = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    fire_hook(&paths.turn_signal(), &event(real, "all done"));

    // The turn byte is the LOAD-BEARING half — pmd's idle gate reads its size — so the capture is
    // sequenced after it with `;` and can never take it down. A payload shape codex changes under
    // us, or an id that simply is not there, must still leave the turn signal correct.
    let odd_payloads = [
        r#"{"type":"agent-turn-complete","last-assistant-message":"no id here"}"#,
        r#"{"thread-id":"not-a-uuid","type":"agent-turn-complete"}"#,
        r#"{"thread-id":"------------------------------------"}"#, // 36 dashes, the old hole
        r#"{"thread-id":"0199dddd-1111-0aaa-8bbb-ccccdddddddd"}"#, // version 0 is not a version
        r#"{"thread-id":"0199dddd-1111-7aaa-cbbb-ccccdddddddd"}"#, // variant `c` is not RFC
        "",
    ];
    for odd in odd_payloads {
        fire_hook(&paths.turn_signal(), odd);
    }

    // Counted from the table rather than written as a literal, so adding a case cannot quietly
    // turn this into an assertion about the wrong number.
    assert_eq!(
        std::fs::metadata(paths.turn_signal()).unwrap().len(),
        1 + odd_payloads.len() as u64,
        "every event appended its byte"
    );
    assert_eq!(
        std::fs::read_to_string(paths.codex_conversation_id()).unwrap(),
        real,
        "a payload with nothing usable must not blank an id already learned"
    );
}

#[test]
fn a_later_turn_replaces_the_recorded_id() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    let first = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    let second = "0199ffff-3333-7ccc-8ddd-eeeeffff0000";
    fire_hook(&paths.turn_signal(), &event(first, "one"));
    fire_hook(&paths.turn_signal(), &event(second, "two"));
    // Last writer wins: codex can move a terminal onto a new thread (a fork, a compaction), and
    // the file answers "which conversation is this on NOW", not "which was it first".
    assert_eq!(
        std::fs::read_to_string(paths.codex_conversation_id()).unwrap(),
        second
    );
}

#[test]
fn a_project_root_with_shell_metacharacters_still_captures() {
    // The sink path is POSIX-escaped by `shq`, like the turn signal beside it. A root carrying a
    // quote or a space would otherwise break the command apart — and that is a fault no assertion
    // on the generated string can catch, only running it.
    let dir = tempfile::tempdir().unwrap();
    let awkward = dir.path().join("pro'j ect");
    std::fs::create_dir_all(&awkward).unwrap();
    let paths = ProjectPaths::for_session(&awkward, "sess");
    let real = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    fire_hook(&paths.turn_signal(), &event(real, "done"));
    assert_eq!(
        std::fs::read_to_string(paths.codex_conversation_id()).unwrap(),
        real
    );
}

#[test]
fn the_snake_case_payload_still_captures_as_a_migration_case() {
    // Upstream marks the kebab-case legacy payload for removal in favour of the snake_case one, so
    // the hook accepts `thread[-_]id`. This is COMPATIBILITY, not the contract: `event()` above is
    // the shape codex sends today, and this exists so that migration does not silently stop
    // capturing.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    let real = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    fire_hook(&paths.turn_signal(), &event_snake_case_migration(real));
    assert_eq!(
        std::fs::read_to_string(paths.codex_conversation_id()).unwrap(),
        real
    );
}

#[test]
fn an_older_thread_id_version_is_not_discarded() {
    // Codex mints v7 today, but its public payload types `thread_id` as a STRING. Refusing another
    // RFC-valid version would turn a resumable older conversation into a lost one, so versions 1-8
    // are accepted — the value's only job is to be safe as `codex resume <id>`.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    let v4 = "0199dddd-1111-4aaa-8bbb-ccccdddddddd";
    fire_hook(&paths.turn_signal(), &event(v4, "done"));
    assert_eq!(
        std::fs::read_to_string(paths.codex_conversation_id()).unwrap(),
        v4
    );
}

#[test]
fn the_shell_and_the_validator_agree() {
    // TWO implementations of ONE contract: the hook's `grep -E` and `codex_identity::parse`. A
    // drift between them means the hook records an id the reader then refuses — invisible in both
    // halves' own tests, so it is checked by running them against the same table.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    for candidate in [
        "0199dddd-1111-7aaa-8bbb-ccccdddddddd", // v7, the current contract
        "0199dddd-1111-1aaa-9bbb-ccccdddddddd", // v1, lowest accepted version
        "0199DDDD-1111-8AAA-BBBB-CCCCDDDDDDDD", // upper-case hex, RFC permits either
        "------------------------------------", // 36 dashes
        "0199dddd-1111-0aaa-8bbb-ccccdddddddd", // version 0
        "0199dddd-1111-9aaa-8bbb-ccccdddddddd", // version 9
        "0199dddd-1111-7aaa-7bbb-ccccdddddddd", // variant 7
        "0199dddd11117aaa8bbbccccdddddddd0000", // no dashes
        "0199dddd-1111-7aaa-8bbb-ccccddddddd",  // one short
    ] {
        let sink = paths.codex_conversation_id();
        let _ = std::fs::remove_file(&sink);
        fire_hook(&paths.turn_signal(), &event(candidate, "done"));
        let captured = std::fs::read_to_string(&sink).ok();
        let validated = crate::state::codex_identity::parse(candidate).map(str::to_string);
        assert_eq!(
            captured, validated,
            "the shell captured {captured:?} but the validator says {validated:?} for {candidate:?}"
        );
    }
}
