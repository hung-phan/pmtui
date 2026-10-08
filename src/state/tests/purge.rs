//! Deleting one session's state subtree — and refusing every other shape.
//!
//! This is the only function in `state` that removes a tree, so what is asserted here is mostly what it
//! will NOT do: not the project's own `.project-state/`, not a sibling session, and not whatever a
//! symlink in a session-writable directory points at.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use tempfile::tempdir;

use crate::state::{ProjectPaths, purge_session_state};

/// A session's own subtree goes, and only that one: its sibling and the project's root state survive.
#[test]
fn a_sessions_subtree_goes_and_its_siblings_stay() {
    let root = tempdir().unwrap();
    let mine = ProjectPaths::for_session(root.path(), "kid");
    let sibling = ProjectPaths::for_session(root.path(), "other");
    let project = ProjectPaths::new(root.path());
    for paths in [&mine, &sibling] {
        fs::create_dir_all(paths.steps_dir()).unwrap();
        fs::write(paths.job_log(), "what it did\n").unwrap();
    }
    fs::create_dir_all(project.state_dir()).unwrap();
    fs::write(project.session(), "{}").unwrap();

    purge_session_state(&mine).expect("the session's own subtree");

    assert!(!mine.state_dir().exists(), "the finished session is gone");
    assert!(sibling.job_log().is_file(), "a sibling is untouched");
    assert!(
        project.session().is_file(),
        "the project's state is untouched"
    );
    assert!(root.path().join(".project-state/sessions").is_dir());
}

/// ALREADY GONE IS SUCCESS: the caller wanted it absent, and a retirement that runs twice must not fail.
#[test]
fn purging_what_is_already_gone_succeeds() {
    let root = tempdir().unwrap();
    let paths = ProjectPaths::for_session(root.path(), "kid");
    purge_session_state(&paths).expect("nothing to do");
    fs::create_dir_all(paths.state_dir()).unwrap();
    purge_session_state(&paths).expect("the first call");
    purge_session_state(&paths).expect("and the second");
}

/// A ROOT `ProjectPaths` IS REFUSED. It resolves to `.project-state/` itself — every sibling session's
/// state plus the project ledger — which no retirement is ever allowed to take with it.
#[test]
fn the_projects_own_state_directory_is_refused() {
    let root = tempdir().unwrap();
    let project = ProjectPaths::new(root.path());
    fs::create_dir_all(project.state_dir()).unwrap();
    let sibling = ProjectPaths::for_session(root.path(), "kid");
    fs::create_dir_all(sibling.state_dir()).unwrap();

    let error = purge_session_state(&project).unwrap_err();

    assert!(
        format!("{error:#}").contains("not a session's own state directory"),
        "{error:#}"
    );
    assert!(project.state_dir().is_dir(), "nothing was removed");
    assert!(sibling.state_dir().is_dir());
}

/// A DIRECTORY IT CANNOT EVEN LOOK AT is reported, not assumed gone: "already absent" is only ever
/// concluded from a real NotFound.
#[test]
fn an_unreadable_parent_is_reported_rather_than_treated_as_absent() {
    let root = tempdir().unwrap();
    let paths = ProjectPaths::for_session(root.path(), "kid");
    let sessions = paths.state_dir().parent().unwrap().to_path_buf();
    fs::create_dir_all(paths.state_dir()).unwrap();
    // No search permission on the parent, so even stat-ing the child fails.
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o000)).unwrap();

    let result = purge_session_state(&paths);

    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();
    assert!(format!("{error:#}").contains("inspect"), "{error:#}");
    assert!(paths.state_dir().is_dir(), "and it is still there");
}

/// A TREE IT CANNOT REMOVE is reported too, naming the directory, so a retirement that could not finish
/// says so instead of logging success.
#[test]
fn a_tree_that_cannot_be_removed_is_reported() {
    let root = tempdir().unwrap();
    let paths = ProjectPaths::for_session(root.path(), "kid");
    let sessions = paths.state_dir().parent().unwrap().to_path_buf();
    fs::create_dir_all(paths.steps_dir()).unwrap();
    fs::write(paths.job_log(), "what it did\n").unwrap();
    // Searchable but not writable: the entry cannot be unlinked from its parent.
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o500)).unwrap();

    let result = purge_session_state(&paths);

    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("remove"), "{text}");
    assert!(
        text.contains("kid-"),
        "the message names the directory: {text}"
    );
}

/// A SYMLINK IS NOT FOLLOWED. A session's state directory is writable by that session's own agent, so a
/// planted link must not turn retirement into a delete of whatever it points at.
#[test]
fn a_symlinked_state_directory_is_refused_not_followed() {
    let root = tempdir().unwrap();
    let elsewhere = tempdir().unwrap();
    let victim = elsewhere.path().join("keep-me");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("file"), "precious").unwrap();

    let paths = ProjectPaths::for_session(root.path(), "kid");
    fs::create_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, paths.state_dir()).unwrap();

    let error = purge_session_state(&paths).unwrap_err();

    assert!(
        format!("{error:#}").contains("not a real directory"),
        "{error:#}"
    );
    assert!(victim.join("file").is_file(), "the link's target survives");
}
