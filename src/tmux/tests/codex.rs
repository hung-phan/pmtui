//! Focused coverage for exact Codex rollout discovery and its procfs failure modes.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::{TempDir, tempdir};

use crate::tmux::codex::{
    ProcessIdentity, collect_live_entries, is_codex_process, is_uuid, live_proc_entry,
    process_children, process_identity, process_is_current, process_parent_and_start_time,
    process_tree, rollout_from_path, session_id_from_proc, validated_rollout,
};

const ID: &str = "11111111-2222-4333-8444-555555555555";
const OTHER_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn stat_line(pid: u32, parent_pid: u32, start_time: u64) -> String {
    let mut fields = vec!["S".to_string(), parent_pid.to_string()];
    fields.extend(std::iter::repeat_n("0".to_string(), 17));
    fields.push(start_time.to_string());
    format!("{pid} (codex helper) {}\n", fields.join(" "))
}

fn write_stat(proc_root: &Path, pid: u32, parent_pid: u32, start_time: u64) {
    let pid_dir = proc_root.join(pid.to_string());
    std::fs::create_dir_all(&pid_dir).unwrap();
    std::fs::write(pid_dir.join("stat"), stat_line(pid, parent_pid, start_time)).unwrap();
}

fn write_proc_node(proc_root: &Path, pid: u32, parent_pid: u32, children: &str) {
    let pid_dir = proc_root.join(pid.to_string());
    let task_dir = pid_dir.join("task").join(pid.to_string());
    std::fs::create_dir_all(&task_dir).unwrap();
    std::fs::write(task_dir.join("children"), children).unwrap();
    std::fs::write(pid_dir.join("cmdline"), b"codex\0").unwrap();
    std::fs::create_dir_all(pid_dir.join("fd")).unwrap();
    write_stat(proc_root, pid, parent_pid, u64::from(pid) * 100);
}

fn project_dir(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

fn metadata_line(
    kind: &str,
    id: Option<&str>,
    session_id: Option<&str>,
    cwd: &Path,
    source: &str,
) -> String {
    let record = serde_json::json!({
        "type": kind,
        "payload": {
            "id": id,
            "session_id": session_id,
            "cwd": cwd,
            "thread_source": source,
        }
    });
    format!("{record}\n")
}

#[cfg(unix)]
fn validation_case(
    root: &Path,
    label: &str,
    contents: &str,
    expected_cwd: &Path,
) -> anyhow::Result<Option<String>> {
    use std::os::unix::fs::symlink;

    let rollout = root.join(format!("{label}.jsonl"));
    let fd = root.join(format!("{label}.fd"));
    std::fs::write(&rollout, contents).unwrap();
    symlink(&rollout, &fd).unwrap();
    validated_rollout(&fd, &rollout, ID, expected_cwd)
}

#[test]
fn proc_stat_parser_rejects_each_missing_or_invalid_required_field() {
    let no_start = format!(
        "1 (codex) S 0 {}",
        std::iter::repeat_n("0", 17).collect::<Vec<_>>().join(" ")
    );
    let invalid_start = format!("{no_start} nope");
    for (stat, expected) in [
        ("1 codex S 0", "no closing command name"),
        ("1 (codex) ", "no state"),
        ("1 (codex) S", "no parent pid"),
        ("1 (codex) S nope", "parent pid is not an integer"),
        (&no_start, "no start time"),
        (&invalid_start, "start time is not an integer"),
    ] {
        let err = process_parent_and_start_time(stat).unwrap_err();
        assert!(err.to_string().contains(expected), "{stat:?}: {err:#}");
    }
}

#[test]
fn process_identity_handles_valid_missing_unreadable_and_malformed_stat() {
    let dir = tempdir().unwrap();
    let proc_root = dir.path();
    write_stat(proc_root, 10, 4, 900);
    assert_eq!(
        process_identity(proc_root, 10).unwrap(),
        Some(ProcessIdentity {
            pid: 10,
            parent_pid: 4,
            start_time: 900,
        })
    );
    assert_eq!(process_identity(proc_root, 11).unwrap(), None);

    std::fs::create_dir_all(proc_root.join("12/stat")).unwrap();
    let err = process_identity(proc_root, 12).unwrap_err();
    assert!(err.to_string().contains("read"));

    std::fs::create_dir_all(proc_root.join("13")).unwrap();
    std::fs::write(proc_root.join("13/stat"), "13 (codex) S").unwrap();
    let err = process_identity(proc_root, 13).unwrap_err();
    assert!(err.to_string().contains("parse"));
}

#[test]
fn process_revalidation_distinguishes_current_root_and_vanished_child() {
    assert!(process_is_current(true, 100, 100, "changed").unwrap());
    assert!(!process_is_current(false, 101, 100, "changed").unwrap());

    let error = process_is_current(false, 100, 100, "changed while inspecting it").unwrap_err();
    assert_eq!(
        error.to_string(),
        "Codex pane process 100 changed while inspecting it"
    );
}

#[test]
fn proc_directory_entries_skip_vanished_paths_and_preserve_other_errors() {
    assert_eq!(
        live_proc_entry(Ok::<_, std::io::Error>(7), "entry").unwrap(),
        Some(7)
    );
    assert_eq!(
        live_proc_entry::<()>(
            Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            "entry",
        )
        .unwrap(),
        None
    );
    assert_eq!(
        live_proc_entry::<()>(Err(std::io::Error::from_raw_os_error(3)), "entry").unwrap(),
        None
    );
    let error = live_proc_entry::<()>(
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        "read test entry",
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("read test entry"));

    let entries = vec![
        Ok(1),
        Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        Ok(2),
    ];
    assert_eq!(
        collect_live_entries(entries, "collect test entries").unwrap(),
        [1, 2]
    );
    let hard_error = collect_live_entries(
        vec![Err::<(), _>(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        ))],
        "collect test entries",
    )
    .unwrap_err();
    assert!(format!("{hard_error:#}").contains("collect test entries"));
}

#[test]
fn process_children_handles_missing_and_unreadable_task_directories() {
    let dir = tempdir().unwrap();
    assert!(process_children(dir.path(), 1).unwrap().is_empty());

    std::fs::create_dir_all(dir.path().join("2")).unwrap();
    std::fs::write(dir.path().join("2/task"), "not a directory").unwrap();
    let err = process_children(dir.path(), 2).unwrap_err();
    assert!(err.to_string().contains("read"));
}

#[test]
fn process_children_validates_each_threads_children_file() {
    let dir = tempdir().unwrap();
    let proc_root = dir.path();

    std::fs::create_dir_all(proc_root.join("3/task/30/children")).unwrap();
    let err = process_children(proc_root, 3).unwrap_err();
    assert!(err.to_string().contains("children"));

    std::fs::create_dir_all(proc_root.join("4/task/40")).unwrap();
    std::fs::write(proc_root.join("4/task/40/children"), "not-a-pid").unwrap();
    let err = process_children(proc_root, 4).unwrap_err();
    assert!(err.to_string().contains("invalid child pid"));

    std::fs::create_dir_all(proc_root.join("5/task/50")).unwrap();
    std::fs::write(proc_root.join("5/task/50/children"), "0").unwrap();
    let err = process_children(proc_root, 5).unwrap_err();
    assert!(err.to_string().contains("child pid 0"));

    std::fs::create_dir_all(proc_root.join("6/task/60")).unwrap();
    std::fs::write(proc_root.join("6/task/60/children"), "999").unwrap();
    assert!(process_children(proc_root, 6).unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn codex_process_detection_accepts_supported_names_and_fails_closed() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let proc_root = dir.path();
    assert!(!is_codex_process(proc_root, 1).unwrap());

    let cmdline = proc_root.join("2/cmdline");
    std::fs::create_dir_all(cmdline.parent().unwrap()).unwrap();
    symlink("cmdline", &cmdline).unwrap();
    let err = is_codex_process(proc_root, 2).unwrap_err();
    assert!(err.to_string().contains("cmdline"));

    let cmdline = proc_root.join("3/cmdline");
    std::fs::create_dir_all(cmdline.parent().unwrap()).unwrap();
    for raw in [
        b"/usr/bin/CODEX.EXE\0".as_slice(),
        b"/opt/bin/codex-linux-x86_64\0".as_slice(),
        b"node\0/usr/local/bin/Codex\0".as_slice(),
    ] {
        std::fs::write(&cmdline, raw).unwrap();
        assert!(is_codex_process(proc_root, 3).unwrap(), "{raw:?}");
    }
    for raw in [b"\0tail\0".as_slice(), b"\xff\0codexish\0".as_slice()] {
        std::fs::write(&cmdline, raw).unwrap();
        assert!(!is_codex_process(proc_root, 3).unwrap(), "{raw:?}");
    }
}

#[test]
fn rollout_paths_require_a_live_session_jsonl_with_a_uuid_suffix() {
    let valid = PathBuf::from(format!("/tmp/codex/sessions/rollout-old-{ID}.jsonl"));
    assert_eq!(
        rollout_from_path(&valid),
        Some((valid.clone(), ID.to_string()))
    );
    for invalid in [
        format!("{} (deleted)", valid.display()),
        format!("/tmp/codex/archive/rollout-old-{ID}.jsonl"),
        format!("/tmp/codex/sessions/not-rollout-{ID}.jsonl"),
        format!("/tmp/codex/sessions/rollout-{ID}.txt"),
        "/tmp/codex/sessions/rollout-short.jsonl".to_string(),
        "/tmp/codex/sessions/rollout-11111111-2222-4333-8444-55555555555z.jsonl".to_string(),
    ] {
        assert_eq!(rollout_from_path(Path::new(&invalid)), None, "{invalid}");
    }
}

#[test]
#[cfg(unix)]
fn rollout_paths_reject_non_utf8_filenames_and_uuid_shape_errors() {
    use std::os::unix::ffi::OsStringExt;

    let mut raw = b"/tmp/sessions/rollout-".to_vec();
    raw.push(0xff);
    assert_eq!(rollout_from_path(Path::new(&OsString::from_vec(raw))), None);

    assert!(is_uuid(ID));
    assert!(is_uuid("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE"));
    for invalid in [
        "",
        "111111112222-4333-8444-555555555555",
        "11111111_2222-4333-8444-555555555555",
        "11111111-2222-4333-8444-55555555555z",
    ] {
        assert!(!is_uuid(invalid), "{invalid}");
    }
}

#[test]
#[cfg(unix)]
fn validated_rollout_handles_open_and_persisted_file_failures() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let expected_cwd = project_dir(&dir, "project");
    let missing = dir.path().join("missing");
    assert_eq!(
        validated_rollout(&missing, &missing, ID, &expected_cwd).unwrap(),
        None
    );

    let fd_loop = dir.path().join("fd-loop");
    symlink("fd-loop", &fd_loop).unwrap();
    let err = validated_rollout(&fd_loop, &missing, ID, &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("open rollout fd"));

    let fd = dir.path().join("fd");
    std::fs::write(&fd, "anything").unwrap();
    assert_eq!(
        validated_rollout(&fd, &missing, ID, &expected_cwd).unwrap(),
        None
    );

    let persisted_loop = dir.path().join("persisted-loop");
    symlink("persisted-loop", &persisted_loop).unwrap();
    let err = validated_rollout(&fd, &persisted_loop, ID, &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("open persisted rollout"));
}

#[test]
#[cfg(unix)]
fn validated_rollout_binds_the_fd_inode_and_reports_read_failures() {
    let dir = tempdir().unwrap();
    let expected_cwd = project_dir(&dir, "project");
    let fd = dir.path().join("fd");
    let persisted = dir.path().join("persisted");
    std::fs::write(&fd, "first").unwrap();
    std::fs::write(&persisted, "second").unwrap();
    let err = validated_rollout(&fd, &persisted, ID, &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("no longer names persisted file"));

    let unreadable = dir.path().join("directory");
    std::fs::create_dir(&unreadable).unwrap();
    let err = validated_rollout(&unreadable, &unreadable, ID, &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("read session metadata"));
}

#[test]
#[cfg(unix)]
fn validated_rollout_rejects_bad_json_kind_and_thread_source() {
    let dir = tempdir().unwrap();
    let expected_cwd = project_dir(&dir, "project");

    let err = validation_case(dir.path(), "json", "{", &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("parse session metadata"));

    let wrong_kind = metadata_line("event", Some(ID), Some(ID), &expected_cwd, "user");
    let err = validation_case(dir.path(), "kind", &wrong_kind, &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("does not begin with session_meta"));

    for (label, source) in [("subagent", "subagent"), ("other", "system")] {
        let line = metadata_line("session_meta", Some(ID), Some(ID), &expected_cwd, source);
        assert_eq!(
            validation_case(dir.path(), label, &line, &expected_cwd).unwrap(),
            None
        );
    }
}

#[test]
#[cfg(unix)]
fn validated_rollout_rejects_absent_or_mismatched_metadata_ids() {
    let dir = tempdir().unwrap();
    let expected_cwd = project_dir(&dir, "project");
    for (label, id, session_id) in [
        ("missing-id", None, Some(ID)),
        ("wrong-id", Some(OTHER_ID), Some(ID)),
        ("wrong-session", Some(ID), Some(OTHER_ID)),
    ] {
        let line = metadata_line("session_meta", id, session_id, &expected_cwd, "user");
        let err = validation_case(dir.path(), label, &line, &expected_cwd).unwrap_err();
        assert!(err.to_string().contains("does not match filename id"));
    }
}

#[test]
#[cfg(unix)]
fn validated_rollout_canonicalizes_and_compares_project_directories() {
    let dir = tempdir().unwrap();
    let expected_cwd = project_dir(&dir, "project");
    let other_cwd = project_dir(&dir, "other");
    let missing_cwd = dir.path().join("gone");

    let line = metadata_line("session_meta", Some(ID), None, &missing_cwd, "user");
    let err = validation_case(dir.path(), "missing-cwd", &line, &expected_cwd).unwrap_err();
    assert!(err.to_string().contains("canonicalize rollout cwd"));

    let line = metadata_line("session_meta", Some(ID), None, &other_cwd, "user");
    assert_eq!(
        validation_case(dir.path(), "other-cwd", &line, &expected_cwd).unwrap(),
        None
    );

    let line = metadata_line("session_meta", Some(ID), None, &expected_cwd, "user");
    assert_eq!(
        validation_case(dir.path(), "expected-cwd", &line, &expected_cwd)
            .unwrap()
            .as_deref(),
        Some(ID)
    );
}

#[test]
fn session_probe_reports_missing_project_and_root_process() {
    let dir = tempdir().unwrap();
    let missing_project = dir.path().join("missing-project");
    let err = session_id_from_proc(dir.path(), 100, &missing_project).unwrap_err();
    assert!(err.to_string().contains("canonicalize Codex project cwd"));

    let project = project_dir(&dir, "project");
    let err = session_id_from_proc(dir.path(), 100, &project).unwrap_err();
    assert!(err.to_string().contains("pane process 100 disappeared"));
}

#[test]
fn session_probe_handles_missing_and_unreadable_fd_directories() {
    let dir = tempdir().unwrap();
    let project = project_dir(&dir, "project");

    let missing_fd_root = dir.path().join("missing-fd-proc");
    write_proc_node(&missing_fd_root, 100, 0, "");
    std::fs::remove_dir(missing_fd_root.join("100/fd")).unwrap();
    assert_eq!(
        session_id_from_proc(&missing_fd_root, 100, &project).unwrap(),
        None
    );

    let bad_fd_root = dir.path().join("bad-fd-proc");
    write_proc_node(&bad_fd_root, 100, 0, "");
    std::fs::remove_dir(bad_fd_root.join("100/fd")).unwrap();
    std::fs::write(bad_fd_root.join("100/fd"), "not a directory").unwrap();
    let err = session_id_from_proc(&bad_fd_root, 100, &project).unwrap_err();
    assert!(err.to_string().contains("read"));
}

#[test]
fn session_probe_ignores_a_live_non_codex_root_process() {
    let dir = tempdir().unwrap();
    let project = project_dir(&dir, "project");
    let proc_root = dir.path().join("proc");
    write_proc_node(&proc_root, 100, 0, "");
    std::fs::write(proc_root.join("100/cmdline"), b"sh\0").unwrap();

    assert_eq!(
        session_id_from_proc(&proc_root, 100, &project).unwrap(),
        None
    );
}

#[test]
fn session_probe_propagates_codex_process_read_errors() {
    let dir = tempdir().unwrap();
    let project = project_dir(&dir, "project");
    let proc_root = dir.path().join("proc");
    write_proc_node(&proc_root, 100, 0, "");
    std::fs::remove_file(proc_root.join("100/cmdline")).unwrap();
    std::fs::create_dir(proc_root.join("100/cmdline")).unwrap();

    let error = session_id_from_proc(&proc_root, 100, &project).unwrap_err();
    assert!(format!("{error:#}").contains("cmdline"));
}

#[test]
fn session_probe_reports_an_unreadable_fd_link() {
    let dir = tempdir().unwrap();
    let project = project_dir(&dir, "project");
    let proc_root = dir.path().join("proc");
    write_proc_node(&proc_root, 100, 0, "");
    std::fs::write(proc_root.join("100/fd/not-a-link"), "plain file").unwrap();

    let err = session_id_from_proc(&proc_root, 100, &project).unwrap_err();
    assert!(err.to_string().contains("readlink"));
}

#[cfg(target_os = "linux")]
fn fifo_proc_fixture() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    use rustix::fs::{CWD, Mode, mkfifoat};

    let dir = tempdir().unwrap();
    let project = project_dir(&dir, "project");
    let proc_root = dir.path().join("proc");
    let pid_dir = proc_root.join("100");
    std::fs::create_dir_all(pid_dir.join("task/100")).unwrap();
    std::fs::write(pid_dir.join("task/100/children"), "").unwrap();
    std::fs::write(pid_dir.join("cmdline"), b"codex\0").unwrap();
    std::fs::create_dir(pid_dir.join("fd")).unwrap();
    let stat_path = pid_dir.join("stat");
    mkfifoat(CWD, &stat_path, Mode::RUSR | Mode::WUSR).unwrap();
    (dir, project, proc_root, stat_path)
}

#[cfg(target_os = "linux")]
fn feed_stat_versions(stat_path: PathBuf, start_times: Vec<u64>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for start_time in start_times {
            let mut pipe = OpenOptions::new().write(true).open(&stat_path).unwrap();
            if let Err(err) = pipe.write_all(stat_line(100, 0, start_time).as_bytes()) {
                assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
            }
            drop(pipe);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    })
}

#[test]
#[cfg(target_os = "linux")]
fn process_tree_rejects_root_pid_reuse_before_and_after_child_discovery() {
    for starts in [vec![100, 101], vec![100, 100, 101]] {
        let (_dir, _project, proc_root, stat_path) = fifo_proc_fixture();
        let writer = feed_stat_versions(stat_path, starts);
        let err = process_tree(&proc_root, 100).unwrap_err();
        writer.join().unwrap();
        assert!(
            err.to_string()
                .contains("changed while walking its children"),
            "{err:#}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn session_probe_rejects_root_pid_reuse_before_and_after_fd_inspection() {
    for starts in [vec![100, 100, 100, 101], vec![100, 100, 100, 100, 101]] {
        let (_dir, project, proc_root, stat_path) = fifo_proc_fixture();
        let writer = feed_stat_versions(stat_path, starts);
        let err = session_id_from_proc(&proc_root, 100, &project).unwrap_err();
        writer.join().unwrap();
        assert!(
            err.to_string().contains("changed while inspecting it"),
            "{err:#}"
        );
    }
}

#[test]
fn process_tree_deduplicates_children_and_rejects_wrong_parents() {
    let dir = tempdir().unwrap();
    let proc_root = dir.path();
    write_proc_node(proc_root, 100, 0, "101 101 102");
    write_proc_node(proc_root, 101, 100, "");
    write_proc_node(proc_root, 102, 999, "");

    assert_eq!(
        process_tree(proc_root, 100).unwrap(),
        vec![
            ProcessIdentity {
                pid: 100,
                parent_pid: 0,
                start_time: 10_000,
            },
            ProcessIdentity {
                pid: 101,
                parent_pid: 100,
                start_time: 10_100,
            },
        ]
    );
}
