use super::super::*;

fn checkpoint() -> WorkerCheckpoint {
    WorkerCheckpoint {
        version: 1,
        seq: 7,
        done: vec!["implemented recovery".into()],
        in_progress: vec!["running integration tests".into()],
        decisions: vec!["preserve the project terminal".into()],
        blockers: Vec::new(),
        activities: vec![CheckpointActivity {
            id: "integration-tests".into(),
            status: CheckpointActivityStatus::Running,
            handle: Some("pid:1234@start:1787800000".into()),
            output_ref: Some("/tmp/integration-tests.log".into()),
            started_unix_s: Some(1787800000),
            deadline_unix_s: Some(1787803600),
        }],
        next: vec!["inspect failures".into()],
        important_files: vec!["src/job_engine/session.rs".into()],
    }
}

fn write(path: &std::path::Path, value: &impl serde::Serialize) {
    write_json_atomic(path, value).unwrap();
}

#[test]
fn checkpoint_round_trips_from_the_agent_owned_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "worker");
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    write(&paths.checkpoint(), &checkpoint());

    assert_eq!(
        read_checkpoint(&paths.checkpoint()).unwrap(),
        Some(checkpoint())
    );
}

#[test]
fn minimal_and_additive_checkpoints_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    std::fs::write(
        &path,
        r#"{"version":1,"seq":1,"future_optional_field":{"value":true}}"#,
    )
    .unwrap();

    let loaded = read_checkpoint(&path).unwrap().unwrap();
    assert_eq!(loaded.seq, 1);
    assert!(loaded.done.is_empty() && loaded.activities.is_empty());
}

#[test]
fn unknown_activity_statuses_remain_forward_compatible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    std::fs::write(
        &path,
        r#"{
          "version": 1,
          "seq": 2,
          "activities": [{
            "id": "future-activity",
            "status": "paused_by_provider",
            "future_detail": true
          }]
        }"#,
    )
    .unwrap();

    let loaded = read_checkpoint(&path).unwrap().unwrap();
    assert_eq!(
        loaded.activities[0].status,
        CheckpointActivityStatus::Unknown
    );
    assert_eq!(loaded.activities[0].status.label(), "unknown");
}

#[test]
fn missing_checkpoint_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        read_checkpoint(&dir.path().join("missing.json")).unwrap(),
        None
    );
}

#[test]
fn unsupported_version_and_malformed_json_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    std::fs::write(&path, r#"{"version":2,"seq":1}"#).unwrap();
    assert!(read_checkpoint(&path).is_err());

    std::fs::write(&path, "{not json").unwrap();
    assert!(read_checkpoint(&path).is_err());
}

#[test]
fn bounded_reader_accepts_the_limit_and_rejects_one_more_byte() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    let prefix = r#"{"version":1,"seq":1}"#;
    let mut exact = prefix.as_bytes().to_vec();
    exact.resize(64 * 1024, b' ');
    std::fs::write(&path, exact).unwrap();
    assert!(read_checkpoint(&path).is_ok());

    std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
    assert!(read_checkpoint(&path).is_err());
}

#[test]
fn collection_and_text_bounds_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    let mut value = checkpoint();
    value.done = (0..33).map(|n| format!("done {n}")).collect();
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());

    let mut value = checkpoint();
    value.next = vec!["x".repeat(513)];
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());

    let mut value = checkpoint();
    value.activities = (0..17)
        .map(|n| CheckpointActivity {
            id: format!("activity-{n}"),
            status: CheckpointActivityStatus::Waiting,
            handle: None,
            output_ref: None,
            started_unix_s: None,
            deadline_unix_s: None,
        })
        .collect();
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());
}

#[test]
fn activity_identity_reference_and_control_bounds_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");

    let mut value = checkpoint();
    value.activities[0].id = "x".repeat(129);
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());

    value.activities[0].id = "tests".into();
    value.activities[0].handle = Some("x".repeat(1025));
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());

    value.activities[0].handle = None;
    value.activities[0].status = CheckpointActivityStatus::Completed;
    value.activities[0].output_ref = Some("x".repeat(1025));
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());

    value.activities[0].output_ref = None;
    value.blockers = vec!["line one\nline two".into()];
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());
}

#[test]
fn detached_activities_require_revalidatable_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");

    for status in [
        CheckpointActivityStatus::Running,
        CheckpointActivityStatus::Waiting,
    ] {
        let mut value = checkpoint();
        value.activities[0].status = status;
        value.activities[0].handle = None;
        write(&path, &value);
        assert!(
            read_checkpoint(&path).is_err(),
            "{status:?} work must be identifiable"
        );
    }

    let mut value = checkpoint();
    value.activities[0].deadline_unix_s = None;
    write(&path, &value);
    assert!(
        read_checkpoint(&path).is_err(),
        "detached work must carry its enforced termination deadline"
    );

    for handle in [
        "pid:1234",
        "pid:not-a-pid@start:42",
        "tmux:missing-session",
        "unknown:provider/id",
    ] {
        let mut value = checkpoint();
        value.activities[0].handle = Some(handle.into());
        write(&path, &value);
        assert!(
            read_checkpoint(&path).is_err(),
            "{handle:?} is not provable"
        );
    }

    for (started, deadline) in [
        (0, 1787803600),
        (1787800000, 1787800000),
        (1787800000, 1787886401),
    ] {
        let mut value = checkpoint();
        value.activities[0].started_unix_s = Some(started);
        value.activities[0].deadline_unix_s = Some(deadline);
        write(&path, &value);
        assert!(
            read_checkpoint(&path).is_err(),
            "invalid bounded window {started}..{deadline}"
        );
    }

    for status in [
        CheckpointActivityStatus::Completed,
        CheckpointActivityStatus::Failed,
        CheckpointActivityStatus::Unknown,
    ] {
        let mut value = checkpoint();
        value.activities[0].status = status;
        value.activities[0].handle = None;
        value.activities[0].started_unix_s = None;
        value.activities[0].deadline_unix_s = None;
        write(&path, &value);
        assert!(
            read_checkpoint(&path).is_ok(),
            "{status:?} history no longer needs a live handle"
        );
    }
}

#[test]
fn invisible_and_directional_formatting_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");

    for unsafe_text in [
        "soft\u{00ad}hyphen",
        "arabic\u{061c}mark",
        "hidden\u{200b}joiner",
        "reversed\u{202e}text",
        "isolated\u{2060}word",
        "mark\u{feff}",
        "annotation\u{fff9}anchor",
        "shorthand\u{1bca0}format",
        "music\u{1d173}format",
        "tag\u{e0001}language",
        "tag\u{e0020}space",
    ] {
        let mut value = checkpoint();
        value.next = vec![unsafe_text.into()];
        write(&path, &value);
        assert!(
            read_checkpoint(&path).is_err(),
            "{unsafe_text:?} must not reach the terminal"
        );
    }
}

#[cfg(unix)]
#[test]
fn checkpoint_reader_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.json");
    let link = dir.path().join("checkpoint.json");
    write(&target, &checkpoint());
    symlink(&target, &link).unwrap();

    assert!(read_checkpoint(&link).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn checkpoint_reader_rejects_fifos_without_blocking() {
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();

    let reader_path = path.clone();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        tx.send(read_checkpoint(&reader_path).is_err()).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_millis(250));
    if result.is_err() {
        // Unblock the pre-fix reader so the test process does not retain a stuck thread.
        drop(std::fs::OpenOptions::new().write(true).open(&path).unwrap());
    }
    reader.join().unwrap();

    assert_eq!(result, Ok(true), "FIFO inspection blocked or was accepted");
}

#[test]
fn empty_entries_and_io_errors_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkpoint.json");
    let mut value = checkpoint();
    value.decisions = vec!["   ".into()];
    write(&path, &value);
    assert!(read_checkpoint(&path).is_err());

    assert!(read_checkpoint(dir.path()).is_err());
}

#[test]
fn every_activity_status_has_a_stable_wire_label() {
    for (status, label) in [
        (CheckpointActivityStatus::Running, "running"),
        (CheckpointActivityStatus::Waiting, "waiting"),
        (CheckpointActivityStatus::Completed, "completed"),
        (CheckpointActivityStatus::Failed, "failed"),
        (CheckpointActivityStatus::Unknown, "unknown"),
    ] {
        assert_eq!(status.label(), label);
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            format!("\"{label}\"")
        );
    }
}
