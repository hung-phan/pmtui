use crate::models::{ModelInfo, trae};
use crate::registry::Engine;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const DISCOVERY_EXPECTATION: &str = "PM_TEST_TRAE_DISCOVERY_EXPECTATION";

fn write_trae_stub(directory: &Path, script: &str) {
    let path = directory.join("traex");
    std::fs::write(&path, script).expect("write TRAE stub");
    let mut permissions = std::fs::metadata(&path)
        .expect("read TRAE stub metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("make TRAE stub executable");
}

fn run_discovery_probe(path: &Path, expectation: &str) {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "models::tests::trae::discovery_probe",
            "--nocapture",
        ])
        .env(DISCOVERY_EXPECTATION, expectation)
        .env("PATH", path)
        .output()
        .expect("run TRAE discovery probe");
    assert!(
        output.status.success(),
        "probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parses_models_from_json_array() {
    let got = trae::parse(
        r#"[
            {"name":"seed-1","real_name":"Seed 1","description":"fast","context_window":200000},
            {"name":"seed-2","config_name":"Seed Two","description":"large","context_window":1000000},
            {"name":"","real_name":"Missing"},
            {"description":"Missing name"}
        ]"#,
    );
    assert_eq!(
        got,
        vec![
            ModelInfo {
                label: "Seed 1 - fast".into(),
                value: "seed-1".into(),
            },
            ModelInfo {
                label: "Seed Two - large".into(),
                value: "seed-2".into(),
            },
        ]
    );
}

#[test]
fn blank_display_metadata_keeps_a_usable_label_and_the_launch_name() {
    // Optional display metadata must not hide a usable model or change its launch value.
    let got = trae::parse(
        r#"[
            {"name":"seed-default"},
            {"name":"seed-blank","config_name":" \t","real_name":"\n","description":" "},
            {"name":"seed-real","config_name":" ","real_name":"Seed Real","description":"\t"},
            {"name":"seed-config","config_name":"Seed Config","real_name":"Internal Name"},
            {"name":"seed-described","description":"fast"},
            {"name":" \t","config_name":"Not launchable"}
        ]"#,
    );
    assert_eq!(
        got,
        vec![
            ModelInfo {
                label: "seed-default".into(),
                value: "seed-default".into(),
            },
            ModelInfo {
                label: "seed-blank".into(),
                value: "seed-blank".into(),
            },
            ModelInfo {
                label: "Seed Real".into(),
                value: "seed-real".into(),
            },
            ModelInfo {
                label: "Seed Config".into(),
                value: "seed-config".into(),
            },
            ModelInfo {
                label: "seed-described - fast".into(),
                value: "seed-described".into(),
            },
        ]
    );
}

#[test]
fn empty_on_malformed() {
    assert!(trae::parse("nonsense").is_empty());
    assert!(trae::parse(r#"{"models":[]}"#).is_empty());
}

#[test]
fn discovers_models_from_successful_command() {
    let bin = tempfile::tempdir().expect("temporary TRAE bin directory");
    write_trae_stub(
        bin.path(),
        "#!/bin/sh\nprintf '%s\\n' '[{\"name\":\"seed-1\",\"real_name\":\"Seed 1\",\"description\":\"fast\",\"context_window\":200000},{\"name\":\"seed-2\",\"config_name\":\"Seed Two\",\"description\":\"large\",\"context_window\":1000000}]'\n",
    );

    run_discovery_probe(bin.path(), "configured");
}

#[test]
fn discovery_is_empty_for_malformed_or_failed_command() {
    let bin = tempfile::tempdir().expect("temporary TRAE bin directory");
    write_trae_stub(bin.path(), "#!/bin/sh\nprintf '%s\\n' 'not json'\n");
    run_discovery_probe(bin.path(), "empty");

    let failed = tempfile::tempdir().expect("temporary TRAE bin directory");
    write_trae_stub(failed.path(), "#!/bin/sh\nexit 7\n");
    run_discovery_probe(failed.path(), "empty");

    let missing = tempfile::tempdir().expect("empty TRAE bin directory");
    run_discovery_probe(missing.path(), "empty");
}

#[test]
fn discovery_probe() {
    let Ok(expectation) = std::env::var(DISCOVERY_EXPECTATION) else {
        return;
    };
    let got = crate::models::available_models(Engine::Trae);
    match expectation.as_str() {
        "configured" => assert_eq!(
            got,
            vec![
                ModelInfo {
                    label: "Seed 1 - fast".into(),
                    value: "seed-1".into(),
                },
                ModelInfo {
                    label: "Seed Two - large".into(),
                    value: "seed-2".into(),
                },
            ]
        ),
        "empty" => assert!(got.is_empty(), "unexpected models: {got:?}"),
        other => panic!("unknown discovery expectation: {other}"),
    }
}
