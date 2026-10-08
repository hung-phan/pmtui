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
