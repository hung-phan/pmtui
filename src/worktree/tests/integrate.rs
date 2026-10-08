//! Bringing a child's commit into the human's checkout: what applies, what is refused, and what a
//! conflict leaves behind.

use super::lifecycle::{commit, git, repo};
use crate::worktree::{Integration, create, integrate};

/// THE ORDINARY CASE: a child committed in its own worktree, and one keystroke puts that commit on the
/// checkout the human is using. Serial by construction — one commit, one call.
#[test]
fn a_childs_commit_applies_to_the_parent_checkout() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    let sha = commit(&wt.path, "the child's work", &[("src/new.rs", "// new\n")]);
    let before = git(&root, &["rev-parse", "HEAD"]);

    let applied = integrate(&root, &sha);

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

/// TWO CHILDREN, ONE AT A TIME. Integrating is serial: the second pick lands on top of the first, so two
/// children that touched different files both arrive without anyone resolving anything.
#[test]
fn two_childrens_commits_apply_one_after_the_other() {
    let (_tmp, root) = repo();
    let one = create(&root, &root.join(".project-state/wt/one"), "pm/one").expect("one");
    let two = create(&root, &root.join(".project-state/wt/two"), "pm/two").expect("two");
    let first = commit(&one.path, "one", &[("src/one.rs", "// one\n")]);
    let second = commit(&two.path, "two", &[("src/two.rs", "// two\n")]);

    assert!(matches!(integrate(&root, &first), Integration::Applied(_)));
    assert!(matches!(integrate(&root, &second), Integration::Applied(_)));

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

    assert!(matches!(integrate(&root, &first), Integration::Applied(_)));
    let head_after_first = git(&root, &["rev-parse", "HEAD"]);

    let conflicted = integrate(&root, &second);

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

/// REFUSED BEFORE TOUCHING ANYTHING: a dirty checkout, a directory that is not a repository, and a commit
/// that was never named. None of these is a conflict — nothing was attempted.
#[test]
fn integration_refuses_a_dirty_checkout_a_non_repo_and_an_empty_commit() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    let sha = commit(&wt.path, "work", &[("src/new.rs", "// new\n")]);

    assert!(matches!(integrate(&root, ""), Integration::Refused(_)));

    let bare = tempfile::tempdir().expect("tempdir");
    let Integration::Refused(why) = integrate(bare.path(), &sha) else {
        panic!("a non-repository must be refused");
    };
    assert!(why.contains("not a git repository"), "{why}");

    std::fs::write(root.join("README.md"), "the human was editing this\n").expect("write");
    let Integration::Refused(why) = integrate(&root, &sha) else {
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
