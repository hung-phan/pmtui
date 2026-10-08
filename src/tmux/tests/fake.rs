//! Tests for the fake driver itself — the recording and the armed probes other
//! modules' tests assert against, pinned here so a fake that quietly stopped
//! recording could not make those tests pass for the wrong reason.

use std::path::Path;

use crate::tmux::fake::FakeDriver;
use crate::tmux::{Driver, LaunchError, LaunchOutcome, ManagedEnv};
use tempfile::tempdir;

#[test]
fn fake_launch_interactive_marks_alive_and_records_argv() {
    let d = FakeDriver::new();
    assert!(!d.is_alive("pmloop-x").unwrap());
    let env = ManagedEnv {
        session_id: "x".into(),
        state_dir: "/tmp/.project-state/sessions/x-00000000".into(),
        pmtui_bin: Some("/bin/pmtui".into()),
    };
    assert_eq!(
        d.launch_interactive(
            "pmloop-x",
            Path::new("/tmp"),
            &["claude".into(), "--session-id".into(), "u1".into()],
            &env,
        ),
        Ok(LaunchOutcome::Started)
    );
    assert!(
        d.is_alive("pmloop-x").unwrap(),
        "launch marks the session alive"
    );
    assert_eq!(
        d.launched(),
        vec![(
            "pmloop-x".to_string(),
            vec![
                "claude".to_string(),
                "--session-id".to_string(),
                "u1".to_string()
            ]
        )]
    );
    assert_eq!(
        d.launched_env(),
        vec![("pmloop-x".to_string(), env.clone())]
    );

    // Idempotent by name, as in production: an alive session keeps its process and our argv
    // (and env) are never used.
    assert_eq!(
        d.launch_interactive("pmloop-x", Path::new("/tmp"), &["other".into()], &env),
        Ok(LaunchOutcome::AlreadyAlive)
    );
    assert_eq!(d.launched().len(), 1);
    assert_eq!(d.launched_env().len(), 1);
}

#[test]
fn an_armed_launch_error_fires_once_for_its_session_and_launches_nothing() {
    let d = FakeDriver::new();
    let env = ManagedEnv::default();
    d.arm_launch_error("pm-a", LaunchError::NotOnPath("claude".into()));
    assert_eq!(
        d.launch_interactive("pm-b", Path::new("/tmp"), &["claude".into()], &env),
        Ok(LaunchOutcome::Started),
        "the error is armed for one session only"
    );
    assert_eq!(
        d.launch_interactive("pm-a", Path::new("/tmp"), &["claude".into()], &env),
        Err(LaunchError::NotOnPath("claude".into()))
    );
    assert!(
        !d.is_alive("pm-a").unwrap(),
        "a failed launch starts nothing"
    );
    assert_eq!(
        d.launched()
            .into_iter()
            .map(|(session, _)| session)
            .collect::<Vec<_>>(),
        ["pm-b"]
    );
    assert_eq!(
        d.launch_interactive("pm-a", Path::new("/tmp"), &["claude".into()], &env),
        Ok(LaunchOutcome::Started),
        "one-shot: the next launch goes through"
    );
}

#[test]
fn fake_send_keys_records_session_and_text_in_order() {
    let d = FakeDriver::new();
    d.send_keys("sess-1", "hello world").unwrap();
    d.send_keys("sess-1", "second nudge").unwrap();
    d.send_keys("other", "multi\nline\npayload").unwrap();
    assert_eq!(
        d.sent_keys(),
        vec![
            ("sess-1".to_string(), "hello world".to_string()),
            ("sess-1".to_string(), "second nudge".to_string()),
            ("other".to_string(), "multi\nline\npayload".to_string()),
        ]
    );
}

#[test]
fn fake_dialog_selection_records_session_and_indices_in_order() {
    let d = FakeDriver::new();
    d.select_dialog_option("sess-1", 0, 2).unwrap();
    d.select_dialog_option("sess-2", 2, 1).unwrap();
    assert_eq!(
        d.selected_dialog_options(),
        vec![("sess-1".to_string(), 0, 2), ("sess-2".to_string(), 2, 1),]
    );
}

#[test]
fn fake_verifies_and_records_structured_dialog_selections() {
    let d = FakeDriver::new();
    let dialog = crate::tmux::classify_dialog(concat!(
        " Which paths?\n",
        " ❯ 1. [ ] A\n",
        "   2. [ ] B\n",
        "      Submit\n",
        " Enter to select · Esc to cancel\n",
    ))
    .unwrap();
    let moved = d.verify_dialog_interactive("sess", &dialog).unwrap();
    assert_eq!(moved.selected_index, Some(1));
    d.select_dialog_options("sess", &moved, &[0, 1]).unwrap();
    assert_eq!(
        d.applied_dialog_selections(),
        vec![("sess".to_string(), vec![0, 1])]
    );
}

#[test]
fn fake_driver_reports_a_pane_left_in_copy_mode() {
    let d = FakeDriver::new();
    assert!(!d.pane_in_mode("s").unwrap(), "absent ⇒ not in a mode");
    d.set_pane_in_mode("s", true);
    assert!(d.pane_in_mode("s").unwrap());
    d.set_pane_in_mode("s", false);
    assert!(!d.pane_in_mode("s").unwrap());
}

#[test]
fn fake_driver_injects_exact_codex_ids_and_probe_failures() {
    let d = FakeDriver::new();
    assert_eq!(d.codex_session_id("s", Path::new("/tmp")).unwrap(), None);
    d.set_codex_session_id("s", "11111111-2222-4333-8444-555555555555");
    assert_eq!(
        d.codex_session_id("s", Path::new("/tmp"))
            .unwrap()
            .as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
    d.fail_codex_session_id("s", true);
    assert!(d.codex_session_id("s", Path::new("/tmp")).is_err());
    d.fail_codex_session_id("s", false);
    assert!(d.codex_session_id("s", Path::new("/tmp")).is_ok());
}

#[test]
fn fake_driver_exercises_state_failure_and_cleanup_paths() {
    let dir = tempdir().unwrap();
    let d = FakeDriver::new();
    let done = dir.path().join("done");
    let log = dir.path().join("log");

    assert_eq!(d.command_for("step"), None);
    assert_eq!(d.spawn_count(), 0);
    let handle = d
        .spawn_step(
            "step",
            dir.path(),
            &["sh".into(), "-c".into(), "exit 0".into()],
            &done,
            &log,
        )
        .unwrap();
    assert_eq!(handle.done_signal, done);
    assert_eq!(
        d.command_for("step").unwrap(),
        ["sh", "-c", "exit 0"].map(str::to_string)
    );
    assert_eq!(d.spawn_count(), 1);

    d.set_tail("step", "tail");
    assert_eq!(d.capture_tail("step", 1).unwrap(), "tail");
    assert_eq!(d.capture_tail_styled("step", 1).unwrap(), "tail");
    assert_eq!(d.capture_tail("missing", 1).unwrap(), "");

    d.set_clients("step", true);
    d.set_created("step", 42);
    d.set_pane_dead("step", true);
    assert!(d.has_clients("step").unwrap());
    assert_eq!(d.session_created("step").unwrap(), Some(42));
    assert!(d.pane_dead("step").unwrap());
    assert!(!d.pane_dead("missing").unwrap());

    d.fail_is_alive("probe");
    assert!(d.is_alive("probe").is_err());

    d.fail_send_keys("step", true);
    assert!(d.send_keys("step", "blocked").is_err());
    d.fail_send_keys("step", false);
    d.send_keys("step", "sent").unwrap();

    let dialog = crate::tmux::classify_dialog(concat!(
        " Which path?\n",
        " ❯ 1. A\n",
        "   2. B\n",
        " Enter to select · Esc to cancel\n",
    ))
    .unwrap();
    d.fail_select_dialog("step", true);
    assert!(d.select_dialog_option("step", 0, 1).is_err());
    assert!(d.select_dialog_options("step", &dialog, &[1]).is_err());
    d.fail_select_dialog("step", false);
    d.select_dialog_option("step", 0, 1).unwrap();
    d.select_dialog_options("step", &dialog, &[1]).unwrap();

    d.fail_verify_dialog("step", true);
    assert!(d.verify_dialog_interactive("step", &dialog).is_err());
    d.fail_verify_dialog("step", false);
    let mut no_selection = dialog.clone();
    no_selection.selected_index = None;
    assert!(d.verify_dialog_interactive("step", &no_selection).is_err());
    let mut one_option = dialog.clone();
    one_option.options.truncate(1);
    one_option.selected_index = Some(0);
    assert!(d.verify_dialog_interactive("step", &one_option).is_err());
    let mut last_selected = dialog.clone();
    last_selected.selected_index = Some(1);
    assert_eq!(
        d.verify_dialog_interactive("step", &last_selected)
            .unwrap()
            .selected_index,
        Some(0)
    );

    d.terminate("step").unwrap();
    assert!(!d.is_alive("step").unwrap());
    assert!(!d.has_clients("step").unwrap());
    assert_eq!(d.session_created("step").unwrap(), None);
}
