use super::*;
use std::process::{Command, Stdio};

const THREAD_ID: &str = "01a133c3-b12f-7033-ae77-38f3294fb68e";
const PAYLOAD: &str = r#"{"session_id":"01a133c3-b12f-7033-ae77-38f3294fb68e"}"#;

fn hook_env(paths: &ProjectPaths) -> impl Fn(&str) -> Option<String> + '_ {
    move |key| {
        (key == tmux::ENV_STATE_DIR).then(|| paths.state_dir().to_string_lossy().into_owned())
    }
}

#[test]
fn hook_syntax_rejects_missing_unknown_and_extra_arguments() {
    for (args, message, json) in [
        (
            vec!["hook"],
            "hook: expected turn-complete or session-start",
            false,
        ),
        (
            vec!["hook", "unknown"],
            "hook: unknown hook \"unknown\"",
            false,
        ),
        (
            vec!["hook", "session-start", "--json"],
            "hook: unexpected argument \"--json\"",
            true,
        ),
        (
            vec!["hook", "session-start", "extra"],
            "hook: unexpected argument \"extra\"",
            false,
        ),
        (
            vec!["hook", "turn-complete", "extra"],
            "hook: unexpected argument \"extra\"",
            false,
        ),
    ] {
        let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
        let error = parse_action(&args).unwrap_err();
        assert_eq!(error.message, message);
        assert_eq!(error.json, json);
    }
    for (name, action) in [
        ("turn-complete", Action::HookTurnComplete),
        ("session-start", Action::HookSessionStart),
    ] {
        assert_eq!(parse_action(&["hook".into(), name.into()]).unwrap(), action);
    }
}

#[test]
fn identity_rejects_invalid_state_locations_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, "keep").unwrap();
    for value in [
        None,
        Some(String::new()),
        Some(" \t ".into()),
        Some(".project-state".into()),
        Some(dir.path().join("missing").display().to_string()),
        Some(file.display().to_string()),
        Some(dir.path().display().to_string()),
    ] {
        let env = |key: &str| {
            (key == tmux::ENV_STATE_DIR)
                .then(|| value.clone())
                .flatten()
        };
        let error = record_session_identity(&env, PAYLOAD).unwrap_err();
        let text = error.to_string();
        assert!(text.contains(tmux::ENV_STATE_DIR), "{value:?}: {error:#}");
        if value.as_ref().is_none_or(|value| value.trim().is_empty()) {
            assert!(text.contains("is not set"), "{error:#}");
        } else if value.as_deref() == dir.path().to_str() {
            assert!(text.contains("not a pmtui state directory"), "{error:#}");
        } else {
            assert!(
                text.contains("not an existing absolute directory"),
                "{error:#}"
            );
        }
    }
    assert_eq!(std::fs::read_to_string(file).unwrap(), "keep");
    assert!(!dir.path().join(".daemon").exists());
}

#[test]
fn identity_directory_failure_stops_before_the_turn_signal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "blocked-daemon");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    // A file collision fails even as root, unlike chmod-based permission fixtures.
    std::fs::write(paths.daemon_dir(), "keep").unwrap();
    let error = run_turn_complete_hook(&hook_env(&paths), PAYLOAD).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("create_dir_all {}", paths.daemon_dir().display())
    );
    assert_eq!(std::fs::read_to_string(paths.daemon_dir()).unwrap(), "keep");
    assert!(!paths.turn_signal().exists());
}

#[test]
fn failed_identity_replacement_preserves_existing_state_and_turn_count() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "blocked-identity");
    std::fs::create_dir_all(paths.codex_conversation_id()).unwrap();
    let sentinel = paths.codex_conversation_id().join("keep");
    std::fs::write(&sentinel, "existing identity state").unwrap();
    std::fs::write(paths.turn_signal(), "..").unwrap();

    let error = run_turn_complete_hook(&hook_env(&paths), PAYLOAD).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("write {}", paths.codex_conversation_id().display())
    );
    assert_eq!(std::fs::read(paths.turn_signal()).unwrap(), b"..");
    assert_eq!(
        std::fs::read_to_string(sentinel).unwrap(),
        "existing identity state"
    );
    assert_eq!(std::fs::read_dir(paths.daemon_dir()).unwrap().count(), 2);
}

#[test]
fn turn_open_failure_keeps_the_identity_already_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "blocked-signal");
    std::fs::create_dir_all(paths.turn_signal()).unwrap();
    let error = run_turn_complete_hook(&hook_env(&paths), PAYLOAD).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("open {}", paths.turn_signal().display())
    );
    assert_eq!(
        read_captured_conversation_id(&paths).as_deref(),
        Some(THREAD_ID)
    );
    assert!(paths.turn_signal().is_dir());
}

#[cfg(unix)]
#[test]
fn turn_sync_failure_is_reported_after_identity_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "unsyncable-signal");
    std::fs::create_dir_all(paths.daemon_dir()).unwrap();
    // The null device accepts the write but cannot promise durable storage.
    std::os::unix::fs::symlink("/dev/null", paths.turn_signal()).unwrap();
    let error = run_turn_complete_hook(&hook_env(&paths), PAYLOAD).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("sync {}", paths.turn_signal().display())
    );
    assert_eq!(
        read_captured_conversation_id(&paths).as_deref(),
        Some(THREAD_ID)
    );
}

#[test]
fn malformed_identity_fields_never_replace_a_known_thread() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "known-thread");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    record_session_identity(&hook_env(&paths), PAYLOAD).unwrap();
    for payload in [
        "null",
        "[]",
        r#"{"session_id":null}"#,
        r#"{"session_id":42}"#,
        r#"{"session_id":{"id":"01a133c3-b12f-7033-ae77-38f3294fb68e"}}"#,
        r#"{"session_id":"bad","thread_name":"01a133c3-b12f-7033-ae77-38f3294fb68e"}"#,
        r#"{"last_assistant_message":"{\"session_id\":\"01a133c3-b12f-7033-ae77-38f3294fb68e\"}"}"#,
    ] {
        assert_eq!(reported_thread_id(payload), None, "{payload}");
        record_session_identity(&hook_env(&paths), payload).unwrap();
        assert_eq!(
            read_captured_conversation_id(&paths).as_deref(),
            Some(THREAD_ID)
        );
        assert!(!paths.turn_signal().exists());
    }
}

// Re-exec isolates stdin and environment from the parent test runner. No engine,
// dashboard, or trust prompt runs in these hook contract tests.
fn dispatch_child(hook: &str, state_dir: Option<&Path>, payload: &[u8], expected_error: &str) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::hook_regressions::hook_dispatch_child",
            "--ignored",
            "--nocapture",
        ])
        .env("PM_TEST_HOOK", hook)
        .env("PM_TEST_HOOK_ERROR", expected_error)
        .env_remove(tmux::ENV_STATE_DIR)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(state_dir) = state_dir {
        command.env(tmux::ENV_STATE_DIR, state_dir);
    }
    let mut child = command.spawn().unwrap();
    child.stdin.take().unwrap().write_all(payload).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "subprocess helper: stdin and environment are supplied by dispatch_child"]
fn hook_dispatch_child() {
    let hook = std::env::var("PM_TEST_HOOK").unwrap();
    let expected_error = std::env::var("PM_TEST_HOOK_ERROR").unwrap();
    let env = |key: &str| {
        assert_eq!(key, tmux::ENV_STATE_DIR, "hooks need no registry or HOME");
        std::env::var(key).ok()
    };
    let mut waiter = crate::spawn_cli::ThreadWaiter::new();
    let mut deps = crate::spawn_cli::SpawnDeps {
        env: &env,
        clock: &SystemClock,
        waiter: &mut waiter,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = dispatch(
        parse_action(&["hook".into(), hook.clone()]),
        &mut |_| panic!("a hook must not open the dashboard"),
        &mut deps,
        &mut out,
        &mut err,
    )
    .unwrap();
    assert!(out.is_empty());
    if expected_error.is_empty() {
        assert_eq!(code, 0);
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    } else {
        assert_eq!(code, 1);
        let err = String::from_utf8(err).unwrap();
        assert!(err.starts_with(&format!("hook {hook}: ")), "{err}");
        assert!(err.contains(&expected_error), "{err}");
    }
}

#[test]
fn hook_dispatch_reads_piped_identity_and_keeps_start_separate_from_stop() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "dispatch");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    dispatch_child(
        "session-start",
        Some(&paths.state_dir()),
        PAYLOAD.as_bytes(),
        "",
    );
    assert_eq!(
        read_captured_conversation_id(&paths).as_deref(),
        Some(THREAD_ID)
    );
    assert!(!paths.turn_signal().exists());
    for _ in 0..2 {
        dispatch_child(
            "turn-complete",
            Some(&paths.state_dir()),
            PAYLOAD.as_bytes(),
            "",
        );
    }
    assert_eq!(std::fs::read(paths.turn_signal()).unwrap(), b"..");
}

#[test]
fn hook_dispatch_reports_missing_environment_and_filesystem_failures() {
    for hook in ["session-start", "turn-complete"] {
        dispatch_child(hook, None, b"", "PMTUI_STATE_DIR is not set");
        let dir = tempfile::tempdir().unwrap();
        let paths = ProjectPaths::for_session(dir.path(), "dispatch-error");
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::write(paths.daemon_dir(), "keep").unwrap();
        dispatch_child(
            hook,
            Some(&paths.state_dir()),
            PAYLOAD.as_bytes(),
            "create_dir_all",
        );
        assert_eq!(std::fs::read(paths.daemon_dir()).unwrap(), b"keep");
    }
}

#[test]
fn hook_payload_reads_to_eof_so_large_payloads_keep_identity() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "payload-limit");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    const OLD_LIMIT: usize = 1 << 20;
    let mut payload = format!(r#"{{"session_id":"{THREAD_ID}","last_assistant_message":""#);
    payload.push_str(&"x".repeat(OLD_LIMIT - payload.len() - 2));
    payload.push_str("\"}");
    assert_eq!(payload.len(), OLD_LIMIT);
    dispatch_child(
        "session-start",
        Some(&paths.state_dir()),
        payload.as_bytes(),
        "",
    );
    assert_eq!(
        read_captured_conversation_id(&paths).as_deref(),
        Some(THREAD_ID)
    );
    std::fs::remove_file(paths.codex_conversation_id()).unwrap();

    payload.insert(payload.len() - 2, 'x');
    assert!(payload.len() > OLD_LIMIT);
    dispatch_child(
        "turn-complete",
        Some(&paths.state_dir()),
        payload.as_bytes(),
        "",
    );
    assert_eq!(
        read_captured_conversation_id(&paths).as_deref(),
        Some(THREAD_ID)
    );
    assert_eq!(std::fs::read(paths.turn_signal()).unwrap(), b".");
    std::fs::remove_file(paths.codex_conversation_id()).unwrap();

    dispatch_child("turn-complete", Some(&paths.state_dir()), b"\xff", "");
    dispatch_child("turn-complete", Some(&paths.state_dir()), b"", "");
    assert!(!paths.codex_conversation_id().exists());
    assert_eq!(std::fs::read(paths.turn_signal()).unwrap(), b"...");
}
