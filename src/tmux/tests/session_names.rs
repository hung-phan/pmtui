//! Tests for the unified project-session name and the separate supervisor namespace.

use std::path::Path;

use crate::tmux::{session_name, supervisor_session_name};

#[test]
fn project_session_name_is_neutral_stable_and_disambiguated() {
    let a = session_name("auth.rewrite", Path::new("/tmp/a"));
    let b = session_name("auth.rewrite", Path::new("/tmp/a"));
    let c = session_name("auth.rewrite", Path::new("/tmp/b"));
    let d = session_name("auth.other", Path::new("/tmp/a"));
    assert!(a.starts_with("pm-auth_rewrite-"), "{a}");
    assert_eq!(a, b, "stable for the same id+root");
    assert_ne!(a, c, "different root disambiguates");
    assert_ne!(a, d, "different id disambiguates");
}

#[test]
fn supervisor_names_remain_separate_and_sequenced() {
    let root = Path::new("/tmp/a");
    let session = session_name("auth.rewrite", root);
    let first = supervisor_session_name("auth.rewrite", root, 1);
    let second = supervisor_session_name("auth.rewrite", root, 2);
    assert!(first.starts_with("pmsup-auth_rewrite-"), "{first}");
    assert_ne!(session, first);
    assert_ne!(first, second);
}
