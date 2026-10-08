//! Atomic, durable file replacement — the one implementation of the temp+fsync+rename
//! pattern, and the readers paired with it. It lives on its own because every
//! `.project-state/` file goes through it, and the guarantee it makes (a reader sees the
//! whole old file or the whole new one, never a torn prefix) is what lets the record
//! layer read a file another process may be replacing right now.

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically and durably: temp (in the SAME directory, so
/// the rename never crosses a filesystem) + fsync(temp) + rename + fsync(dir).
///
/// This is the single implementation of the pattern; [`write_json_atomic`] and
/// [`write_text_atomic`] are thin serializers on top of it. Atomicity is what makes
/// a concurrent READER safe: every reader either sees the whole previous file or the
/// whole new one, never a truncated or empty prefix. That matters most for files a
/// reader parses leniently (e.g. `brief.md`, read with `unwrap_or_default()`), where
/// a torn read would be silently mistaken for "no content" rather than erroring.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("create_dir_all {}", dir.display()))?;
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("path has no file name")?;
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{fname}.tmp.{}.{nonce}", std::process::id()));
    // Write + fsync + rename in one fallible block so ANY failure (a full disk mid-write,
    // an fsync error, a rename across a permission boundary) can drop the half-written
    // temp on the way out. Without this cleanup a failing writer leaks a
    // `.<name>.tmp.<pid>.<n>` file every attempt, and `.project-state/` fills with orphans
    // that nothing ever collects. A SUCCESSFUL rename consumes `tmp`, so the remove only
    // fires on the error paths.
    let staged = stage_atomic(path, bytes, &tmp);
    if let Err(e) = staged {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // Make the rename itself durable. Best-effort: some platforms reject dir
    // fsync; a failure here does not corrupt the (already-renamed) file.
    if let Ok(dirf) = fs::File::open(dir) {
        let _ = dirf.sync_all();
    }
    Ok(())
}

/// Stage and rename one atomic replacement. Kept separate so each filesystem
/// failure can be exercised without relying on a race against the temp nonce.
///
/// The temp is created with `create_new` (`O_CREAT|O_EXCL`), which never follows a symlink:
/// session state dirs are agent-writable, so a link planted at the predictable temp name must
/// make this write fail rather than redirect it onto the link's target.
pub(super) fn stage_atomic(path: &Path, bytes: &[u8], tmp: &Path) -> Result<()> {
    let f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .with_context(|| format!("create temp {}", tmp.display()))?;
    commit_temp(f, path, bytes, tmp)
}

/// Write, fsync, and rename an already-created temp into `path`.
pub(super) fn commit_temp(mut f: fs::File, path: &Path, bytes: &[u8], tmp: &Path) -> Result<()> {
    f.write_all(bytes)
        .with_context(|| format!("write temp {}", tmp.display()))?;
    f.sync_all()
        .with_context(|| format!("fsync temp {}", tmp.display()))?;
    drop(f);
    fs::rename(tmp, path).with_context(|| format!("rename into {}", path.display()))
}

/// Write `value` as pretty JSON to `path` atomically and durably (see
/// [`write_atomic`]).
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Write plain text to `path` atomically and durably (see [`write_atomic`]) — the
/// text sibling of [`write_json_atomic`], for the human-authored `.md` files
/// (`brief.md`) that a concurrently-running harness re-reads at every tick.
pub fn write_text_atomic(path: &Path, text: &str) -> Result<()> {
    write_atomic(path, text.as_bytes())
}

/// Read and parse JSON at `path`.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let data = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))
}

/// Read and parse JSON at `path`, returning `default` when the file is absent.
pub fn read_json_or<T: DeserializeOwned>(path: &Path, default: T) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(data) => {
            serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Read and parse JSON at `path`, returning `None` when the file is absent.
pub fn read_json_opt<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(data) => Ok(Some(
            serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}
