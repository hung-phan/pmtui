//! Bringing a child's work into the human's checkout: what applies, what is refused, and what a
//! conflict leaves behind.

use super::lifecycle::{commit, git, repo};
use crate::worktree::{Integration, collect, create, integrate};

/// THE ORDINARY CASE: a child committed in its own worktree, and one keystroke puts that commit on the
/// checkout the human is using. Serial by construction — one commit, one call.
#[test]
fn a_childs_commit_applies_to_the_parent_checkout() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    let sha = commit(&wt.path, "the child's work", &[("src/new.rs", "// new\n")]);
    let before = git(&root, &["rev-parse", "HEAD"]);

    let applied = integrate(&root, &wt.base, &sha);

    let Integration::Applied(head) = applied else {
        panic!("expected it to apply, got {applied:?}");
    };
    assert_ne!(head, before, "the checkout moved");
    assert_eq!(
        std::fs::read_to_string(root.join("src/new.rs")).expect("the child's file is here now"),
        "// new\n"
    );
    assert_eq!(
        git(&root, &["log", "-1", "--pretty=%s"]),
        "the child's work",
        "and it is the child's commit, not a merge of it"
    );
}

/// EVERY COMMIT ON THE BRANCH, NOT JUST THE TIP.
///
/// A child commits as often as it likes — "add the function", then "add its test" — and the receipt
/// lists the files its WHOLE branch touched. Picking only `HEAD` applied the second commit, left the
/// first behind, and the dashboard still reported the full file count as applied: work vanished and the
/// human was told it had landed. This is the test that failed to exist while that shipped, so it asserts
/// on BOTH files and on both commits being present as themselves.
#[test]
fn every_commit_on_a_childs_branch_applies_not_only_the_tip() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    commit(&wt.path, "add the function", &[("src/f.rs", "// f\n")]);
    let tip = commit(&wt.path, "add its test", &[("src/f_test.rs", "// test\n")]);

    // What the dashboard reads and reports, so the assertion below is about the same two numbers the
    // human sees: the tip it names, and the file count it claims landed.
    let work = collect(&wt);
    assert_eq!(work.commit.as_deref(), Some(tip.as_str()));
    assert_eq!(work.touched.len(), 2, "the branch changed two files");

    let applied = integrate(&root, &wt.base, &tip);

    assert!(
        matches!(applied, Integration::Applied(_)),
        "expected it to apply, got {applied:?}"
    );
    assert!(
        root.join("src/f.rs").exists(),
        "the FIRST commit's file must be here too — this is the one that used to be dropped"
    );
    assert!(root.join("src/f_test.rs").exists(), "and the tip's");
    assert_eq!(
        git(&root, &["log", "-2", "--pretty=%s"]),
        "add its test\nadd the function",
        "both arrived as their own commits, in order"
    );
}

/// TWO CHILDREN, ONE AT A TIME. Integrating is serial: the second pick lands on top of the first, so two
/// children that touched different files both arrive without anyone resolving anything.
#[test]
fn two_childrens_commits_apply_one_after_the_other() {
    let (_tmp, root) = repo();
    let one = create(&root, &root.join(".project-state/wt/one"), "pm/one").expect("one");
    let two = create(&root, &root.join(".project-state/wt/two"), "pm/two").expect("two");
    let first = commit(&one.path, "one", &[("src/one.rs", "// one\n")]);
    let second = commit(&two.path, "two", &[("src/two.rs", "// two\n")]);

    assert!(matches!(
        integrate(&root, &one.base, &first),
        Integration::Applied(_)
    ));
    assert!(matches!(
        integrate(&root, &two.base, &second),
        Integration::Applied(_)
    ));

    assert!(root.join("src/one.rs").exists() && root.join("src/two.rs").exists());
    assert_eq!(
        git(&root, &["log", "-2", "--pretty=%s"]),
        "two\none",
        "newest first, each as its own commit"
    );
}

/// A CONFLICT LEAVES THE CHECKOUT EXACTLY AS IT WAS. A half-applied cherry-pick in someone else's working
/// tree is the worst outcome available here, so the pick is aborted and the conflict is reported for a
/// person to settle.
#[test]
fn a_conflict_is_reported_and_the_checkout_is_left_untouched() {
    let (_tmp, root) = repo();
    let one = create(&root, &root.join(".project-state/wt/one"), "pm/one").expect("one");
    let two = create(&root, &root.join(".project-state/wt/two"), "pm/two").expect("two");
    let first = commit(&one.path, "one's line", &[("src/same.rs", "// one\n")]);
    let second = commit(&two.path, "two's line", &[("src/same.rs", "// two\n")]);

    assert!(matches!(
        integrate(&root, &one.base, &first),
        Integration::Applied(_)
    ));
    let head_after_first = git(&root, &["rev-parse", "HEAD"]);

    let conflicted = integrate(&root, &two.base, &second);

    let Integration::Conflicted(why) = conflicted else {
        panic!("expected a conflict, got {conflicted:?}");
    };
    assert!(!why.is_empty(), "the conflict says something");
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        head_after_first,
        "HEAD did not move"
    );
    assert_eq!(
        git(&root, &["status", "--porcelain", "--untracked-files=no"]),
        "",
        "and nothing is left half-applied for the human to clean up"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/same.rs")).expect("the file"),
        "// one\n",
        "the first child's version still stands"
    );
}

/// A CONFLICT PART-WAY THROUGH A RANGE UNDOES THE WHOLE RANGE.
///
/// `--abort` is what makes this true, and it is why the range is safe to apply as one action: the
/// child's first commit applies cleanly and its second conflicts, so an abort that kept the first would
/// leave the human's checkout somewhere neither they nor the child chose.
#[test]
fn a_conflict_mid_range_leaves_none_of_the_range_applied() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    commit(&wt.path, "a clean one", &[("src/clean.rs", "// kid\n")]);
    let tip = commit(&wt.path, "and a clashing one", &[("README.md", "kid\n")]);
    // The human changed the same file on their own checkout, and committed it — so the range's SECOND
    // commit cannot apply while its first still can.
    let before = commit(&root, "the human's own line", &[("README.md", "human\n")]);

    let conflicted = integrate(&root, &wt.base, &tip);

    let Integration::Conflicted(why) = conflicted else {
        panic!("expected a conflict, got {conflicted:?}");
    };
    assert!(!why.is_empty(), "the conflict says something");
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        before,
        "HEAD is back where the human left it"
    );
    assert!(
        !root.join("src/clean.rs").exists(),
        "the range's first commit was rolled back with the rest — nothing is half-applied"
    );
    assert_eq!(
        git(&root, &["status", "--porcelain", "--untracked-files=no"]),
        "",
        "and no conflict markers are left for them to clean up"
    );
}

/// REFUSED BEFORE TOUCHING ANYTHING: a dirty checkout, a directory that is not a repository, and a range
/// with nothing in it. None of these is a conflict — nothing was attempted.
#[test]
fn integration_refuses_a_dirty_checkout_a_non_repo_and_an_empty_range() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    let sha = commit(&wt.path, "work", &[("src/new.rs", "// new\n")]);

    assert!(matches!(
        integrate(&root, &wt.base, ""),
        Integration::Refused(_)
    ));
    assert!(matches!(
        integrate(&root, "", &sha),
        Integration::Refused(_)
    ));
    assert!(
        matches!(
            integrate(&root, &wt.base, &wt.base),
            Integration::Refused(_)
        ),
        "a child that committed nothing leaves base == head, which is no range at all"
    );

    let bare = tempfile::tempdir().expect("tempdir");
    let Integration::Refused(why) = integrate(bare.path(), &wt.base, &sha) else {
        panic!("a non-repository must be refused");
    };
    assert!(why.contains("not a git repository"), "{why}");

    std::fs::write(root.join("README.md"), "the human was editing this\n").expect("write");
    let Integration::Refused(why) = integrate(&root, &wt.base, &sha) else {
        panic!("a dirty checkout must be refused");
    };
    assert!(why.contains("uncommitted changes"), "{why}");
    assert_eq!(
        std::fs::read_to_string(root.join("README.md")).expect("the human's file"),
        "the human was editing this\n",
        "their edit is untouched"
    );
    assert!(
        !root.join("src/new.rs").exists(),
        "and nothing was applied behind it"
    );
}
