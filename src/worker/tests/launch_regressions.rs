use super::*;
use std::path::Path;

#[test]
fn public_hook_switch_reads_the_same_policy_as_the_engine_specific_entry() {
    // Read the inherited value; mutating process-wide env would race other tests.
    let value = std::env::var("PM_TURN_HOOK").ok();
    let expected = !matches!(value.as_deref().map(str::trim), Some("off" | "0" | "false"));
    assert_eq!(turn_hook_enabled(), expected);
    assert_eq!(turn_hook_enabled_for(Engine::Claude), expected);
    assert_eq!(turn_hook_enabled_for(Engine::Codex), expected);
    assert_eq!(turn_hook_enabled_for(Engine::Trae), expected);
}

#[test]
fn unnamed_fresh_exec_keeps_extra_directories_before_the_literal_prompt() {
    let dirs = [
        PathBuf::from("/workspace/a b"),
        PathBuf::from("/workspace/o'hare"),
    ];
    for (engine, executable, sandbox) in [
        (Engine::Codex, "codex", "-s"),
        (Engine::Trae, "traex", "--sandbox"),
    ] {
        for prompt in ["--help", "resume", "review\nthis change"] {
            let argv = build_command(
                engine,
                prompt,
                PermissionMode::Auto,
                &dirs,
                Resume::Fresh { session_id: None },
            );
            assert_eq!(
                argv,
                [
                    executable,
                    "exec",
                    sandbox,
                    "workspace-write",
                    "--skip-git-repo-check",
                    "--json",
                    "--add-dir",
                    "/workspace/a b",
                    "--add-dir",
                    "/workspace/o'hare",
                    "--",
                    prompt,
                ],
                "{engine:?}"
            );
        }
    }
}

#[test]
fn trae_named_exec_adds_directories_but_resume_uses_the_recorded_sandbox() {
    let dirs = [PathBuf::from("/workspace/extra")];
    let fresh = build_command(
        Engine::Trae,
        "continue work",
        PermissionMode::Auto,
        &dirs,
        Resume::Fresh {
            session_id: Some("job-name".into()),
        },
    );
    assert_eq!(
        fresh,
        [
            "traex",
            "exec",
            "--sandbox",
            "workspace-write",
            "--skip-git-repo-check",
            "--json",
            "--session-id",
            "job-name",
            "--add-dir",
            "/workspace/extra",
            "--",
            "continue work",
        ]
    );
    let resumed = build_command(
        Engine::Trae,
        "continue work",
        PermissionMode::Auto,
        &dirs,
        Resume::Continue("engine-thread-id".into()),
    );
    assert_eq!(
        resumed,
        [
            "traex",
            "exec",
            "resume",
            "engine-thread-id",
            "--skip-git-repo-check",
            "--json",
            "--",
            "continue work",
        ]
    );
}

#[test]
fn trae_unnamed_interactive_launches_do_not_invent_an_identity_or_install_disabled_hooks() {
    for model in [None, Some(""), Some(" \t ")] {
        let resume = Resume::Fresh { session_id: None };
        assert_eq!(
            build_standard_command(Engine::Trae, &resume, None, model),
            ["traex"]
        );
        assert_eq!(
            build_loop_command(Engine::Trae, &resume, &[], None, model),
            [
                "env",
                "-u",
                "CLAUDECODE",
                "traex",
                "--permission-mode",
                "auto"
            ]
        );
    }
}

#[test]
fn trae_start_and_stop_hook_commands_stay_stable_across_standard_loop_and_fork() {
    let first = Path::new("/project-one/.daemon/turn-complete");
    let second = Path::new("/project-two/.daemon/turn-complete");
    let resume = Resume::Continue("reported-thread".into());
    let launches = [
        build_standard_command(Engine::Trae, &resume, Some(first), Some("model-a")),
        build_loop_command(Engine::Trae, &resume, &[], Some(second), Some("model-a")),
        build_fork_command(
            Engine::Trae,
            "source-thread",
            Some(second),
            None,
            Some("model-a"),
        ),
    ];
    let mut expected_configs = None;
    for argv in &launches {
        let configs = argv
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].clone())
            .collect::<Vec<_>>();
        assert_eq!(configs.len(), 2, "{argv:?}");
        for (config, event, command) in [
            (&configs[0], "Stop", "turn-complete"),
            (&configs[1], "SessionStart", "session-start"),
        ] {
            assert!(config.starts_with(&format!("hooks.{event}=")), "{config}");
            assert!(config.contains(&format!("hook {command}")), "{config}");
            assert!(config.contains("\\\"$PMTUI_BIN\\\""), "{config}");
            assert!(!config.contains(":-pmtui"), "{config}");
            assert!(config.contains("async=false"), "{config}");
            assert!(config.contains("timeout=5"), "{config}");
            assert!(!config.contains("/project-"), "{config}");
        }
        if let Some(expected) = &expected_configs {
            // Trust is tied to command text, so changing sessions must not change it.
            assert_eq!(&configs, expected);
        } else {
            expected_configs = Some(configs);
        }
        assert!(!argv.iter().any(|arg| matches!(
            arg.as_str(),
            "-y" | "--yolo" | "--dangerously-bypass-hook-trust" | "bypass_permissions"
        )));
    }
    let fork = &launches[2];
    assert_eq!(&fork[fork.len() - 2..], ["fork", "source-thread"]);
    assert!(fork.windows(2).any(|pair| pair == ["-m", "model-a"]));
    assert!(!fork.iter().any(|arg| matches!(
        arg.as_str(),
        "--session-id" | "resume" | "--permission-mode" | "--sandbox"
    )));
}

#[test]
fn claude_fork_only_installs_requested_hooks_and_omits_blank_models() {
    let signal = Path::new("/project/.daemon/turn-complete");
    let identity = Path::new("/project/.daemon/child-id");
    for (turn, sink, expected_events) in [
        (None, None, vec![]),
        (Some(signal), None, vec!["Stop"]),
        (None, Some(identity), vec!["SessionStart"]),
    ] {
        for model in [None, Some(""), Some(" \t ")] {
            let argv = build_fork_command(Engine::Claude, "source", turn, sink, model);
            assert!(!argv.iter().any(|arg| arg == "--model"));
            assert_eq!(
                &argv[argv.len() - 3..],
                ["--resume", "source", "--fork-session"]
            );
            let settings = argv.windows(2).find(|pair| pair[0] == "--settings");
            if expected_events.is_empty() {
                assert!(settings.is_none(), "{argv:?}");
            } else {
                let value: serde_json::Value =
                    serde_json::from_str(&settings.expect("requested hooks")[1]).unwrap();
                let hooks = value["hooks"].as_object().unwrap();
                assert_eq!(
                    hooks.keys().map(String::as_str).collect::<Vec<_>>(),
                    expected_events
                );
            }
        }
    }
}
