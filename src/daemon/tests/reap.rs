//! Graceful and startup reaping over pure name filters and a scripted tmux driver.

use super::*;
use crate::daemon::reap::{orphan_loops, owned_to_reap, reap_owned_sessions, sweep_orphan_loops};
use crate::tmux::{TmuxDriver, session_name};
use std::collections::HashSet;

struct ScriptedTmux {
    _dir: tempfile::TempDir,
    driver: TmuxDriver,
    log: std::path::PathBuf,
}

impl ScriptedTmux {
    fn new(sessions: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tmux");
        let session_list = dir.path().join("sessions");
        let log = dir.path().join("calls");
        std::fs::write(&session_list, format!("{}\n", sessions.join("\n"))).unwrap();
        std::fs::write(
            &bin,
            format!(
                r#"#!/bin/sh
set -eu
[ "${{1:-}}" = "-L" ] && shift 2
case "${{1:-}}" in
list-sessions) cat '{}' ;;
kill-session) printf '%s\n' "$*" >> '{}' ;;
*) exit 1 ;;
esac
"#,
                session_list.display(),
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            driver: TmuxDriver {
                tmux: bin.to_string_lossy().into_owned(),
                socket: Some("scripted".into()),
            },
            _dir: dir,
            log,
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn graceful_reap_only_takes_transient_supervisor_sessions() {
    let sessions = strings(&[
        "pm-bot-abc123",
        "pmsup-bot-4",
        "pmloop-legacy-abc123",
        "pmchat-legacy-abc123",
        "some-unrelated",
    ]);
    let reap = owned_to_reap(&sessions);
    assert_eq!(reap, ["pmsup-bot-4"]);
    assert!(!reap.contains(&"pm-bot-abc123"), "pmd is only a driver");
}

#[test]
fn startup_sweep_selects_only_unmapped_unified_sessions() {
    let kept = session_name("kept", std::path::Path::new("/tmp/kept"));
    let gone = session_name("gone", std::path::Path::new("/tmp/gone"));
    let expected: HashSet<String> = [kept.clone()].into_iter().collect();
    let sessions = strings(&[
        &kept,
        &gone,
        "pmloop-legacy-xyz",
        "pmchat-legacy-xyz",
        "some-unrelated",
    ]);

    let orphans = orphan_loops(&sessions, &expected);
    assert_eq!(
        orphans,
        [gone.as_str()],
        "legacy split-session terminals require explicit operator cleanup"
    );
    assert!(!orphans.contains(&kept.as_str()));
}

#[test]
fn concrete_reapers_terminate_only_the_sessions_they_own() {
    let dir = tempfile::tempdir().unwrap();
    let transient = "pmsup-consult-1";
    let kept = session_name("kept", dir.path());
    let orphan_root = dir.path().join("orphan");
    let orphan = session_name("orphan", &orphan_root);
    let fixture = ScriptedTmux::new(&[
        transient,
        &kept,
        &orphan,
        "pmloop-legacy-1",
        "some-unrelated",
    ]);

    assert_eq!(reap_owned_sessions(&fixture.driver), 1);

    let registry = Registry {
        projects: vec![agent_loop_entry("kept", dir.path())],
    };
    assert_eq!(sweep_orphan_loops(&fixture.driver, &registry), 1);
    assert_eq!(
        fixture.calls(),
        vec![
            format!("kill-session -t ={transient}"),
            format!("kill-session -t ={orphan}"),
        ]
    );
}
