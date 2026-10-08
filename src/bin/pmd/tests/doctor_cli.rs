use std::path::PathBuf;

use super::super::*;

fn doctor(values: &[&str]) -> DoctorArgs {
    match parse_args(values.iter().map(|value| (*value).to_string())).unwrap() {
        CliAction::Doctor(args) => args,
        other => panic!("expected doctor arguments, got {other:?}"),
    }
}

#[test]
fn doctor_uses_daemon_defaults_and_accepts_read_only_options() {
    let defaults = doctor(&["doctor"]);
    assert_eq!(defaults.registry, default_registry_path());
    assert_eq!(defaults.socket, "pmd");
    assert!(!defaults.json);

    let explicit = doctor(&[
        "doctor",
        "--json",
        "--socket",
        "health-check",
        "--registry",
        "/tmp/registry.json",
    ]);
    assert_eq!(explicit.registry, PathBuf::from("/tmp/registry.json"));
    assert_eq!(explicit.socket, "health-check");
    assert!(explicit.json);
}

#[test]
fn doctor_rejects_daemon_flags_and_unknown_options() {
    for option in ["--once", "--tick-ms", "--unknown"] {
        let mut values = vec!["doctor", option];
        if option == "--tick-ms" {
            values.push("10");
        }
        assert!(matches!(
            parse_args(values.into_iter().map(str::to_owned)).unwrap(),
            CliAction::UsageError(message)
                if message.contains("pmd doctor") && message.contains(option)
        ));
    }
}

#[test]
fn doctor_requires_option_values_and_honors_help() {
    for option in ["--registry", "--socket"] {
        let action = parse_args(["doctor".into(), option.into()]).unwrap();
        assert!(matches!(
            &action,
            CliAction::UsageError(message)
                if message == &format!("pmd doctor: {option} requires a value")
        ));
        assert_eq!(dispatch(action).unwrap(), 2);
    }
    assert_eq!(
        parse_args(["doctor".into(), "--help".into()]).unwrap(),
        CliAction::Help
    );
}

#[test]
fn version_is_a_top_level_action() {
    assert_eq!(
        parse_args(["--version".into()]).unwrap(),
        CliAction::Version
    );
    assert_eq!(dispatch(CliAction::Version).unwrap(), 0);
}

#[test]
fn help_documents_doctor_version_json_and_exit_statuses() {
    for text in [
        "pmd doctor",
        "--json",
        "--version",
        "warnings still exit 0",
        "failures exit 1",
    ] {
        assert!(HELP.contains(text), "help omitted {text}");
    }
}

#[test]
fn doctor_dispatch_propagates_failure_for_text_and_json() {
    let dir = tempfile::TempDir::new().unwrap();
    let registry = dir.path().join("registry.json");
    std::fs::write(&registry, b"{broken").unwrap();
    for json in [false, true] {
        assert_eq!(
            dispatch(CliAction::Doctor(DoctorArgs {
                registry: registry.clone(),
                socket: format!("doctor-dispatch-{}", std::process::id()),
                json,
            }))
            .unwrap(),
            1
        );
    }
}
