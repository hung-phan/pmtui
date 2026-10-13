use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn pmtui_arguments_parse_registry_socket_and_help() {
    let args = parse_args_from(&strings(&[
        "--registry",
        "/tmp/r.json",
        "--socket",
        "isolated",
    ]))
    .unwrap();
    assert_eq!(args.registry, PathBuf::from("/tmp/r.json"));
    assert_eq!(args.socket, "isolated");
    assert!(!args.help);

    assert!(parse_args_from(&strings(&["--help"])).unwrap().help);
}

#[test]
fn pmtui_arguments_reject_unknown_or_missing_values() {
    assert!(parse_args_from(&strings(&["--regsitry", "/tmp/r.json"])).is_err());
    assert!(parse_args_from(&strings(&["--registry"])).is_err());
    assert!(parse_args_from(&strings(&["--socket"])).is_err());
}

#[test]
fn hook_turn_complete_appends_to_the_managed_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join(".project-state/sessions/bot");
    std::fs::create_dir_all(&state_dir).unwrap();
    let env =
        |key: &str| (key == tmux::ENV_STATE_DIR).then(|| state_dir.to_string_lossy().into_owned());

    run_turn_complete_hook(&env, "").unwrap();
    run_turn_complete_hook(&env, "").unwrap();

    assert_eq!(
        std::fs::read(state_dir.join(".daemon/turn-complete")).unwrap(),
        b".."
    );
}

#[test]
fn hook_turn_complete_requires_a_managed_state_dir() {
    let error = run_turn_complete_hook(&|_| None, "").unwrap_err();
    assert!(error.to_string().contains(tmux::ENV_STATE_DIR), "{error:#}");
}

/// A real TraeX `Stop` payload (traex 0.209.1), trimmed: `session_id` is the THREAD id, and
/// `thread_name` is the `--session-id` pmtui gave it, which `traex resume` cannot open.
const TRAE_STOP_PAYLOAD: &str = r#"{"session_id":"01a133c3-b12f-7033-ae77-38f3294fb68e","thread_name":"40d6555b-b1aa-45f8-9d23-20594c062543","session_name":"40d6555b-b1aa-45f8-9d23-20594c062543","hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"ok"}"#;

#[test]
fn hook_turn_complete_records_the_thread_id_trae_reported() {
    let dir = tempfile::tempdir().unwrap();
    let paths = agent_manager::state::ProjectPaths::for_session(dir.path(), "bot");
    let state_dir = paths.state_dir();
    let env =
        |key: &str| (key == tmux::ENV_STATE_DIR).then(|| state_dir.to_string_lossy().into_owned());

    run_turn_complete_hook(&env, TRAE_STOP_PAYLOAD).unwrap();

    assert_eq!(
        agent_manager::state::codex_identity::read(&paths).as_deref(),
        Some("01a133c3-b12f-7033-ae77-38f3294fb68e"),
        "the id lands where pmd and pmtui read it"
    );
    assert_eq!(std::fs::read(paths.turn_signal()).unwrap(), b".");
}

#[test]
fn hook_turn_complete_signals_the_turn_without_a_usable_id() {
    let dir = tempfile::tempdir().unwrap();
    let paths = agent_manager::state::ProjectPaths::for_session(dir.path(), "bot");
    let state_dir = paths.state_dir();
    let env =
        |key: &str| (key == tmux::ENV_STATE_DIR).then(|| state_dir.to_string_lossy().into_owned());

    for payload in [
        "",
        "not json",
        r#"{"session_id":"not-a-uuid"}"#,
        r#"{"thread_name":"x"}"#,
    ] {
        run_turn_complete_hook(&env, payload).unwrap();
    }

    assert_eq!(agent_manager::state::codex_identity::read(&paths), None);
    assert_eq!(std::fs::read(paths.turn_signal()).unwrap(), b"....");
}
