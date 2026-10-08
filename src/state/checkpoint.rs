//! Bounded, agent-owned continuity state for long-running worker sessions.
//!
//! `checkpoint.json` is untrusted supplemental memory only. The worker is its sole writer; pmd and
//! pmtui only read it, and no scheduler, policy, stop, cadence, or completion decision depends on
//! it. Its strings are display data, never instructions.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const CHECKPOINT_VERSION: u32 = 1;
const CHECKPOINT_MAX_BYTES: u64 = 64 * 1024;
const CHECKPOINT_MAX_LIST_ITEMS: usize = 32;
const CHECKPOINT_MAX_ACTIVITIES: usize = 16;
const CHECKPOINT_MAX_TEXT_BYTES: usize = 512;
const CHECKPOINT_MAX_ACTIVITY_ID_BYTES: usize = 128;
const CHECKPOINT_MAX_REFERENCE_BYTES: usize = 1024;
const CHECKPOINT_MAX_ACTIVITY_RUNTIME_S: u64 = 24 * 60 * 60;

const UNSAFE_FORMAT_RANGES: &[(char, char)] = &[
    ('\u{00ad}', '\u{00ad}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061c}', '\u{061c}'),
    ('\u{06dd}', '\u{06dd}'),
    ('\u{070f}', '\u{070f}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08e2}', '\u{08e2}'),
    ('\u{180e}', '\u{180e}'),
    ('\u{200b}', '\u{200f}'),
    ('\u{202a}', '\u{202e}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206f}'),
    ('\u{feff}', '\u{feff}'),
    ('\u{fff9}', '\u{fffb}'),
    ('\u{110bd}', '\u{110bd}'),
    ('\u{110cd}', '\u{110cd}'),
    ('\u{13430}', '\u{1343f}'),
    ('\u{1bca0}', '\u{1bca3}'),
    ('\u{1d173}', '\u{1d17a}'),
    ('\u{e0001}', '\u{e0001}'),
    ('\u{e0020}', '\u{e007f}'),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointActivity {
    pub id: String,
    pub status: CheckpointActivityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_unix_s: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointActivityStatus {
    Running,
    Waiting,
    Completed,
    Failed,
    #[serde(other)]
    Unknown,
}

impl CheckpointActivityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCheckpoint {
    pub version: u32,
    pub seq: u64,
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(default)]
    pub in_progress: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub activities: Vec<CheckpointActivity>,
    #[serde(default)]
    pub next: Vec<String>,
    #[serde(default)]
    pub important_files: Vec<String>,
}

impl WorkerCheckpoint {
    fn validate(&self) -> Result<()> {
        if self.version != CHECKPOINT_VERSION {
            bail!(
                "unsupported worker checkpoint version {}; expected {CHECKPOINT_VERSION}",
                self.version
            );
        }
        for (label, values) in [
            ("done", self.done.as_slice()),
            ("in_progress", self.in_progress.as_slice()),
            ("decisions", self.decisions.as_slice()),
            ("blockers", self.blockers.as_slice()),
            ("next", self.next.as_slice()),
            ("important_files", self.important_files.as_slice()),
        ] {
            validate_list(label, values)?;
        }
        if self.activities.len() > CHECKPOINT_MAX_ACTIVITIES {
            bail!(
                "worker checkpoint activities has {} items; maximum is {CHECKPOINT_MAX_ACTIVITIES}",
                self.activities.len()
            );
        }
        for activity in &self.activities {
            validate_text(
                "activity id",
                &activity.id,
                CHECKPOINT_MAX_ACTIVITY_ID_BYTES,
            )?;
            if matches!(
                activity.status,
                CheckpointActivityStatus::Running | CheckpointActivityStatus::Waiting
            ) {
                if activity.handle.is_none() {
                    bail!(
                        "worker checkpoint running or waiting activities require a revalidatable handle"
                    );
                }
                let (Some(started), Some(deadline)) =
                    (activity.started_unix_s, activity.deadline_unix_s)
                else {
                    bail!(
                        "worker checkpoint running or waiting activities require a bounded start and deadline"
                    );
                };
                if started == 0
                    || deadline <= started
                    || deadline - started > CHECKPOINT_MAX_ACTIVITY_RUNTIME_S
                {
                    bail!(
                        "worker checkpoint activity runtime must be between 1 and {CHECKPOINT_MAX_ACTIVITY_RUNTIME_S} seconds"
                    );
                }
            }
            if let Some(handle) = activity.handle.as_deref() {
                validate_text("activity handle", handle, CHECKPOINT_MAX_REFERENCE_BYTES)?;
                validate_activity_handle(handle)?;
            }
            if let Some(output_ref) = activity.output_ref.as_deref() {
                validate_text(
                    "activity output_ref",
                    output_ref,
                    CHECKPOINT_MAX_REFERENCE_BYTES,
                )?;
            }
        }
        Ok(())
    }
}

/// Read and validate an optional checkpoint without ever creating or modifying it.
pub fn read_checkpoint(path: &Path) -> Result<Option<WorkerCheckpoint>> {
    let file = match open_checkpoint(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open worker checkpoint {}", path.display()));
        }
    };
    let inspect_context = format!("inspect worker checkpoint {}", path.display());
    let metadata = file.metadata().context(inspect_context)?;
    if !metadata.is_file() {
        bail!("worker checkpoint {} is not a regular file", path.display());
    }
    let mut bytes = Vec::new();
    let read_context = format!("read worker checkpoint {}", path.display());
    file.take(CHECKPOINT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context(read_context)?;
    if bytes.len() as u64 > CHECKPOINT_MAX_BYTES {
        bail!(
            "worker checkpoint {} exceeds {CHECKPOINT_MAX_BYTES} bytes",
            path.display()
        );
    }
    let checkpoint: WorkerCheckpoint = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse worker checkpoint {}", path.display()))?;
    checkpoint.validate()?;
    Ok(Some(checkpoint))
}

#[cfg(unix)]
fn open_checkpoint(path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    Ok(fd.into())
}

#[cfg(not(unix))]
fn open_checkpoint(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

fn validate_list(label: &str, values: &[String]) -> Result<()> {
    if values.len() > CHECKPOINT_MAX_LIST_ITEMS {
        bail!(
            "worker checkpoint {label} has {} items; maximum is {CHECKPOINT_MAX_LIST_ITEMS}",
            values.len()
        );
    }
    for value in values {
        validate_text(label, value, CHECKPOINT_MAX_TEXT_BYTES)?;
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() {
        bail!("worker checkpoint {label} entries must not be empty");
    }
    if value.len() > max_bytes {
        bail!(
            "worker checkpoint {label} entry has {} bytes; maximum is {max_bytes}",
            value.len()
        );
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || is_invisible_or_directional(ch))
    {
        bail!("worker checkpoint {label} entries contain unsafe formatting characters");
    }
    Ok(())
}

fn is_invisible_or_directional(ch: char) -> bool {
    UNSAFE_FORMAT_RANGES
        .iter()
        .any(|(start, end)| (*start..=*end).contains(&ch))
}

fn validate_activity_handle(handle: &str) -> Result<()> {
    let Some((kind, identity)) = handle.split_once(':') else {
        bail!("worker checkpoint activity handles require a typed identity");
    };
    match kind {
        "pid" => {
            let Some((pid, start)) = identity.split_once("@start:") else {
                bail!("worker checkpoint pid handles require pid and process start identity");
            };
            if pid.parse::<u32>().ok().is_none_or(|pid| pid == 0)
                || start.parse::<u64>().ok().is_none_or(|start| start == 0)
            {
                bail!("worker checkpoint pid handles require numeric nonzero identity");
            }
        }
        "tmux" | "ci" | "agent" | "job" | "external" => {
            let Some((owner, id)) = identity.split_once('/') else {
                bail!("worker checkpoint {kind} handles require owner and id");
            };
            if owner.trim().is_empty() || id.trim().is_empty() {
                bail!("worker checkpoint {kind} handles require nonempty owner and id");
            }
        }
        _ => bail!("unsupported worker checkpoint activity handle kind {kind:?}"),
    }
    Ok(())
}
