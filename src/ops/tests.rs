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
