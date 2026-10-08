//! Tests for the typed interactive-launch vocabulary: the managed session env's `-e`
//! pairs, which failures prove nothing ran, the text every caller shows for a failure,
//! and where `PMTUI_BIN` comes from.

use std::path::PathBuf;
use tempfile::tempdir;

use crate::tmux::launch::pmtui_bin_for_exe;
use crate::tmux::{
    ENV_BIN, ENV_SESSION, ENV_STATE_DIR, LAUNCH_COMMAND_MAX_BYTES, LaunchError, LaunchOutcome,
    ManagedEnv, pmtui_bin_for_current_process,
};

fn pair(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

#[test]
fn vars_orders_session_state_dir_then_bin_and_omits_absent_bin() {
    assert_eq!(
        (ENV_SESSION, ENV_STATE_DIR, ENV_BIN),
        ("PMTUI_SESSION", "PMTUI_STATE_DIR", "PMTUI_BIN")
    );
    let mut env = ManagedEnv {
        session_id: "s1".into(),
        state_dir: PathBuf::from("/x/.project-state/sessions/s1-0a1b2c3d"),
        pmtui_bin: Some(PathBuf::from("/bin/pmtui")),
    };
    assert_eq!(
        env.vars(),
        vec![
            pair("PMTUI_SESSION", "s1"),
            pair("PMTUI_STATE_DIR", "/x/.project-state/sessions/s1-0a1b2c3d"),
            pair("PMTUI_BIN", "/bin/pmtui"),
        ]
    );

    env.pmtui_bin = None;
    assert_eq!(
        env.vars(),
        vec![
            pair("PMTUI_SESSION", "s1"),
            pair("PMTUI_STATE_DIR", "/x/.project-state/sessions/s1-0a1b2c3d"),
        ],
        "an absent pmtui is omitted, never passed as an empty path"
    );
}

#[test]
fn only_pre_tmux_failures_are_proven_not_started() {
    let proven = [
        LaunchError::EmptyArgv,
        LaunchError::NotOnPath("claude".into()),
        LaunchError::CommandTooLong {
            session: "pm-a".into(),
            bytes: LAUNCH_COMMAND_MAX_BYTES + 1,
        },
    ];
    for error in &proven {
        assert!(error.proven_not_started(), "{error:?}");
    }
    let ambiguous = [
        LaunchError::NewSessionFailed("tmux new-session failed".into()),
        LaunchError::ExitedAfterStart("exited immediately".into()),
        LaunchError::Probe("run tmux has-session".into()),
    ];
    for error in &ambiguous {
        assert!(!error.proven_not_started(), "{error:?}");
    }
}

#[test]
fn display_keeps_the_text_every_caller_already_shows() {
    let cases = [
        (
            LaunchError::EmptyArgv,
            "launch_interactive: empty argv".to_string(),
        ),
        (
            LaunchError::NotOnPath("claude".into()),
            "\"claude\" was not found on PATH — is it installed?".to_string(),
        ),
        (
            LaunchError::CommandTooLong {
                session: "pm-a-1".into(),
                bytes: 12_300,
            },
            format!(
                "launch command for pm-a-1 is 12300 bytes after shell quoting; tmux accepts at most {LAUNCH_COMMAND_MAX_BYTES}"
            ),
        ),
        (
            LaunchError::NewSessionFailed(
                "tmux new-session failed for interactive session pm-a-1".into(),
            ),
            "tmux new-session failed for interactive session pm-a-1".to_string(),
        ),
        (
            LaunchError::ExitedAfterStart(
                "interactive session pm-a-1 exited immediately (did \"claude\" fail to start?)"
                    .into(),
            ),
            "interactive session pm-a-1 exited immediately (did \"claude\" fail to start?)"
                .to_string(),
        ),
        (
            LaunchError::Probe("run tmux has-session: No such file or directory".into()),
            "run tmux has-session: No such file or directory".to_string(),
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
        // A std error, so `?` and `.context(..)` keep working at every caller.
        let wrapped = anyhow::Error::new(error.clone()).context("launch pm-a-1");
        assert_eq!(format!("{wrapped:#}"), format!("launch pm-a-1: {text}"));
    }
    assert_ne!(LaunchOutcome::Started, LaunchOutcome::AlreadyAlive);
}

#[test]
fn pmtui_bin_is_the_pmtui_executable_itself_or_the_one_beside_pmd() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
    let pmd = dir.path().join("pmd");
    std::fs::write(&pmd, "").unwrap();
    assert_eq!(
        pmtui_bin_for_exe(&pmd),
        None,
        "pmd with no sibling pmtui omits the variable"
    );

    let pmtui = dir.path().join("pmtui");
    std::fs::write(&pmtui, "").unwrap();
    std::fs::set_permissions(&pmtui, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        pmtui_bin_for_exe(&pmd),
        None,
        "a sibling that cannot be executed is not a pmtui to run"
    );

    std::fs::set_permissions(&pmtui, std::fs::Permissions::from_mode(0o755)).unwrap();
    let expected = Some(canonical_dir.join("pmtui"));
    assert_eq!(pmtui_bin_for_exe(&pmd), expected, "pmd passes its sibling");
    assert_eq!(pmtui_bin_for_exe(&pmtui), expected, "pmtui passes itself");

    // A symlinked install resolves to the real executable.
    let link_dir = tempdir().unwrap();
    let link = link_dir.path().join("pmtui");
    std::os::unix::fs::symlink(&pmtui, &link).unwrap();
    assert_eq!(pmtui_bin_for_exe(&link), expected);

    // A directory named pmtui is not an executable.
    let other = tempdir().unwrap();
    std::fs::create_dir(other.path().join("pmtui")).unwrap();
    assert_eq!(pmtui_bin_for_exe(&other.path().join("pmd")), None);
}

#[test]
fn the_current_process_resolves_through_its_own_executable() {
    // A test binary is neither `pmtui` nor beside one (cargo names it `<crate>-<hash>` inside
    // `deps/`), so the managed env of every unit-test launch omits PMTUI_BIN.
    let exe = std::env::current_exe().unwrap();
    assert_eq!(pmtui_bin_for_current_process(), pmtui_bin_for_exe(&exe));
    assert_eq!(pmtui_bin_for_current_process(), None);
}
