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

    run_turn_complete_hook(&env).unwrap();
    run_turn_complete_hook(&env).unwrap();

    assert_eq!(
        std::fs::read(state_dir.join(".daemon/turn-complete")).unwrap(),
        b".."
    );
}

#[test]
fn hook_turn_complete_requires_a_managed_state_dir() {
    let error = run_turn_complete_hook(&|_| None).unwrap_err();
    assert!(error.to_string().contains(tmux::ENV_STATE_DIR), "{error:#}");
}
