//! The conversation id codex reported for itself.
//!
//! Codex has no caller-chosen id: there is no equivalent of `claude --session-id <uuid>`, so a
//! launcher cannot name the conversation it is about to start and resume that name later. What it
//! can do is listen. Codex's `notify` hook fires on `agent-turn-complete` with a payload carrying
//! `thread_id`, and [`crate::worker`] wires that hook to write the id beside the turn signal.
//!
//! ONE RULE, TWO READERS, like [`super::turn_signal`]: pmd adopts the id into its ledger so a
//! relaunch RESUMES instead of starting over, and pmtui reads it so `Enter` resumes instead of
//! opening a second conversation. Both were blind before — a codex session that had run for hours
//! still looked never-woken to each of them.

use std::path::Path;

/// The id recorded for the session at `paths`, or `None` when the hook has not written one yet (no
/// turn has completed) or what it wrote is no longer usable.
///
/// Read-only and panic-free: a missing file, an unreadable one and a permission error all mean the
/// same thing — nothing to adopt — and none is a reason to fail a tick or an attach.
pub fn read(paths: &super::ProjectPaths) -> Option<String> {
    read_at(&paths.codex_conversation_id())
}

/// [`read`] against an explicit path, for a caller that already holds one.
pub fn read_at(path: &Path) -> Option<String> {
    parse(&std::fs::read_to_string(path).ok()?).map(str::to_string)
}

/// Accept a recorded id only while it still looks like one.
///
/// The hook only ever writes a shape-matched UUIDv7, so anything else means the file was truncated
/// or tampered with — and the cost of believing it is `codex resume <garbage>`, a launch that dies
/// and takes a human's `Enter` (or a daemon's relaunch) with it. The check is deliberately
/// structural rather than a full UUID parse: 36 characters of hex digits and dashes is all a caller
/// needs to know before putting the value on a command line, and it keeps this crate free of a uuid
/// dependency it has no other use for.
///
/// Trailing whitespace is tolerated. The hook writes the id bare, but an editor or a `cat >` would
/// leave a newline, and refusing a conversation over that would be pedantry.
pub fn parse(raw: &str) -> Option<&str> {
    let id = raw.trim();
    let shaped = id.len() == 36 && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    shaped.then_some(id)
}
