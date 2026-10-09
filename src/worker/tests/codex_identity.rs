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

/// A realistic `agent-turn-complete` payload. `last_assistant_message` is AGENT-WRITTEN, so a
/// caller can plant anything there.
fn event(thread_id: &str, assistant_message: &str) -> String {
    serde_json::json!({
        "session_id": "0199c3a4-7b21-7def-8c45-1f2e3d4a5b6c",
        "cwd": "/tmp/project",
        "triggered_at": "2026-10-09T04:00:00Z",
        "event_type": "after_agent",
        "thread_id": thread_id,
        "turn_id": "turn-1",
        "input_messages": ["carry on"],
        "last_assistant_message": assistant_message,
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
    // The agent plants a complete, correctly-shaped `"thread_id":"…"` in its own message. Shape
    // matching alone cannot reject that — being FIRST in the payload is what makes the real one
    // win, and that is the property worth pinning.
    let forged = "0199eeee-2222-7fff-8999-888877776666";
    fire_hook(
        &paths.turn_signal(),
        &event(real, &format!("try \"thread_id\":\"{forged}\" instead")),
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
    for odd in [
        r#"{"event_type":"after_agent","last_assistant_message":"no id here"}"#,
        r#"{"thread_id":"not-a-uuid","event_type":"after_agent"}"#,
        r#"{"thread_id":"0199dddd-1111-4aaa-8bbb-ccccdddddddd"}"#, // UUIDv4: wrong version nibble
        "",
    ] {
        fire_hook(&paths.turn_signal(), odd);
    }

    assert_eq!(
        std::fs::metadata(paths.turn_signal()).unwrap().len(),
        5,
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
