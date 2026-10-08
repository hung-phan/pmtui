//! Creating a job's worktree, reading what it did, and refusing to delete what only lives there.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::worktree::{branch_name, collect, create, drop_branch, is_repo, prune};

/// `git -C <dir> <args…>`, which must succeed.
pub(super) fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with one commit, and no dependence on the machine's git identity or hooks.
pub(super) fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("proj");
    std::fs::create_dir_all(&root).expect("create the project");
    git(&root, &["init", "--quiet", "-b", "main"]);
    git(&root, &["config", "user.email", "pm@example.invalid"]);
    git(&root, &["config", "user.name", "pm tests"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("README.md"), "start\n").expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "--quiet", "-m", "start"]);
    (dir, root)
}

/// Commit `files` inside `dir` as a child would.
pub(super) fn commit(dir: &Path, message: &str, files: &[(&str, &str)]) -> String {
    for (name, body) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create");
        }
        std::fs::write(path, body).expect("write");
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A BRANCH PER JOB, NOT PER ID. Session ids are reused the moment a job retires, and `git worktree add
/// -b` refuses a branch that exists — two jobs that share an id across time must not share a branch.
#[test]
fn a_branch_name_is_unique_per_request_not_per_session_id() {
    let first = branch_name("proj-2", "550e8400-e29b-41d4-a716-446655440000");
    let second = branch_name("proj-2", "6ba7b810-9dad-41d1-80b4-00c04fd430c8");
    assert_ne!(first, second, "the same id, a different request");
    assert_eq!(first, "pm/proj-2-550e8400");
    assert!(
        !first.contains("..") && !first.contains(' '),
        "a branch name git will take: {first}"
    );
}

/// TWO CHILDREN, TWO CHECKOUTS, ONE `.git`. Each writes the same path without seeing the other, and both
/// commits are reachable from the parent checkout afterwards — which is what makes the work survive
/// without anyone copying a diff around.
#[test]
fn two_worktrees_write_the_same_file_without_colliding() {
    let (_tmp, root) = repo();
    let one = create(&root, &root.join(".project-state/wt/one"), "pm/one").expect("one");
    let two = create(&root, &root.join(".project-state/wt/two"), "pm/two").expect("two");
    assert_eq!(one.base, two.base, "both start from the parent's HEAD");

    let first = commit(&one.path, "one's change", &[("src/lib.rs", "// one\n")]);
    let second = commit(&two.path, "two's change", &[("src/lib.rs", "// two\n")]);
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read_to_string(one.path.join("src/lib.rs")).expect("one's file"),
        "// one\n",
        "neither child saw the other's edit"
    );

    // Both are in the parent's object store already, on their own branches.
    for (branch, sha) in [("pm/one", &first), ("pm/two", &second)] {
        assert_eq!(&git(&root, &["rev-parse", branch]), sha, "{branch}");
    }
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        one.base,
        "and the parent checkout has not moved"
    );
}

/// WHAT CAME BACK: the commit, the files it touched, and nothing else. A child that committed is clean, so
/// its worktree is prunable and its commit outlives the directory.
#[test]
fn collect_reports_the_commit_and_its_files_then_prunes_clean() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    assert!(collect(&wt).is_empty(), "nothing done yet");

    let sha = commit(
        &wt.path,
        "did the task",
        &[("src/a.rs", "// a\n"), ("docs/b.md", "b\n")],
    );
    let work = collect(&wt);
    assert_eq!(work.commit.as_deref(), Some(sha.as_str()));
    assert_eq!(work.branch, "pm/kid");
    assert_eq!(work.touched, vec!["docs/b.md", "src/a.rs"], "{work:?}");
    assert!(!work.dirty, "a child that committed left nothing behind");
    assert!(!work.is_empty());

    prune(&root, &wt).expect("a clean worktree is removable");
    assert!(!wt.path.exists(), "the directory is gone");
    assert_eq!(
        git(&root, &["rev-parse", "pm/kid"]),
        sha,
        "and the commit is still there, on its branch"
    );
    assert!(
        !git(&root, &["worktree", "list"]).contains("wt/kid"),
        "the repository forgot the worktree too"
    );
    drop_branch(&root, "pm/kid").expect("the branch goes when a human is done with it");
    assert!(
        drop_branch(&root, "pm/kid").is_err(),
        "and dropping it twice is an error, not a silent success"
    );
}

/// UNCOMMITTED WORK IS NEVER DELETED. It exists nowhere but that directory, so the one cleanup that could
/// destroy it declines — a job that ignored its instructions leaves evidence rather than nothing.
#[test]
fn a_dirty_worktree_is_kept_and_says_why() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/dirty"), "pm/dirty").expect("worktree");
    std::fs::write(wt.path.join("scratch.txt"), "half a thought\n").expect("write");

    let work = collect(&wt);
    assert!(work.dirty, "an untracked file is work");
    assert!(work.commit.is_none());
    assert!(!work.is_empty(), "so there is something to keep");

    let error = prune(&root, &wt).expect_err("a dirty worktree is not pruned");
    assert!(
        error.to_string().contains("uncommitted"),
        "and says why: {error:#}"
    );
    assert!(wt.path.exists(), "the directory stays");
}

/// EVERY WAY MAKING ONE CAN FAIL SAYS SO, rather than half-creating a directory a job would then run in:
/// a repository with no commit to branch from, a path whose parent cannot be made, and a branch name
/// already taken (which is why a branch carries its request id).
#[test]
fn creating_a_worktree_reports_each_way_it_can_fail() {
    // A repository with no HEAD yet.
    let empty = tempfile::tempdir().expect("tempdir");
    git(empty.path(), &["init", "--quiet", "-b", "main"]);
    let error = create(empty.path(), &empty.path().join("wt"), "pm/x")
        .expect_err("there is no commit to branch from");
    assert!(error.to_string().contains("no HEAD"), "{error:#}");

    let (_tmp, root) = repo();
    // A parent that cannot be created, because it is a FILE.
    std::fs::write(root.join("blocked"), "not a directory\n").expect("write");
    assert!(
        create(&root, &root.join("blocked/wt"), "pm/blocked").is_err(),
        "a worktree cannot be made under a file"
    );

    // And a branch that already exists — the case that makes a branch carry its request id.
    create(&root, &root.join(".project-state/wt/one"), "pm/taken").expect("the first");
    let error = create(&root, &root.join(".project-state/wt/two"), "pm/taken")
        .expect_err("the same branch twice");
    assert!(error.to_string().contains("pm/taken"), "{error:#}");
}

/// PRUNING WHAT GIT DOES NOT KNOW ABOUT is an error, not a silent delete. The directory stays, because
/// anything this cannot account for through git is not this function's to remove.
#[test]
fn pruning_a_directory_that_is_not_a_worktree_fails_and_keeps_it() {
    let (_tmp, root) = repo();
    let stray = root.join(".project-state/wt/stray");
    std::fs::create_dir_all(&stray).expect("create");
    std::fs::write(stray.join("file.txt"), "left over\n").expect("write");
    let pretend = crate::worktree::JobWorktree {
        path: stray.clone(),
        branch: "pm/stray".into(),
        base: "0".repeat(40),
    };

    assert!(
        prune(&root, &pretend).is_err(),
        "git does not know this directory"
    );
    assert!(stray.exists(), "so it is left where it is");
}

/// AN UNREADABLE STATUS COUNTS AS DIRTY. The only consumer of that flag is the cleanup that deletes the
/// directory, so the unknown answer has to be the one that keeps it — reading a git failure as "clean" is
/// what would let `--force` run over work nothing could see.
#[test]
fn a_worktree_whose_status_cannot_be_read_counts_as_dirty() {
    let (_tmp, root) = repo();
    let wt = create(&root, &root.join(".project-state/wt/kid"), "pm/kid").expect("worktree");
    // A directory that exists but is no longer a worktree of anything: `git status` fails in it.
    std::fs::remove_file(wt.path.join(".git")).expect("drop its git link");

    let work = collect(&wt);

    assert!(work.dirty, "an unreadable status is not clean: {work:?}");
    assert!(prune(&root, &wt).is_err(), "so nothing deletes it");
    assert!(wt.path.exists());
}

/// NOT EVERY PROJECT IS A REPOSITORY, and one that is not keeps the old behaviour rather than failing.
/// `collect` on a worktree whose directory is gone reports empty work for the same reason.
#[test]
fn a_directory_that_is_not_a_repository_is_recognised() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!is_repo(dir.path()), "no .git anywhere");
    assert!(
        !is_repo(&dir.path().join("does-not-exist")),
        "and a missing path is not a repository either"
    );
    assert!(
        create(dir.path(), &dir.path().join("wt"), "pm/x").is_err(),
        "so no worktree can be made in it"
    );

    let (_tmp, root) = repo();
    assert!(is_repo(&root));
    let wt = create(&root, &root.join(".project-state/wt/gone"), "pm/gone").expect("worktree");
    std::fs::remove_dir_all(&wt.path).expect("the purge took it");
    let work = collect(&wt);
    assert!(
        work.is_empty() && !work.dirty,
        "a worktree that is gone reports nothing, not an error: {work:?}"
    );
    prune(&root, &wt).expect("and pruning it is still fine");
}
