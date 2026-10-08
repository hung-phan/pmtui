use std::fs;
use std::path::Path;

use serde::Serialize;
use serde::ser::{Error as _, Serializer};
use tempfile::tempdir;

use super::super::*;

struct SerializationFailure;

impl Serialize for SerializationFailure {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(S::Error::custom("intentional serialization failure"))
    }
}

fn temp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp."))
        .collect()
}

#[test]
fn write_text_atomic_replaces_whole_content_without_temp_files() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nested/brief.md");
    let body = "ship the thing\n\n- keep tests green".repeat(500);

    write_text_atomic(&path, &body).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), body);

    write_text_atomic(&path, "shorter").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "shorter");
    assert!(temp_files(path.parent().unwrap()).is_empty());
}

#[test]
fn write_json_atomic_writes_pretty_json_with_one_trailing_newline() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("value.json");

    write_json_atomic(&path, &serde_json::json!({"enabled": true})).unwrap();

    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "{\n  \"enabled\": true\n}\n"
    );
}

#[test]
fn write_json_atomic_does_not_create_a_file_when_serialization_fails() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("value.json");

    let error = write_json_atomic(&path, &SerializationFailure).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("intentional serialization failure")
    );
    assert!(!path.exists());
}

#[test]
fn write_atomic_rejects_a_path_without_a_parent() {
    let error = write_atomic(Path::new("/"), b"value").unwrap_err();

    assert_eq!(error.to_string(), "path has no parent directory");
}

#[test]
fn write_atomic_reports_when_the_parent_cannot_be_created() {
    let dir = tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, "not a directory").unwrap();
    let path = blocker.join("value.json");

    let error = write_atomic(&path, b"value").unwrap_err();

    assert!(
        error
            .to_string()
            .contains(&format!("create_dir_all {}", blocker.display()))
    );
}

#[test]
fn write_atomic_rejects_a_path_without_a_file_name() {
    let dir = tempdir().unwrap();
    let parent = dir.path().join("nested");
    let path = parent.join("..");

    let error = write_atomic(&path, b"value").unwrap_err();

    assert_eq!(error.to_string(), "path has no file name");
}

#[test]
fn failed_replacement_removes_the_staged_temp_file() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target");
    fs::create_dir(&target).unwrap();

    let error = write_atomic(&target, b"value").unwrap_err();

    assert!(error.to_string().contains("rename into"));
    assert!(target.is_dir());
    assert!(temp_files(dir.path()).is_empty());
}

#[test]
fn staging_reports_when_the_temp_file_cannot_be_created() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target");
    let temp = dir.path().join("missing/temp");

    let error = super::super::atomic::stage_atomic(&target, b"value", &temp).unwrap_err();

    assert!(error.to_string().contains("create temp"));
}

#[cfg(unix)]
#[test]
fn staging_never_writes_through_a_symlink_planted_at_the_temp_name() {
    use std::os::unix::fs::symlink;

    // Session state dirs are agent-writable, so an agent can plant a link at a predictable temp
    // name. The dashboard must refuse it rather than overwrite the link's target.
    let dir = tempdir().unwrap();
    let victim = dir.path().join("registry.json");
    fs::write(&victim, b"precious").unwrap();
    let target = dir.path().join("target");
    let temp = dir.path().join("planted-temp");
    symlink(&victim, &temp).unwrap();

    let error = super::super::atomic::stage_atomic(&target, b"value", &temp).unwrap_err();

    assert!(error.to_string().contains("create temp"), "{error:#}");
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
    assert!(!target.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn staging_reports_a_write_failure() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target");
    let temp = dir.path().join("forced-temp");
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();

    let error = super::super::atomic::commit_temp(full, &target, b"value", &temp).unwrap_err();

    assert!(error.to_string().contains("write temp"));
}

#[cfg(unix)]
#[test]
fn staging_reports_an_fsync_failure() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target");
    let temp = dir.path().join("forced-temp");
    let null = fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();

    let error = super::super::atomic::commit_temp(null, &target, b"value", &temp).unwrap_err();

    assert!(error.to_string().contains("fsync temp"));
}

#[test]
fn read_json_round_trips_valid_json() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("value.json");
    fs::write(&path, r#"{"value":7}"#).unwrap();

    let value: serde_json::Value = read_json(&path).unwrap();

    assert_eq!(value, serde_json::json!({"value": 7}));
}

#[test]
fn read_json_adds_path_context_to_read_failures() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("missing.json");

    let error = read_json::<serde_json::Value>(&path).unwrap_err();

    assert_eq!(error.to_string(), format!("read {}", path.display()));
}

#[test]
fn read_json_adds_path_context_to_parse_failures() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("invalid.json");
    fs::write(&path, "{").unwrap();

    let error = read_json::<serde_json::Value>(&path).unwrap_err();

    assert_eq!(error.to_string(), format!("parse {}", path.display()));
}

#[test]
fn read_json_or_reads_a_present_value() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("value.json");
    fs::write(&path, "7").unwrap();

    assert_eq!(read_json_or(&path, 11_u64).unwrap(), 7);
}

#[test]
fn read_json_or_returns_the_supplied_default_when_absent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("missing.json");

    assert_eq!(read_json_or(&path, 11_u64).unwrap(), 11);
}

#[test]
fn read_json_or_adds_path_context_to_parse_failures() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("invalid.json");
    fs::write(&path, "not-json").unwrap();

    let error = read_json_or(&path, 11_u64).unwrap_err();

    assert_eq!(error.to_string(), format!("parse {}", path.display()));
}

#[test]
fn read_json_or_does_not_treat_other_io_errors_as_absent() {
    let dir = tempdir().unwrap();

    let error = read_json_or(dir.path(), 11_u64).unwrap_err();

    assert_eq!(error.to_string(), format!("read {}", dir.path().display()));
}

#[test]
fn read_json_opt_reads_a_present_value() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("value.json");
    fs::write(&path, "7").unwrap();

    assert_eq!(read_json_opt(&path).unwrap(), Some(7_u64));
}

#[test]
fn read_json_opt_returns_none_when_absent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("missing.json");

    assert_eq!(read_json_opt::<u64>(&path).unwrap(), None);
}

#[test]
fn read_json_opt_adds_path_context_to_parse_failures() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("invalid.json");
    fs::write(&path, "not-json").unwrap();

    let error = read_json_opt::<u64>(&path).unwrap_err();

    assert_eq!(error.to_string(), format!("parse {}", path.display()));
}

#[test]
fn read_json_opt_does_not_treat_other_io_errors_as_absent() {
    let dir = tempdir().unwrap();

    let error = read_json_opt::<u64>(dir.path()).unwrap_err();

    assert_eq!(error.to_string(), format!("read {}", dir.path().display()));
}
