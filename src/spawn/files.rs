//! Hardened file I/O for spawn requests and receipts.
//!
//! The requests directory lives in a session's state directory, which that session's agent
//! can write, so every read here treats the directory as hostile: it never follows a final
//! symlink, never blocks opening a FIFO, and never reads more than [`MAX_REQUEST_BYTES`].
//!
//! A request is published once and never overwritten: it is staged in a private temp file in
//! the same directory and hard-linked to its final name, and the link fails rather than
//! replace a file that already has that name. Receipts are replaced atomically.

use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, anyhow, ensure};
use rustix::fs::{Mode, OFlags};

use super::{
    MAX_RECEIPTS_LISTED, MAX_REQUEST_BYTES, REQUESTS_DIR, SCHEMA_VERSION, SpawnReceipt,
    SpawnRequest,
};
use crate::state;

const REQUEST_SUFFIX: &str = ".request.json";
const RECEIPT_SUFFIX: &str = ".receipt.json";
/// A cancel marker: `<request-id>.cancel`. Deliberately not `.json` — it has no content to read.
const CANCEL_SUFFIX: &str = ".cancel";

/// Makes every temp name unique within this process, so two threads publishing the same id
/// never share a temp file.
static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// `state_dir/spawn-requests`.
pub fn requests_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(REQUESTS_DIR)
}

/// `dir/<id>.request.json`.
pub fn request_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{REQUEST_SUFFIX}"))
}

/// `dir/<id>.receipt.json`.
pub fn receipt_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{RECEIPT_SUFFIX}"))
}

/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`: a canonical lowercase
/// UUID, the only request id that may become a file name.
pub fn is_valid_request_id(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// The result of [`publish_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publish {
    /// This call created the request file.
    Published,
    /// A request with this id was already published; it is returned unchanged so the caller
    /// can tell an identical replay from a conflict.
    AlreadyPresent(Box<SpawnRequest>),
}

/// If the directory already holds a request with this id, read and return it as AlreadyPresent
/// before writing anything. Otherwise create_dir_all(dir) + chmod 0700; write
/// `<dir>/.<id>.<pid>.<nonce>.tmp` (0600); hard_link to request_path; remove temp; on EEXIST (a
/// racing publisher) read and parse the existing file and return AlreadyPresent.
///
/// The existing request is looked for first so that a directory which cannot take a temp file,
/// such as one on a full disk, still reports it rather than hide it behind a write error.
pub fn publish_request(dir: &Path, req: &SpawnRequest) -> Result<Publish> {
    ensure_valid_id(&req.request_id)?;
    let mut bytes = serde_json::to_vec_pretty(req).context("serialize the spawn request")?;
    bytes.push(b'\n');
    let final_path = request_path(dir, &req.request_id);
    if fs::symlink_metadata(dir).is_ok() {
        ensure_real_dir(dir)?;
        if fs::symlink_metadata(&final_path).is_ok() {
            return already_present(&final_path);
        }
    }
    fs::create_dir_all(dir).context(format!("create {}", dir.display()))?;
    ensure_real_dir(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .context(format!("restrict {} to 0700", dir.display()))?;

    let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".{}.{}.{nonce}.tmp",
        req.request_id,
        std::process::id()
    ));
    let linked = stage_temp(&tmp, &bytes).and_then(|()| fs::hard_link(&tmp, &final_path));
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => {
            sync_dir(dir);
            Ok(Publish::Published)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => already_present(&final_path),
        Err(error) => Err(anyhow!("publish {}: {error}", final_path.display())),
    }
}

/// The request already published at `final_path`, as [`Publish::AlreadyPresent`].
fn already_present(final_path: &Path) -> Result<Publish> {
    let existing =
        load_request(final_path).context(format!("read the existing {}", final_path.display()))?;
    Ok(Publish::AlreadyPresent(Box::new(existing)))
}

/// One `*.request.json` found by [`scan_requests`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEntry {
    Valid(Box<SpawnRequest>),
    /// A valid UUID name with bad content; the broker answers it with `invalid_request`.
    Invalid {
        request_id: String,
        reason: String,
    },
    /// A name with no trustworthy id to answer (or an unreadable directory); log only.
    Skipped {
        name: String,
        reason: String,
    },
}

/// Every `*.request.json` in dir, sorted by name: each validly named one loaded with
/// [`load_request_entry`], any other name `Skipped`. Temp files and receipts are ignored.
///
/// An absent directory has no requests. A directory that is a symlink, not a directory, or
/// cannot be listed yields one `Skipped` entry named after it.
pub fn scan_requests(dir: &Path, owner_session: &str) -> Vec<ScanEntry> {
    list_requests(dir)
        .into_iter()
        .map(|listed| match listed {
            Listed::Request(id) => match load_request_entry(dir, &id, owner_session) {
                Ok(req) => ScanEntry::Valid(req),
                Err(reason) => ScanEntry::Invalid {
                    request_id: id,
                    reason,
                },
            },
            Listed::Skipped { name, reason } => ScanEntry::Skipped { name, reason },
        })
        .collect()
}

/// One `*.request.json` name found by [`list_requests`], which opens nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listed {
    /// A validly named request, by id; [`load_request_entry`] opens it.
    Request(String),
    /// A name with no trustworthy id to answer (or an unreadable directory); log only.
    Skipped { name: String, reason: String },
}

/// Every `*.request.json` in dir, sorted by name, found by listing the directory alone: no
/// entry is opened. Discovery lists every directory on each refresh, so a request it has already
/// settled costs it nothing more.
///
/// An absent directory has no requests. A directory that is a symlink, not a directory, or
/// cannot be listed yields one `Skipped` entry named after it.
pub fn list_requests(dir: &Path) -> Vec<Listed> {
    let listing = match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => fs::read_dir(dir),
        Ok(_) => return vec![skipped_dir(dir, "the requests dir is not a real directory")],
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => Err(error),
    };
    let entries = match listing {
        Ok(entries) => entries,
        Err(error) => {
            return vec![skipped_dir(
                dir,
                &format!("the requests dir cannot be listed: {error}"),
            )];
        }
    };
    let mut found: Vec<(String, Listed)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            classify_name(&name).map(|listed| (name, listed))
        })
        .collect();
    found.sort_by(|(left, _), (right, _)| left.cmp(right));
    found.into_iter().map(|(_, listed)| listed).collect()
}

/// Open and check the request `id` in `dir`: a regular file (checked with `symlink_metadata`, so
/// a symlink is never followed) of at most [`MAX_REQUEST_BYTES`], valid JSON for this schema,
/// whose `request_id` is its file stem and whose `parent_session` is `owner_session`. Anything
/// else is the reason it is invalid.
pub fn load_request_entry(
    dir: &Path,
    id: &str,
    owner_session: &str,
) -> std::result::Result<Box<SpawnRequest>, String> {
    match load_request(&request_path(dir, id)) {
        Err(error) => Err(format!("{error:#}")),
        Ok(req) if req.request_id != id => Err(format!(
            "request_id {:?} does not match the file name",
            req.request_id
        )),
        Ok(req) if req.parent_session != owner_session => Err(format!(
            "parent_session {:?} is not {owner_session:?}, the session that owns this directory",
            req.parent_session
        )),
        Ok(req) => Ok(Box::new(req)),
    }
}

/// The receipt for `id`, or `None` when there is none yet. Read with the same no-follow,
/// non-blocking, size-capped reader as requests, and refused when it names another request.
pub fn read_receipt(dir: &Path, id: &str) -> Result<Option<SpawnReceipt>> {
    ensure_valid_id(id)?;
    let path = receipt_path(dir, id);
    let file = match open_no_follow(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow!("open {}: {error}", path.display())),
    };
    let bytes = read_capped(file).context(format!("read {}", path.display()))?;
    let receipt: SpawnReceipt =
        serde_json::from_slice(&bytes).context(format!("parse {}", path.display()))?;
    ensure!(
        receipt.request_id == id,
        "{} names request {}",
        path.display(),
        receipt.request_id
    );
    Ok(Some(receipt))
}

/// Replace `<dir>/<request_id>.receipt.json` atomically (`state::write_json_atomic`). The
/// directory must already exist and must not be a symlink.
pub fn write_receipt(dir: &Path, receipt: &SpawnReceipt) -> Result<()> {
    ensure_valid_id(&receipt.request_id)?;
    ensure_real_dir(dir)?;
    state::write_json_atomic(&receipt_path(dir, &receipt.request_id), receipt)
}

/// Remove the request file only (NotFound is Ok). Its receipt is kept for good as a small
/// tombstone: a replay of the request id finds the final answer, and its `args_hash`, even after
/// the request is gone, so a cleaned-up request can never be answered with a second child.
pub fn remove_request(dir: &Path, id: &str) -> Result<()> {
    ensure_valid_id(id)?;
    remove_if_present(&request_path(dir, id))
}

/// Every request id in `dir` that has a RECEIPT, in id order so the same directory always lists the
/// same way — the caller re-orders them by what it reads inside each one.
///
/// Names only: this opens no file, so the hardened [`read_receipt`] stays the one path that parses one.
/// A name that is not `<valid request id>.receipt.json` is ignored, as is a directory that is missing or
/// is not a real directory. Capped at [`MAX_RECEIPTS_LISTED`] so a pathological directory cannot turn a
/// listing into unbounded work.
pub fn list_receipt_ids(dir: &Path) -> Vec<String> {
    let entries = match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(RECEIPT_SUFFIX)?;
            is_valid_request_id(id).then(|| id.to_string())
        })
        .take(MAX_RECEIPTS_LISTED)
        .collect();
    ids.sort_unstable();
    ids
}

/// `dir/<id>.cancel`.
pub fn cancel_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{CANCEL_SUFFIX}"))
}

/// Ask the dashboard to stop the job answering request `id`.
///
/// A MARKER, NOT A MESSAGE: the file's presence is the whole signal, so there is nothing in it for
/// the dashboard to parse or trust, and asking twice is the same as asking once (`Ok(false)` the
/// second time). It is created with the same no-clobber link as a request, in the directory the
/// agent already writes, so a sandboxed child needs no new permission to cancel.
pub fn publish_cancel(dir: &Path, id: &str) -> Result<bool> {
    ensure_valid_id(id)?;
    let final_path = cancel_path(dir, id);
    if fs::symlink_metadata(dir).is_ok() {
        ensure_real_dir(dir)?;
        if fs::symlink_metadata(&final_path).is_ok() {
            return Ok(false);
        }
    }
    fs::create_dir_all(dir).context(format!("create {}", dir.display()))?;
    ensure_real_dir(dir)?;
    let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{id}.cancel.{}.{nonce}.tmp", std::process::id()));
    let linked = stage_temp(&tmp, b"").and_then(|()| fs::hard_link(&tmp, &final_path));
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => {
            sync_dir(dir);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(anyhow!("publish {}: {error}", final_path.display())),
    }
}

/// Whether a cancel was asked for request `id`. A marker that is not a regular file is ignored
/// rather than obeyed: everything else here treats the directory as hostile, and a dangling symlink
/// must not stop a job.
pub fn cancel_requested(dir: &Path, id: &str) -> bool {
    is_valid_request_id(id)
        && fs::symlink_metadata(cancel_path(dir, id)).is_ok_and(|meta| meta.is_file())
}

/// Remove the cancel marker (NotFound is Ok), once its job has been stopped.
pub fn remove_cancel(dir: &Path, id: &str) -> Result<()> {
    ensure_valid_id(id)?;
    remove_if_present(&cancel_path(dir, id))
}

fn ensure_valid_id(id: &str) -> Result<()> {
    ensure!(
        is_valid_request_id(id),
        "{id:?} is not a valid spawn request id (a lowercase UUID)"
    );
    Ok(())
}

fn ensure_real_dir(dir: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(dir).context(format!("inspect {}", dir.display()))?;
    ensure!(meta.is_dir(), "{} is not a real directory", dir.display());
    Ok(())
}

/// Create `path` as a new private (0600) file. A leftover at that exact name can only come
/// from a dead process that had this pid, so it is unlinked (a symlink is never followed) and
/// the file is created again.
pub(super) fn create_temp(path: &Path) -> io::Result<File> {
    match open_new(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(path)?;
            open_new(path)
        }
        opened => opened,
    }
}

fn open_new(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn stage_temp(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create_temp(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make the new directory entry durable. Best-effort, as in `state::write_atomic`.
fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

fn skipped_dir(dir: &Path, reason: &str) -> Listed {
    Listed::Skipped {
        name: dir.display().to_string(),
        reason: reason.to_string(),
    }
}

/// `None` for a file that is not a request at all (a receipt, a temp file, anything else).
fn classify_name(name: &str) -> Option<Listed> {
    let stem = name.strip_suffix(REQUEST_SUFFIX)?;
    Some(if is_valid_request_id(stem) {
        Listed::Request(stem.to_string())
    } else {
        Listed::Skipped {
            name: name.to_string(),
            reason: format!("the file name is not a lowercase UUID followed by {REQUEST_SUFFIX}"),
        }
    })
}

/// Parse one request file: a regular file (checked with `symlink_metadata`, then again on the
/// open descriptor), within the size cap, valid JSON, and this schema version.
fn load_request(path: &Path) -> Result<SpawnRequest> {
    check_regular(&fs::symlink_metadata(path).context("cannot inspect the request file")?)?;
    let file = open_no_follow(path).context("cannot open the request file")?;
    let bytes = read_capped(file)?;
    let req: SpawnRequest =
        serde_json::from_slice(&bytes).context("the request is not valid JSON for this schema")?;
    ensure!(
        req.schema_version == SCHEMA_VERSION,
        "unsupported schema_version {} (expected {SCHEMA_VERSION})",
        req.schema_version
    );
    Ok(req)
}

/// Open read-only without following a final symlink, and without blocking on a FIFO.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    Ok(fd.into())
}

/// Read an opened file that must still be regular and within the cap. The read itself is
/// bounded too, so a file that grows after the check is still refused.
fn read_capped(file: File) -> Result<Vec<u8>> {
    check_regular(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    check_len(bytes.len() as u64)?;
    Ok(bytes)
}

fn check_regular(meta: &Metadata) -> Result<()> {
    let kind = meta.file_type();
    ensure!(!kind.is_symlink(), "the file is a symlink");
    ensure!(kind.is_file(), "the file is not a regular file");
    check_len(meta.len())
}

fn check_len(len: u64) -> Result<()> {
    ensure!(
        len <= MAX_REQUEST_BYTES,
        "the file is larger than {MAX_REQUEST_BYTES} bytes"
    );
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(anyhow!("remove {}: {error}", path.display()))
        }
        _ => Ok(()),
    }
}
