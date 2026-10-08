use crate::models::{ModelInfo, codex};
use crate::registry::Engine;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const DISCOVERY_EXPECTATION: &str = "PM_TEST_CODEX_DISCOVERY_EXPECTATION";

fn write_codex_stub(directory: &Path, script: &str) {
    let path = directory.join("codex");
    std::fs::write(&path, script).expect("write Codex stub");
    let mut permissions = std::fs::metadata(&path)
        .expect("read Codex stub metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("make Codex stub executable");
}

fn run_discovery_probe(path: &Path, expectation: &str) {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "models::tests::codex::discovery_probe",
            "--nocapture",
        ])
        .env(DISCOVERY_EXPECTATION, expectation)
        .env("PATH", path)
        .output()
        .expect("run Codex discovery probe");
    assert!(
        output.status.success(),
        "probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parses_visible_models_only() {
    let json = r#"{"models":[
        {"slug":"openai.gpt-5.6-sol","display_name":"GPT-5.6 Sol","visibility":"list","supported_in_api":true},
        {"slug":"hidden-one","display_name":"Hidden","visibility":"hidden","supported_in_api":true},
        {"slug":"no-name","display_name":"","visibility":"list"},
        {"slug":"unsupported","display_name":"Unsupported","visibility":"list","supported_in_api":false},
        {"slug":"","display_name":"Missing slug","visibility":"list"}
    ]}"#;
    let got = codex::parse(json);
    assert_eq!(
        got,
        vec![
            ModelInfo {
                label: "GPT-5.6 Sol".into(),
                value: "openai.gpt-5.6-sol".into()
            },
            // empty display_name falls back to the slug
            ModelInfo {
                label: "no-name".into(),
                value: "no-name".into()
            },
        ]
    );
}

#[test]
fn empty_on_malformed() {
    assert!(codex::parse("nonsense").is_empty());
    assert!(codex::parse(r#"{"models":[]}"#).is_empty());
}

#[test]
fn discovers_models_from_successful_command() {
    let bin = tempfile::tempdir().expect("temporary Codex bin directory");
    write_codex_stub(
        bin.path(),
        "#!/bin/sh\nprintf '%s\\n' '{\"models\":[{\"slug\":\"gpt-test\",\"display_name\":\"GPT Test\",\"visibility\":\"list\"}]}'\n",
    );

    run_discovery_probe(bin.path(), "configured");
}

#[test]
fn discovery_is_empty_for_malformed_command_output() {
    let bin = tempfile::tempdir().expect("temporary Codex bin directory");
    write_codex_stub(bin.path(), "#!/bin/sh\nprintf '%s\\n' 'not json'\n");

    run_discovery_probe(bin.path(), "empty");
}

#[test]
fn discovery_is_empty_for_failed_or_missing_command() {
    let bin = tempfile::tempdir().expect("temporary Codex bin directory");
    write_codex_stub(bin.path(), "#!/bin/sh\nexit 7\n");
    run_discovery_probe(bin.path(), "empty");

    let empty_bin = tempfile::tempdir().expect("empty Codex bin directory");
    run_discovery_probe(empty_bin.path(), "empty");
}

#[test]
fn discovery_probe() {
    let Ok(expectation) = std::env::var(DISCOVERY_EXPECTATION) else {
        return;
    };
    let got = crate::models::available_models(Engine::Codex);
    match expectation.as_str() {
        "configured" => assert_eq!(
            got,
            vec![ModelInfo {
                label: "GPT Test".into(),
                value: "gpt-test".into(),
            }]
        ),
        "empty" => assert!(got.is_empty(), "unexpected models: {got:?}"),
        other => panic!("unknown discovery expectation: {other}"),
    }
}
