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

/// The UUID shape BOTH the capture hook's `grep -E` and [`parse`] accept — one contract, written
/// once, because they are two implementations of the same rule and any drift means the hook records
/// something the reader then refuses.
///
/// RFC 4122 layout, with the version nibble constrained to `1-8` and the variant nibble to
/// `8|9|a|b`. Codex mints UUIDv7 today (`ThreadId` wraps `Uuid::now_v7()`), but its public payload
/// types `thread_id` as a plain STRING, so pinning `7` would refuse an otherwise valid thread from
/// an older build — turning a resumable session into a lost one. Layout plus variant is everything
/// the only job this value has needs: being safe to pass as `codex resume <id>`. Both hex cases are
/// accepted because RFC 4122 permits either.
pub const UUID_ERE: &str =
    "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}";

/// Byte offsets of the four dashes, the version nibble and the variant nibble, and the total
/// length — [`parse`]'s transcription of [`UUID_ERE`].
const DASHES: [usize; 4] = [8, 13, 18, 23];
const VERSION: usize = 14;
const VARIANT: usize = 19;
const LEN: usize = 36;

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

/// Accept a recorded id only while it still is one: [`UUID_ERE`], checked position by position.
///
/// The hook only ever writes a matching id, so anything else means the file was truncated or
/// tampered with — and the cost of believing it is `codex resume <garbage>`, a launch that dies and
/// takes a human's `Enter` (or a daemon's relaunch) with it.
///
/// An earlier version of this accepted any 36 characters drawn from hex digits and dashes, which
/// admitted 36 dashes and a 36-character hex blob. "Looks vaguely like a uuid" is not a contract;
/// the LAYOUT is, so the dashes, the version nibble and the variant nibble are each checked where
/// they belong. Hand-written rather than pulled from a uuid crate: this needs to validate a shape,
/// not parse a value, and the shell half of the same contract has no crate to reach for either —
/// what it does have is [`UUID_ERE`], and `the_shell_and_the_validator_agree` holds them together.
///
/// Trailing whitespace is tolerated. The hook writes the id bare, but an editor or a `cat >` would
/// leave a newline, and refusing a conversation over that would be pedantry.
pub fn parse(raw: &str) -> Option<&str> {
    let id = raw.trim();
    // ASCII first: every byte offset below is a character position only while that holds, and a
    // multi-byte character would otherwise be measured in the wrong units.
    if id.len() != LEN || !id.is_ascii() {
        return None;
    }
    for (i, c) in id.bytes().enumerate() {
        let ok = match i {
            _ if DASHES.contains(&i) => c == b'-',
            VERSION => c.is_ascii_digit() && (b'1'..=b'8').contains(&c),
            VARIANT => matches!(c, b'8' | b'9' | b'a' | b'b' | b'A' | b'B'),
            _ => c.is_ascii_hexdigit(),
        };
        if !ok {
            return None;
        }
    }
    Some(id)
}
