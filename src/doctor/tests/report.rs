use std::path::PathBuf;

use super::super::{Check, CheckStatus, DoctorReport, OverallStatus, write_report};

fn report(checks: Vec<Check>) -> DoctorReport {
    DoctorReport::new(
        PathBuf::from("/tmp/registry.json"),
        "doctor-test".into(),
        checks,
    )
}

#[test]
fn report_aggregates_severity_and_exit_code() {
    let passing = report(vec![
        Check::pass("version", "pmd 0.1.0"),
        Check::skip("tmux.sessions", "tmux unavailable"),
    ]);
    assert_eq!(passing.status, OverallStatus::Pass);
    assert_eq!(passing.exit_code(), 0);
    assert_eq!(passing.summary.pass, 1);
    assert_eq!(passing.summary.skip, 1);

    let warning = report(vec![Check::warn("registry", "registry is absent")]);
    assert_eq!(warning.status, OverallStatus::Warn);
    assert_eq!(warning.exit_code(), 0);

    let failing = report(vec![
        Check::warn("notify", "notify-send is absent"),
        Check::fail("worker.claude", "claude is absent"),
    ]);
    assert_eq!(failing.status, OverallStatus::Fail);
    assert_eq!(failing.exit_code(), 1);
    assert_eq!(failing.summary.warn, 1);
    assert_eq!(failing.summary.fail, 1);
}

#[test]
fn report_renders_stable_json_and_readable_text() -> Result<(), serde_json::Error> {
    let report = report(vec![
        Check::pass("version", "pmd 0.1.0"),
        Check::warn("registry", "registry is absent")
            .with_detail("/tmp/registry.json")
            .with_remediation("create a project in pmtui"),
    ]);

    let json: serde_json::Value = serde_json::from_str(&report.render_json()?)?;
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["pmd_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["status"], "warn");
    assert_eq!(json["registry"], "/tmp/registry.json");
    assert_eq!(json["socket"], "doctor-test");
    assert_eq!(json["summary"]["pass"], 1);
    assert_eq!(json["summary"]["warn"], 1);
    assert_eq!(json["checks"][1]["status"], "warn");
    assert_eq!(json["checks"][1]["detail"], "/tmp/registry.json");
    assert_eq!(
        json["checks"][1]["remediation"],
        "create a project in pmtui"
    );

    let text = report.render_text();
    assert!(text.contains("pmd doctor: WARN"));
    assert!(text.contains("[PASS] version: pmd 0.1.0"));
    assert!(text.contains("[WARN] registry: registry is absent"));
    assert!(text.contains("recovery: create a project in pmtui"));
    assert!(text.contains("1 passed, 1 warning, 0 failed, 0 skipped"));
    assert!(
        DoctorReport::new(
            PathBuf::from("/tmp/registry.json"),
            "doctor-test".into(),
            vec![Check::pass("version", "ok")],
        )
        .render_text()
        .contains("pmd doctor: PASS")
    );
    Ok(())
}

#[test]
fn status_wire_labels_cover_every_check_state() {
    assert_eq!(CheckStatus::Pass.to_string(), "PASS");
    assert_eq!(CheckStatus::Warn.to_string(), "WARN");
    assert_eq!(CheckStatus::Fail.to_string(), "FAIL");
    assert_eq!(CheckStatus::Skip.to_string(), "SKIP");
}

#[test]
fn command_output_is_injectable_and_write_failures_are_contextual() -> anyhow::Result<()> {
    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let report = report(vec![Check::warn("registry", "missing")]);
    let mut text = Vec::new();
    assert_eq!(write_report(&report, false, &mut text)?, 0);
    assert!(String::from_utf8(text)?.contains("pmd doctor: WARN"));

    let mut json = Vec::new();
    assert_eq!(write_report(&report, true, &mut json)?, 0);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&json)?["status"],
        "warn"
    );

    let error = write_report(&report, false, &mut FailingWriter).unwrap_err();
    assert!(format!("{error:#}").contains("write doctor report"));
    Ok(())
}
