//! Reading back the conversation id codex reported for itself.
//!
//! The value goes straight onto a command line (`codex resume <id>`), and pmd adopts it into the
//! ledger it alone writes, so what this accepts matters more than its size suggests.

use crate::state::{ProjectPaths, codex_identity};

const GOOD: &str = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";

#[test]
fn an_id_is_accepted_only_while_it_still_looks_like_one() {
    assert_eq!(codex_identity::parse(GOOD), Some(GOOD));
    // Trailing whitespace is tolerated: the hook writes the id bare, but an editor or a `cat >`
    // would leave a newline, and refusing a conversation over that would be pedantry.
    assert_eq!(codex_identity::parse(&format!("{GOOD}\n  ")), Some(GOOD));

    // Anything else means a truncated or tampered file, and believing it costs
    // `codex resume <garbage>` — a launch that dies and takes a human's Enter with it.
    for bad in [
        "",
        "   ",
        "not-a-conversation-id",
        "0199dddd-1111-7aaa-8bbb",                // truncated
        "0199dddd-1111-7aaa-8bbb-ccccddddddddXX", // too long
        "0199dddd-1111-7aaa-8bbb-ccccdddddddz",   // non-hex
        "../../../etc/passwd",
        "$(touch /tmp/pwned)",
        "--help",
    ] {
        assert_eq!(codex_identity::parse(bad), None, "must reject {bad:?}");
    }
}

#[test]
fn reading_a_session_with_no_recorded_id_is_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    // No file at all is the ordinary state of a session that has not finished a turn yet — the
    // hook writes only on `agent-turn-complete`. It must read as "nothing to adopt", never as an
    // error that could fail a tick or an attach.
    assert_eq!(codex_identity::read(&paths), None);

    // A garbage or unreadable file reads the same way, for the same reason.
    std::fs::create_dir_all(paths.codex_conversation_id().parent().unwrap()).unwrap();
    std::fs::write(paths.codex_conversation_id(), b"\xff\xfe not an id").unwrap();
    assert_eq!(codex_identity::read(&paths), None);
}

#[test]
fn a_recorded_id_is_read_from_the_path_the_hook_writes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "sess");
    std::fs::create_dir_all(paths.codex_conversation_id().parent().unwrap()).unwrap();
    std::fs::write(paths.codex_conversation_id(), GOOD).unwrap();
    assert_eq!(codex_identity::read(&paths), Some(GOOD.to_string()));
    // The identity file is a SIBLING of the turn signal — the launch hook derives one path from
    // the other, so a change to either must keep them in the same directory.
    assert_eq!(
        paths.codex_conversation_id().parent(),
        paths.turn_signal().parent()
    );
}
