//! Reaping transient or orphaned tmux sessions without treating project terminals
//! as daemon-owned processes.
//!
//! Two entry points:
//!
//! - [`reap_owned_sessions`] — graceful daemon shutdown. Only short-lived
//!   supervisor consults are daemon-owned; project terminals survive.
//!
//! - [`sweep_orphan_loops`] — startup cleanup. Kill unified `pm-` terminals that
//!   no current registry row maps to. Legacy split-session terminals are left for
//!   explicit operator cleanup because pmd cannot prove nobody is attached to them.

use std::collections::HashSet;

use crate::registry::Registry;
use crate::tmux::{Driver, TmuxDriver, session_name};

/// Transient sessions pmd owns and may reap when it exits.
const OWNED_PREFIXES: [&str; 1] = ["pmsup-"];

/// Pure graceful-reap decision, separated from tmux calls for direct testing.
pub(super) fn owned_to_reap(sessions: &[String]) -> Vec<&str> {
    sessions
        .iter()
        .map(String::as_str)
        .filter(|s| OWNED_PREFIXES.iter().any(|p| s.starts_with(p)))
        .collect()
}

/// Pure startup-sweep decision. Only unified terminals are recognized; legacy
/// names are deliberately outside automatic ownership.
pub(super) fn orphan_loops<'a>(sessions: &'a [String], expected: &HashSet<String>) -> Vec<&'a str> {
    sessions
        .iter()
        .map(String::as_str)
        .filter(|s| s.starts_with("pm-") && !expected.contains(*s))
        .collect()
}

/// Kill every session pmd owns on this socket. Returns how many were terminated (for the
/// shutdown log line). `terminate` is idempotent, so a session that dies between the list
/// and the kill is not an error.
pub fn reap_owned_sessions(driver: &TmuxDriver) -> usize {
    let sessions = driver.list_sessions();
    owned_to_reap(&sessions)
        .into_iter()
        .filter(|s| driver.terminate(s).is_ok())
        .count()
}

/// Kill unified project terminals that belong to no current registry row. Every
/// mapped terminal and every legacy split-session terminal is left alone.
pub fn sweep_orphan_loops(driver: &TmuxDriver, reg: &Registry) -> usize {
    let expected: HashSet<String> = reg
        .projects
        .iter()
        .map(|p| session_name(&p.id, &p.root))
        .collect();
    let sessions = driver.list_sessions();
    orphan_loops(&sessions, &expected)
        .into_iter()
        .filter(|s| driver.terminate(s).is_ok())
        .count()
}
