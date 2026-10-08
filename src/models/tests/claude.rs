use crate::models::{ModelInfo, claude};
use crate::registry::Engine;
use std::path::Path;
use std::process::Command;

const DISCOVERY_EXPECTATION: &str = "PM_TEST_CLAUDE_DISCOVERY_EXPECTATION";
const PROVIDER_ENVS: [(&str, &str); 6] = [
    ("CLAUDE_CODE_USE_BEDROCK", "1"),
    ("CLAUDE_CODE_USE_VERTEX", "1"),
    ("CLAUDE_CODE_USE_FOUNDRY", "1"),
    ("CLAUDE_CODE_USE_MANTLE", "1"),
    ("CLAUDE_CODE_USE_ANTHROPIC_AWS", "1"),
    ("ANTHROPIC_BASE_URL", "https://gateway.example"),
];

fn run_discovery_probe(claude_config_dir: Option<&Path>, home: Option<&Path>, expectation: &str) {
    run_discovery_probe_with_provider(claude_config_dir, home, None, expectation);
}

fn run_discovery_probe_with_provider(
    claude_config_dir: Option<&Path>,
    home: Option<&Path>,
    provider_env: Option<(&str, &str)>,
    expectation: &str,
) {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .args([
            "--exact",
            "models::tests::claude::discovery_probe",
            "--nocapture",
        ])
        .env(DISCOVERY_EXPECTATION, expectation)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("HOME");
    for (name, _) in PROVIDER_ENVS {
        command.env_remove(name);
    }
    if let Some(path) = claude_config_dir {
        command.env("CLAUDE_CONFIG_DIR", path);
    }
    if let Some(path) = home {
        command.env("HOME", path);
    }
    if let Some((name, value)) = provider_env {
        command.env(name, value);
    }

    let output = command.output().expect("run Claude discovery probe");
    assert!(
        output.status.success(),
        "probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn subscription_aliases() -> Vec<ModelInfo> {
    ["sonnet", "opus", "haiku"]
        .into_iter()
        .map(|alias| ModelInfo {
            label: alias.into(),
            value: alias.into(),
        })
        .collect()
}

#[test]
fn parses_slugs_and_resolves_overrides() {
    let settings = r#"{
        "availableModels": ["claude-opus-4-8[1m]", "claude-sonnet-5", "claude-no-override"],
        "modelOverrides": {
            "claude-opus-4-8[1m]": "global.anthropic.claude-opus-4-8[1m]",
            "claude-sonnet-5": "global.anthropic.claude-sonnet-5"
        }
    }"#;
    let got = claude::parse(settings);
    assert_eq!(
        got,
        vec![
            ModelInfo {
                label: "claude-opus-4-8[1m]".into(),
                value: "global.anthropic.claude-opus-4-8[1m]".into()
            },
            ModelInfo {
                label: "claude-sonnet-5".into(),
                value: "global.anthropic.claude-sonnet-5".into()
            },
            // no override -> value falls back to the slug itself
            ModelInfo {
                label: "claude-no-override".into(),
                value: "claude-no-override".into()
            },
        ]
    );
}

#[test]
fn malformed_settings_are_empty() {
    assert!(claude::parse("not json").is_empty());
}

#[test]
fn missing_allowlist_uses_subscription_aliases() {
    assert_eq!(claude::parse("{}"), subscription_aliases());
}

#[test]
fn explicit_empty_allowlist_stays_empty() {
    assert!(
        claude::parse(r#"{"availableModels": []}"#).is_empty(),
        "an explicit provider allowlist remains authoritative"
    );
}

#[test]
fn null_allowlist_is_invalid_not_absent() {
    assert!(
        claude::parse(r#"{"availableModels": null}"#).is_empty(),
        "an invalid explicit allowlist must not enable fallback aliases"
    );
}

#[test]
fn provider_declared_in_settings_requires_an_explicit_allowlist() {
    for (name, value) in PROVIDER_ENVS {
        let provider_settings = format!(r#"{{"env": {{"{name}": "{value}"}}}}"#);
        assert!(
            claude::parse(&provider_settings).is_empty(),
            "{name} must disable inferred aliases"
        );
    }

    let configured = claude::parse(
        r#"{
            "env": {"CLAUDE_CODE_USE_BEDROCK": "1"},
            "availableModels": ["sonnet"],
            "modelOverrides": {"sonnet": "global.anthropic.sonnet"}
        }"#,
    );
    assert_eq!(
        configured,
        vec![ModelInfo {
            label: "sonnet".into(),
            value: "global.anthropic.sonnet".into(),
        }]
    );
}

#[test]
fn subscription_aliases_resolve_configured_overrides() {
    let got = claude::parse(
        r#"{
            "modelOverrides": {
                "sonnet": "global.anthropic.sonnet",
                "opus": "global.anthropic.opus"
            }
        }"#,
    );
    assert_eq!(
        got,
        vec![
            ModelInfo {
                label: "sonnet".into(),
                value: "global.anthropic.sonnet".into(),
            },
            ModelInfo {
                label: "opus".into(),
                value: "global.anthropic.opus".into(),
            },
            ModelInfo {
                label: "haiku".into(),
                value: "haiku".into(),
            },
        ]
    );
}

#[test]
fn drops_empty_value_and_empty_slug_rows() {
    // An availableModels entry mapped to `""` in modelOverrides, and one with an empty
    // slug, both yield NO row (a blank `--model` value is "no model set", not a pick).
    // Valid entries around them are unaffected.
    let settings = r#"{
        "availableModels": ["claude-real", "claude-blank-override", ""],
        "modelOverrides": {
            "claude-real": "global.anthropic.claude-real",
            "claude-blank-override": ""
        }
    }"#;
    let got = claude::parse(settings);
    assert_eq!(
        got,
        vec![ModelInfo {
            label: "claude-real".into(),
            value: "global.anthropic.claude-real".into()
        }],
        "only the valid entry survives: {got:?}"
    );
}

#[test]
fn discovers_models_from_claude_config_dir() {
    let config = tempfile::tempdir().expect("temporary Claude config");
    std::fs::write(
        config.path().join("settings.json"),
        r#"{
            "availableModels": ["sonnet"],
            "modelOverrides": {"sonnet": "global.anthropic.sonnet"}
        }"#,
    )
    .expect("write Claude settings");

    run_discovery_probe(Some(config.path()), None, "configured");
}

#[test]
fn blank_config_dir_falls_back_to_home() {
    let home = tempfile::tempdir().expect("temporary home");
    let config = home.path().join(".claude");
    std::fs::create_dir(&config).expect("create fallback Claude config");
    std::fs::write(
        config.join("settings.json"),
        r#"{"availableModels":["sonnet"]}"#,
    )
    .expect("write fallback Claude settings");

    run_discovery_probe(Some(Path::new("   ")), Some(home.path()), "fallback");
}

#[test]
fn discovery_uses_subscription_aliases_without_config_location() {
    run_discovery_probe(None, None, "subscription");
}

#[test]
fn discovery_falls_back_only_for_missing_settings() {
    let config = tempfile::tempdir().expect("temporary Claude config");
    run_discovery_probe(Some(config.path()), None, "subscription");

    std::fs::write(config.path().join("settings.json"), "not json")
        .expect("write malformed Claude settings");
    run_discovery_probe(Some(config.path()), None, "empty");
}

#[test]
fn provider_backends_require_an_explicit_allowlist() {
    let config = tempfile::tempdir().expect("temporary Claude config");
    for (name, value) in PROVIDER_ENVS {
        run_discovery_probe_with_provider(Some(config.path()), None, Some((name, value)), "empty");
    }

    std::fs::write(
        config.path().join("settings.json"),
        r#"{
            "availableModels": ["sonnet"],
            "modelOverrides": {"sonnet": "global.anthropic.sonnet"}
        }"#,
    )
    .expect("write provider settings");
    run_discovery_probe_with_provider(
        Some(config.path()),
        None,
        Some(("CLAUDE_CODE_USE_BEDROCK", "1")),
        "configured",
    );
}

#[test]
fn unreadable_settings_path_fails_closed() {
    let config = tempfile::tempdir().expect("temporary Claude config");
    std::fs::create_dir(config.path().join("settings.json"))
        .expect("create non-file settings path");
    run_discovery_probe(Some(config.path()), None, "empty");
}

#[test]
fn discovery_probe() {
    let Ok(expectation) = std::env::var(DISCOVERY_EXPECTATION) else {
        return;
    };
    let got = crate::models::available_models(Engine::Claude);
    match expectation.as_str() {
        "configured" => assert_eq!(
            got,
            vec![ModelInfo {
                label: "sonnet".into(),
                value: "global.anthropic.sonnet".into(),
            }]
        ),
        "fallback" => assert_eq!(
            got,
            vec![ModelInfo {
                label: "sonnet".into(),
                value: "sonnet".into(),
            }]
        ),
        "subscription" => assert_eq!(got, subscription_aliases()),
        "empty" => assert!(got.is_empty(), "unexpected models: {got:?}"),
        other => panic!("unknown discovery expectation: {other}"),
    }
}
