//! Exact Codex rollout identity discovery from a live tmux pane's Linux process tree.
//! Codex opens its active rollout JSONL for the life of the session, so the process's
//! file descriptors are a stronger binding than cwd-wide "latest session" scans.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const MAX_SESSION_META_BYTES: u64 = 1024 * 1024;
const ESRCH: i32 = 3;

#[derive(Deserialize)]
struct RolloutRecord {
    #[serde(rename = "type")]
    kind: String,
    payload: RolloutMeta,
}

#[derive(Deserialize)]
struct RolloutMeta {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    cwd: PathBuf,
    thread_source: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessIdentity {
    pub(super) pid: u32,
    pub(super) parent_pid: u32,
    pub(super) start_time: u64,
}

/// Return the one user-thread rollout held open by `root_pid` or its descendants.
///
/// The candidate must prove all four bindings: rollout-shaped filename, matching
/// metadata ID, canonical project cwd, and `thread_source=user`. Subagent rollouts
/// are deliberately ignored because Codex refuses new user turns on those threads.
/// More than one distinct user rollout is ambiguous and therefore an error.
pub(super) fn session_id_from_proc(
    proc_root: &Path,
    root_pid: u32,
    expected_cwd: &Path,
) -> Result<Option<String>> {
    let expected_cwd = std::fs::canonicalize(expected_cwd)
        .with_context(|| format!("canonicalize Codex project cwd {}", expected_cwd.display()))?;
    let processes = process_tree(proc_root, root_pid)?;
    let mut candidates = BTreeSet::new();

    for process in processes {
        if !process_is_current(
            same_process(proc_root, process)?,
            process.pid,
            root_pid,
            "changed while inspecting it",
        )? {
            continue;
        }
        if !is_codex_process(proc_root, process.pid)? {
            continue;
        }
        let fd_dir = proc_root.join(process.pid.to_string()).join("fd");
        let entries = match std::fs::read_dir(&fd_dir) {
            Ok(entries) => entries,
            Err(err) if process_path_gone(&err) => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("read {}", fd_dir.display()));
            }
        };
        let mut process_candidates = Vec::new();
        for entry in collect_live_entries(entries, "read Codex process fd entry")? {
            let fd_path = entry.path();
            let target = match std::fs::read_link(&fd_path) {
                Ok(target) => target,
                Err(err) if process_path_gone(&err) => continue,
                Err(err) => {
                    return Err(err).with_context(|| format!("readlink {}", fd_path.display()));
                }
            };
            let Some((rollout_path, filename_id)) = rollout_from_path(&target) else {
                continue;
            };
            if let Some(id) =
                validated_rollout(&fd_path, &rollout_path, &filename_id, &expected_cwd)?
            {
                process_candidates.push(id);
            }
        }
        if !process_is_current(
            same_process(proc_root, process)?,
            process.pid,
            root_pid,
            "changed while inspecting it",
        )? {
            continue;
        }
        candidates.extend(process_candidates);
    }

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next()),
        _ => bail!(
            "multiple user rollout ids are open in the Codex process tree: {}",
            candidates.into_iter().collect::<Vec<_>>().join(", ")
        ),
    }
}

pub(super) fn process_tree(proc_root: &Path, root_pid: u32) -> Result<Vec<ProcessIdentity>> {
    let root = process_identity(proc_root, root_pid)?
        .with_context(|| format!("Codex pane process {root_pid} disappeared"))?;
    let mut queue = VecDeque::from([root]);
    let mut seen = HashSet::from([root_pid]);
    let mut processes = Vec::new();

    while let Some(process) = queue.pop_front() {
        if !process_is_current(
            same_process(proc_root, process)?,
            process.pid,
            root_pid,
            "changed while walking its children",
        )? {
            continue;
        }
        let children = process_children(proc_root, process.pid)?;
        if !process_is_current(
            same_process(proc_root, process)?,
            process.pid,
            root_pid,
            "changed while walking its children",
        )? {
            continue;
        }
        processes.push(process);
        for child in children {
            if child.parent_pid == process.pid && seen.insert(child.pid) {
                queue.push_back(child);
            }
        }
    }
    Ok(processes)
}

pub(super) fn process_children(proc_root: &Path, pid: u32) -> Result<Vec<ProcessIdentity>> {
    let task_dir = proc_root.join(pid.to_string()).join("task");
    let tasks = match std::fs::read_dir(&task_dir) {
        Ok(tasks) => tasks,
        Err(err) if process_path_gone(&err) => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("read {}", task_dir.display())),
    };
    let mut children = Vec::new();

    for task in collect_live_entries(tasks, "read Codex process task entry")? {
        let children_path = task.path().join("children");
        let raw_children = match std::fs::read_to_string(&children_path) {
            Ok(children) => children,
            Err(err) if process_path_gone(&err) => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("read {}", children_path.display()));
            }
        };
        for raw in raw_children.split_whitespace() {
            let child_pid = raw.parse::<u32>().with_context(|| {
                format!("invalid child pid {raw:?} in {}", children_path.display())
            })?;
            if child_pid == 0 {
                bail!("invalid child pid 0 in {}", children_path.display());
            }
            if let Some(child) = process_identity(proc_root, child_pid)? {
                children.push(child);
            }
        }
    }
    Ok(children)
}

pub(super) fn process_identity(proc_root: &Path, pid: u32) -> Result<Option<ProcessIdentity>> {
    let stat_path = proc_root.join(pid.to_string()).join("stat");
    let stat = match std::fs::read_to_string(&stat_path) {
        Ok(stat) => stat,
        Err(err) if process_path_gone(&err) => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", stat_path.display())),
    };
    let (parent_pid, start_time) = process_parent_and_start_time(&stat)
        .with_context(|| format!("parse {}", stat_path.display()))?;
    Ok(Some(ProcessIdentity {
        pid,
        parent_pid,
        start_time,
    }))
}

fn same_process(proc_root: &Path, expected: ProcessIdentity) -> Result<bool> {
    Ok(process_identity(proc_root, expected.pid)? == Some(expected))
}

pub(super) fn process_is_current(
    same: bool,
    pid: u32,
    root_pid: u32,
    changed_while: &str,
) -> Result<bool> {
    if same {
        return Ok(true);
    }
    if pid == root_pid {
        bail!("Codex pane process {root_pid} {changed_while}");
    }
    Ok(false)
}

pub(super) fn process_parent_and_start_time(stat: &str) -> Result<(u32, u64)> {
    let comm_end = stat
        .rfind(") ")
        .context("process stat has no closing command name")?;
    let mut fields = stat[comm_end + 2..].split_whitespace();
    fields.next().context("process stat has no state")?;
    let parent_pid = fields
        .next()
        .context("process stat has no parent pid")?
        .parse::<u32>()
        .context("process stat parent pid is not an integer")?;
    let start_time = fields
        .nth(17)
        .context("process stat has no start time")?
        .parse::<u64>()
        .context("process stat start time is not an integer")?;
    Ok((parent_pid, start_time))
}

pub(super) fn process_path_gone(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::NotFound || err.raw_os_error() == Some(ESRCH)
}

pub(super) fn live_proc_entry<T>(
    entry: std::io::Result<T>,
    context: &'static str,
) -> Result<Option<T>> {
    match entry {
        Ok(entry) => Ok(Some(entry)),
        Err(err) if process_path_gone(&err) => Ok(None),
        Err(err) => Err(err).context(context),
    }
}

pub(super) fn collect_live_entries<T>(
    entries: impl IntoIterator<Item = std::io::Result<T>>,
    context: &'static str,
) -> Result<Vec<T>> {
    let mut live = Vec::new();
    for entry in entries {
        if let Some(entry) = live_proc_entry(entry, context)? {
            live.push(entry);
        }
    }
    Ok(live)
}

pub(super) fn is_codex_process(proc_root: &Path, pid: u32) -> Result<bool> {
    let cmdline_path = proc_root.join(pid.to_string()).join("cmdline");
    let cmdline = match std::fs::read(&cmdline_path) {
        Ok(cmdline) => cmdline,
        Err(err) if process_path_gone(&err) => return Ok(false),
        Err(err) => {
            return Err(err).with_context(|| format!("read {}", cmdline_path.display()));
        }
    };
    Ok(cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .filter_map(|arg| Path::new(std::str::from_utf8(arg).ok()?).file_name())
        .filter_map(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .any(|name| name == "codex" || name == "codex.exe" || name.starts_with("codex-")))
}

pub(super) fn rollout_from_path(path: &Path) -> Option<(PathBuf, String)> {
    if path.to_string_lossy().ends_with(" (deleted)") {
        return None;
    }
    if !path
        .components()
        .any(|component| component.as_os_str() == "sessions")
    {
        return None;
    }
    let filename = path.file_name()?.to_str()?;
    let stem = filename.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    is_uuid(id).then(|| (path.to_path_buf(), id.to_string()))
}

pub(super) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(super) fn validated_rollout(
    fd_path: &Path,
    rollout_path: &Path,
    filename_id: &str,
    expected_cwd: &Path,
) -> Result<Option<String>> {
    let file = match File::open(fd_path) {
        Ok(file) => file,
        Err(err) if process_path_gone(&err) => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("open rollout fd {}", fd_path.display()));
        }
    };
    let persisted = match File::open(rollout_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("open persisted rollout {}", rollout_path.display()));
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let open_meta = file
            .metadata()
            .context(format!("stat rollout fd {}", fd_path.display()))?;
        let persisted_meta = persisted
            .metadata()
            .context(format!("stat persisted rollout {}", rollout_path.display()))?;
        if open_meta.dev() != persisted_meta.dev() || open_meta.ino() != persisted_meta.ino() {
            bail!(
                "open rollout fd {} no longer names persisted file {}",
                fd_path.display(),
                rollout_path.display()
            );
        }
    }
    let mut first_line = String::new();
    BufReader::new(file)
        .take(MAX_SESSION_META_BYTES)
        .read_line(&mut first_line)
        .with_context(|| format!("read session metadata from {}", rollout_path.display()))?;
    let record: RolloutRecord = serde_json::from_str(&first_line)
        .with_context(|| format!("parse session metadata from {}", rollout_path.display()))?;
    if record.kind != "session_meta" {
        bail!(
            "rollout fd {} does not begin with session_meta",
            rollout_path.display()
        );
    }

    if record.payload.thread_source == "subagent" {
        return Ok(None);
    }
    if record.payload.thread_source != "user" {
        return Ok(None);
    }
    if record.payload.id.as_deref() != Some(filename_id)
        || record
            .payload
            .session_id
            .as_deref()
            .is_some_and(|id| id != filename_id)
    {
        bail!(
            "rollout metadata id does not match filename id {filename_id} in {}",
            rollout_path.display()
        );
    }
    let rollout_cwd = std::fs::canonicalize(&record.payload.cwd).with_context(|| {
        format!(
            "canonicalize rollout cwd {} from {}",
            record.payload.cwd.display(),
            rollout_path.display()
        )
    })?;
    Ok((rollout_cwd == expected_cwd).then(|| filename_id.to_string()))
}
