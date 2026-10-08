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
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---- op_key: determinism + injective encoding -------------------------

    #[test]
    fn op_key_is_deterministic() {
        let a = op_key("draft-cr", &["auth+api", "slice-1"]);
        let b = op_key("draft-cr", &["auth+api", "slice-1"]);
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn op_key_matches_documented_format() {
        // `+` inside a part is escaped (`%2B`); the `+` between parts and the
        // `:` after the kind are the only literal delimiters.
        assert_eq!(
            op_key("draft-cr", &["auth+api", "slice-1"]),
            "draft-cr:auth%2Bapi+slice-1"
        );
    }

    #[test]
    fn op_key_part_plus_does_not_collide_with_two_parts() {
        // The load-bearing case: `["a+b"]` and `["a","b"]` must never map to the
        // same key (a naive join would collide them).
        assert_ne!(op_key("k", &["a+b"]), op_key("k", &["a", "b"]));
    }

    #[test]
    fn op_key_is_injective_across_reserved_chars() {
        // Every tuple here would collide under naive concatenation with one of
        // the others; encoding `+`, `:`, `%`, and space keeps them all distinct.
        let inputs: &[(&str, &[&str])] = &[
            ("a", &["b", "c"]), // a:b+c
            ("a", &["b+c"]),    // vs a:b%2Bc
            ("a:b", &["c"]),    // a%3Ab:c
            ("a", &["b:c"]),    // vs a:b%3Ac
            ("a", &["b%2Bc"]),  // vs a:b%252Bc (naive: collides with ["b+c"])
            ("a", &["b c"]),    // a:b%20c
            ("a", &["bc"]),     // a:bc
            ("", &["a"]),       // :a
            ("draft-cr", &["auth+api", "slice-1"]),
        ];
        let keys: HashSet<String> = inputs.iter().map(|(k, p)| op_key(k, p)).collect();
        assert_eq!(keys.len(), inputs.len(), "op_key must be injective");
    }

    // ---- begin / find -----------------------------------------------------

    #[test]
    fn begin_fresh_pushes_one_pending() {
        let mut ops = Vec::new();
        assert_eq!(
            begin(&mut ops, "op-1", "push:main", 100),
            BeginOutcome::Fresh
        );
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].id, "op-1");
        assert_eq!(ops[0].key, "push:main");
        assert_eq!(ops[0].status, OpStatus::Pending);
        assert_eq!(ops[0].started_at, 100);
    }

    #[test]
    fn begin_twice_same_key_is_already_pending_without_duplicating() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "push:main", 100);
        assert_eq!(
            begin(&mut ops, "op-2", "push:main", 200),
            BeginOutcome::AlreadyPending
        );
        assert_eq!(ops.len(), 1, "no duplicate record");
        assert_eq!(ops[0].id, "op-1", "original record is preserved");
        assert_eq!(ops[0].started_at, 100);
    }

    #[test]
    fn begin_distinct_keys_each_get_a_record() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "push:main", 100);
        begin(&mut ops, "op-2", "merge:main", 100);
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn find_returns_matching_record_or_none() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "push:main", 100);
        assert_eq!(find(&ops, "push:main").map(|o| o.id.as_str()), Some("op-1"));
        assert!(find(&ops, "absent").is_none());
    }

    // ---- complete / fail: monotonic transitions ---------------------------

    #[test]
    fn complete_flips_pending_and_rebegin_is_already_completed() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "publish:cr", 100);
        assert!(complete(&mut ops, "publish:cr"), "live transition occurred");
        assert_eq!(ops[0].status, OpStatus::Completed);
        assert_eq!(
            begin(&mut ops, "op-2", "publish:cr", 200),
            BeginOutcome::AlreadyCompleted
        );
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn complete_on_already_completed_is_noop_success() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "publish:cr", 100);
        assert!(complete(&mut ops, "publish:cr"));
        // Second complete is a no-op: no live transition, still Completed.
        assert!(!complete(&mut ops, "publish:cr"));
        assert_eq!(ops[0].status, OpStatus::Completed);
    }

    #[test]
    fn fail_flips_pending_and_rebegin_is_retry_after_failed() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "push:main", 100);
        assert!(fail(&mut ops, "push:main"), "live transition occurred");
        assert_eq!(ops[0].status, OpStatus::Failed);
        assert_eq!(
            begin(&mut ops, "op-2", "push:main", 200),
            BeginOutcome::RetryAfterFailed
        );
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn complete_after_fail_completes_the_retry() {
        // The load-bearing retry path: a Failed op that succeeds on retry MUST
        // reach Completed, else begin keeps returning RetryAfterFailed and the
        // external action fires again — the double-action hole.
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "push:main", 100);
        fail(&mut ops, "push:main");
        assert!(
            complete(&mut ops, "push:main"),
            "Failed -> Completed is live"
        );
        assert_eq!(ops[0].status, OpStatus::Completed);
        assert_eq!(
            begin(&mut ops, "op-2", "push:main", 300),
            BeginOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn fail_after_complete_does_not_regress() {
        // Monotonicity: Completed is absorbing.
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "publish:cr", 100);
        complete(&mut ops, "publish:cr");
        assert!(!fail(&mut ops, "publish:cr"), "must not regress Completed");
        assert_eq!(ops[0].status, OpStatus::Completed);
    }

    #[test]
    fn fail_on_already_failed_is_noop() {
        let mut ops = Vec::new();
        begin(&mut ops, "op-1", "push:main", 100);
        assert!(fail(&mut ops, "push:main"));
        assert!(
            !fail(&mut ops, "push:main"),
            "already Failed: no live transition"
        );
        assert_eq!(ops[0].status, OpStatus::Failed);
    }

    #[test]
    fn complete_and_fail_on_missing_key_return_false() {
        let mut ops = Vec::new();
        assert!(!complete(&mut ops, "absent"));
        assert!(!fail(&mut ops, "absent"));
        assert!(ops.is_empty());
    }
}
