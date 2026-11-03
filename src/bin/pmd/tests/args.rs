use super::*;

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn parsed_args(values: &[&str]) -> Args {
    match parse_args(argv(values)).unwrap() {
        CliAction::Run(args) => args,
        other => panic!("expected runnable arguments, got {other:?}"),
    }
}

#[test]
fn empty_command_line_uses_documented_defaults() {
    let args = parsed_args(&[]);

    assert_eq!(args.registry, default_registry_path());
    assert_eq!(args.socket, "pmd");
    assert_eq!(args.tick_ms, 500);
    assert!(!args.once);
}

#[test]
fn every_runtime_option_is_parsed_without_reordering() {
    let args = parsed_args(&[
        "--tick-ms",
        "17",
        "--once",
        "--socket",
        "private-socket",
        "--registry",
        "/tmp/custom-registry.json",
    ]);

    assert_eq!(args.registry, PathBuf::from("/tmp/custom-registry.json"));
    assert_eq!(args.socket, "private-socket");
    assert_eq!(args.tick_ms, 17);
    assert!(args.once);
}

#[test]
fn value_options_reject_a_missing_value() {
    for flag in ["--registry", "--socket", "--tick-ms"] {
        let error = parse_args(argv(&[flag])).unwrap_err();
        assert_eq!(error.to_string(), format!("{flag} needs a value"));
    }
}

#[test]
fn tick_interval_must_be_an_unsigned_integer() {
    for value in ["later", "-1"] {
        let error = parse_args(argv(&["--tick-ms", value])).unwrap_err();
        assert!(
            format!("{error:#}").contains("--tick-ms must be a number"),
            "unexpected error for {value:?}: {error:#}"
        );
    }
}

#[test]
fn help_flags_request_a_successful_help_exit() {
    for flag in ["-h", "--help"] {
        assert_eq!(parse_args(argv(&[flag])).unwrap(), CliAction::Help);
    }
}

#[test]
fn help_takes_effect_before_trailing_arguments() {
    assert_eq!(
        parse_args(argv(&["--once", "--help", "--unknown"])).unwrap(),
        CliAction::Help
    );
}

#[test]
fn unknown_argument_preserves_the_debug_quoted_diagnostic() {
    assert_eq!(
        parse_args(argv(&["--wat"])).unwrap(),
        CliAction::UsageError("pmd: unknown argument \"--wat\"".to_string())
    );
}

#[test]
fn dispatch_returns_the_cli_exit_codes_for_help_and_usage_errors() {
    assert_eq!(dispatch(CliAction::Help).unwrap(), 0);
    assert_eq!(
        dispatch(CliAction::UsageError("bad option".to_string())).unwrap(),
        2
    );
}

#[test]
fn entrypoint_composes_parsing_and_dispatch() {
    assert_eq!(entrypoint(argv(&["--help"])).unwrap(), 0);
    assert_eq!(entrypoint(argv(&["--invalid"])).unwrap(), 2);
    assert_eq!(
        entrypoint(argv(&["--tick-ms"])).unwrap_err().to_string(),
        "--tick-ms needs a value"
    );
}

#[test]
fn help_text_documents_the_complete_cli_contract() {
    assert!(HELP.starts_with("pmd — project-manager daemon\n\nUsage: pmd "));
    for option in [
        "checkpoint",
        "--registry",
        "--socket",
        "--tick-ms",
        "--once",
    ] {
        assert!(HELP.contains(option), "help omitted {option}");
    }
    assert!(HELP.ends_with('\n'));
}

#[test]
fn the_notification_transports_default_to_the_documented_set() {
    assert_eq!(parsed_args(&[]).notify, DEFAULT_TRANSPORTS);
}

#[test]
fn a_notify_list_replaces_the_default_set_in_order() {
    assert_eq!(
        parsed_args(&["--notify", "log,cmux,osc"]).notify,
        [
            Transport::Log,
            Transport::Program(agent_manager::escalation::Wire::Cmux),
            Transport::Osc
        ]
    );
}

/// Naming an unknown transport is a usage error that says WHICH name was unknown —
/// a silently dropped transport would make every missing notification look like that
/// transport failing.
#[test]
fn an_unknown_transport_is_a_usage_error_naming_itself() {
    let error = parse_args(argv(&["--notify", "log,bogus"])).unwrap_err();
    assert!(
        format!("{error:#}").contains("--notify: unknown transport `bogus`"),
        "unexpected error: {error:#}"
    );
}

/// Every name the help text offers must parse, or the help lies.
#[test]
fn every_transport_the_help_text_lists_is_accepted() {
    for transport in Transport::ALL {
        let name = transport.name();
        assert!(
            HELP.contains(name),
            "`{name}` is a transport the help text never mentions"
        );
        assert_eq!(parsed_args(&["--notify", name]).notify, [*transport]);
    }
}
