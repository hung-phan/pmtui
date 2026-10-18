//! The idempotency journal (spec §9): external mutations — branch, push,
//! draft-CR, publish, merge — must happen *at most once*, even across a crash.
//!
//! The anchor is a deterministic, INJECTIVE [`op_key`]: the caller derives the
//! same key for the same intent every time, and two *distinct* intents can
//! never collide onto one key (a collision would let two different external
//! actions dedupe to a single record — a correctness hole). Before the network
//! call the caller records a `Pending` [`Operation`] via [`begin`]; a crash
//! mid-flight leaves that `Pending` record behind, so a retry [`find`]s it and
//! reconciles against its status instead of re-firing. On return the caller
//! [`complete`]s or [`fail`]s the record. Transitions are monotonic: `Completed`
//! is absorbing (never regresses), while `Failed` is retryable and a successful
//! retry completes it.
//!
//! Pure logic over the already-shipped `pmstate::{Operation, OpStatus}`: no I/O,
//! no time source beyond the `now` the caller passes in.

use crate::clock::Epoch;
use crate::pmstate::{OpStatus, Operation};

/// What a [`begin`] found for the key. The caller performs the external action
/// only on [`Fresh`](BeginOutcome::Fresh) or
/// [`RetryAfterFailed`](BeginOutcome::RetryAfterFailed); the other two mean the
/// action already ran (or is presumed to have run) and must not be repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginOutcome {
    /// No prior record — a `Pending` one was just pushed; run the action.
    Fresh,
    /// A `Pending` record already exists (an earlier attempt is unresolved,
    /// e.g. a crash between `begin` and the network reply). Do NOT re-fire.
    AlreadyPending,
    /// A `Completed` record already exists — the action succeeded before.
    AlreadyCompleted,
    /// A `Failed` record exists — the earlier attempt failed; retry the action.
    RetryAfterFailed,
}

/// Percent-encode the four bytes that would otherwise let two distinct inputs
/// collide once concatenated into a key: `%` (the escape byte itself), `:` (the
/// kind/parts delimiter), `+` (the parts delimiter), and space. Each is
/// replaced by `%` followed by its two upper-hex ASCII code. All four are
/// single-byte ASCII, so they never occur inside a multi-byte UTF-8 char — every
/// other char (including non-ASCII) passes through verbatim.
///
/// Escaping `%` itself is what makes this a *reversible* (hence injective) map:
/// a `%` in the output can only begin an escape, so the original string is
/// uniquely recoverable. We never actually decode — equality of keys is all the
/// journal needs — but reversibility is the proof that no two distinct strings
/// share an encoding.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            ':' => out.push_str("%3A"),
            '+' => out.push_str("%2B"),
            ' ' => out.push_str("%20"),
            other => out.push(other),
        }
    }
    out
}

/// Build the journal key for an external action. `kind` names the action
/// (`"draft-cr"`, `"push"`, …) and `parts` are the identifying operands
/// (slice id, branch, …); by contract at least one part is supplied.
///
/// Scheme: `encode(kind) + ":" + parts.map(encode).join("+")`. Because
/// [`encode`] escapes `:` and `+`, the sole unescaped `:` always separates the
/// kind from the parts blob and the unescaped `+`s always separate the parts —
/// so both the kind and the exact list of parts are uniquely recoverable, and
/// the map from `(kind, parts)` to key is injective. Distinct external intents
/// therefore never dedupe onto one journal record.
///
/// Examples: `op_key("draft-cr", &["auth+api", "slice-1"])` →
/// `"draft-cr:auth%2Bapi+slice-1"`; `op_key("k", &["a+b"])` (→ `"k:a%2Bb"`) and
/// `op_key("k", &["a", "b"])` (→ `"k:a+b"`) stay distinct.
pub fn op_key(kind: &str, parts: &[&str]) -> String {
    // Enforce the ≥1-part contract: with no parts, `op_key(k, &[])` and
    // `op_key(k, &[""])` both collapse to `"k:"` — the sole way to break
    // injectivity. A caller that passes no parts has no identifying operand for
    // the action, which is a bug; catch it in debug builds (zero release cost).
    debug_assert!(
        !parts.is_empty(),
        "op_key requires at least one identifying part for kind {kind:?}"
    );
    let joined = parts
        .iter()
        .copied()
        .map(encode)
        .collect::<Vec<_>>()
        .join("+");
    format!("{}:{}", encode(kind), joined)
}

/// The existing record for `key`, if any (the reconcile-before-retry lookup).
pub fn find<'a>(ops: &'a [Operation], key: &str) -> Option<&'a Operation> {
    ops.iter().find(|op| op.key == key)
}

/// Reserve the operation for `key`, or report the existing record's disposition
/// without duplicating it. With no prior record a fresh `Pending` [`Operation`]
/// is pushed and [`Fresh`](BeginOutcome::Fresh) returned (the caller then runs
/// the external action); an existing record maps to its status
/// (`Pending`→`AlreadyPending`, `Completed`→`AlreadyCompleted`,
/// `Failed`→`RetryAfterFailed`) and is left untouched.
pub fn begin(ops: &mut Vec<Operation>, id: &str, key: &str, now: Epoch) -> BeginOutcome {
    if let Some(existing) = find(ops, key) {
        return match existing.status {
            OpStatus::Pending => BeginOutcome::AlreadyPending,
            OpStatus::Completed => BeginOutcome::AlreadyCompleted,
            OpStatus::Failed => BeginOutcome::RetryAfterFailed,
        };
    }
    ops.push(Operation {
        id: id.to_string(),
        key: key.to_string(),
        status: OpStatus::Pending,
        started_at: now,
    });
    BeginOutcome::Fresh
}

/// Drive the record for `key` to `Completed`, returning whether a live
/// transition occurred. `Completed` is absorbing: a `Pending` or (retried)
/// `Failed` record flips to `Completed` (→ `true`), while a record already
/// `Completed` is a no-op success (→ `false`). A missing key is `false`.
// `&mut Vec` (not `&mut [_]`) to keep the journal mutators uniform with `begin`,
// which must own a `Vec` to push; callers pass `&mut state.operations` either way.
#[allow(clippy::ptr_arg)]
pub fn complete(ops: &mut Vec<Operation>, key: &str) -> bool {
    match ops.iter_mut().find(|op| op.key == key) {
        Some(op) if op.status != OpStatus::Completed => {
            op.status = OpStatus::Completed;
            true
        }
        _ => false,
    }
}

/// Drive the record for `key` to `Failed`, returning whether a live transition
/// occurred. Only a `Pending` record flips to `Failed` (→ `true`); a `Completed`
/// record must NOT regress (left `Completed`, → `false`) and an already-`Failed`
/// record is a no-op (→ `false`). A missing key is `false`.
// `&mut Vec` (not `&mut [_]`) to keep the journal mutators uniform with `begin`,
// which must own a `Vec` to push; callers pass `&mut state.operations` either way.
#[allow(clippy::ptr_arg)]
pub fn fail(ops: &mut Vec<Operation>, key: &str) -> bool {
    match ops.iter_mut().find(|op| op.key == key) {
        Some(op) if op.status == OpStatus::Pending => {
            op.status = OpStatus::Failed;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
