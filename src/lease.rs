//! Cross-process, per-project driver lease (design §5/§11: "one driver per
//! project"). A `pmd` takes a project's lease before driving it; a second daemon
//! — or a stray manual run — cannot double-drive because `flock(LOCK_EX)` is
//! exclusive across processes.
//!
//! The lock is held by an open file descriptor, so the kernel releases it
//! automatically when the holder exits or crashes. Unlike a PID/TTL lock file,
//! there is nothing stale to reconcile: a crashed daemon's lease is immediately
//! re-acquirable by the next one.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use rustix::fs::{FlockOperation, flock};

/// A held exclusive lock on a project. Dropping it (or the process exiting)
/// releases the lock. The file handle is the lock, so it is kept alive here.
#[derive(Debug)]
pub struct ProjectLease {
    _file: std::fs::File,
}

/// The daemon-singleton flock path for a `(registry, socket)`:
/// `<registry-dir>/pmd-<socket>-<registry-hash>.lock`. Both `pmd` (which holds it
/// for its whole life) and `pmtui` (which probes it non-blocking) MUST derive it
/// identically, or the liveness check desyncs. Falls back to the current dir when
/// the registry path has no parent (e.g. a bare `registry.json`).
pub fn daemon_lock_path(registry: &Path, socket: &str) -> std::path::PathBuf {
    registry_socket_lock_path(registry, "pmd", socket)
}

/// Stop request consumed by the pmd that owns [`daemon_lock_path`].
pub fn daemon_stop_path(registry: &Path, socket: &str) -> std::path::PathBuf {
    registry_socket_lock_path(registry, "pmd-stop", socket)
}

/// Socket-global owner lock. A second registry cannot run another pmd against
/// the same tmux server and reap or type into the first registry's sessions.
pub fn socket_owner_lock_path(socket: &str) -> std::path::PathBuf {
    socket_lock_path(&runtime_lock_root(), "pmd-owner", socket)
}

fn socket_lock_path(root: &Path, kind: &str, socket: &str) -> std::path::PathBuf {
    lock_path(root, kind, socket, socket.as_bytes())
}

fn registry_socket_lock_path(registry: &Path, kind: &str, socket: &str) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;

    let mut scope = registry.as_os_str().as_bytes().to_vec();
    scope.push(0);
    scope.extend_from_slice(socket.as_bytes());
    lock_path(&registry_lock_root(registry), kind, socket, &scope)
}

fn lock_path(root: &Path, kind: &str, socket: &str, scope: &[u8]) -> std::path::PathBuf {
    let safe: String = socket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "socket"
    } else {
        safe.as_str()
    };
    root.join(format!("{kind}-{safe}-{:016x}.lock", fnv64(scope)))
}

fn fnv64(value: &[u8]) -> u64 {
    value.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn registry_lock_root(registry: &Path) -> std::path::PathBuf {
    registry
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn runtime_lock_root() -> std::path::PathBuf {
    let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
    let home = std::env::var_os("HOME");
    runtime_lock_root_from(
        xdg_runtime_dir.as_deref(),
        home.as_deref(),
        &std::env::temp_dir(),
    )
}

fn runtime_lock_root_from(
    xdg_runtime_dir: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    temp_dir: &Path,
) -> std::path::PathBuf {
    if let Some(dir) = xdg_runtime_dir.filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(dir).join("agent-manager");
    }
    if let Some(home) = home.filter(|s| !s.is_empty()) {
        return std::path::PathBuf::from(home).join(".cache/agent-manager/locks");
    }
    temp_dir.join("agent-manager-locks")
}

/// `<registry-dir>/pmtui-<socket>.lock` — the DASHBOARD's single-instance lock, held for
/// pmtui's whole life so a second dashboard on the same `(registry, socket)` refuses to
/// start rather than racing the first over registry edits and stepping on its sessions.
///
/// A SEPARATE file from [`daemon_lock_path`] (`pmd-<socket>.lock`) on purpose: pmtui and the
/// `pmd` it spawns share a socket and must both run, so they cannot share one lock. Same
/// `(registry, socket)` scoping, so "one dashboard per project" means exactly what "one pmd
/// per project" already does. The OS drops the flock on exit/crash, so a stale lock never
/// blocks the next launch.
pub fn pmtui_lock_path(registry: &Path, socket: &str) -> std::path::PathBuf {
    registry_socket_lock_path(registry, "pmtui", socket)
}

/// Cooperative handoff request consumed only by the pmtui instance whose owner nonce it names.
pub fn pmtui_takeover_path(registry: &Path, socket: &str) -> std::path::PathBuf {
    registry_socket_lock_path(registry, "pmtui-takeover", socket)
}

/// `<registry>.spawn-broker.lock`: the spawn broker's lease, keyed by the registry alone. The
/// dashboard singleton ([`pmtui_lock_path`]) is per `(registry, socket)`, so two dashboards on one
/// registry under different sockets both hold one; only the holder of this lease turns spawn
/// requests into rows, so no request can become two children. It sits beside the registry file,
/// so every spelling of the registry's path names the same lock.
pub fn spawn_broker_lock_path(registry: &Path) -> std::path::PathBuf {
    let mut name = registry.as_os_str().to_os_string();
    name.push(".spawn-broker.lock");
    std::path::PathBuf::from(name)
}

/// Try to take the project's driver lock without blocking:
/// - `Ok(Some(lease))` — acquired; this process now owns the project.
/// - `Ok(None)` — another live process holds it; the caller must not drive.
/// - `Err(_)` — an I/O fault creating/opening the lock file.
pub fn try_acquire(lock_path: &Path) -> io::Result<Option<ProjectLease>> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    lock_outcome(flock(&file, FlockOperation::NonBlockingLockExclusive))
        .map(|taken| taken.then_some(ProjectLease { _file: file }))
}

/// Classify one non-blocking `flock` attempt the same way for every caller: `Ok(true)` when this
/// process took the lock, `Ok(false)` when another process holds it (EWOULDBLOCK == EAGAIN on
/// Linux), and any other errno as an I/O error rather than a free lock.
pub(crate) fn lock_outcome(result: rustix::io::Result<()>) -> io::Result<bool> {
    match result {
        Ok(()) => Ok(true),
        Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN => Ok(false),
        Err(e) => Err(io::Error::from_raw_os_error(e.raw_os_error())),
    }
}

/// Is somebody holding the lock at `path` RIGHT NOW? A read-only liveness probe:
/// - `Ok(None)` — the file does not exist, so nobody has ever taken this lock here
///   (for the daemon singleton: no `pmd` has run for this `(registry, socket)`).
/// - `Ok(Some(true))` — a live process holds it.
/// - `Ok(Some(false))` — the file exists but the lock is FREE (the holder exited).
/// - `Err(_)` — the probe itself failed (permissions, `ENOTDIR`, …). Callers MUST NOT
///   read an error as "held": the honest answer is "unknown".
///
/// **MUST NOT create the file.** [`try_acquire`] opens with `create(true)`, and pmtui's
/// tests use "the singleton lock file exists" as the observable proof that an
/// `ensure_daemon` ran (`submit_create_on_autopilot_rejects_missing_goal`,
/// `cycle_tier_from_autopilot_to_standard_leaves_daemon_untouched`, …). A probe that
/// created the file would silently invalidate every one of them, so this opens the
/// EXISTING file read-only and reports `Ok(None)` when there is none.
///
/// RESIDUAL RACE (deliberate, documented): flock has no "peek" — the only way to learn
/// that a lock is free is to take it. So a free lock is held by this probe for the
/// microseconds between `flock` and `Unlock`, and a `pmd` whose own startup
/// `try_acquire` lands inside that window would see `WOULDBLOCK` and exit as
/// "already running". The window is ~µs against a probe that runs at most every few
/// seconds, and it is the same window `ensure_daemon` has always opened (it acquires
/// the lock, drops it, then spawns), so this adds no new KIND of failure. Keeping the
/// hold as short as possible is one mitigation; the real one is on the OTHER side —
/// `pmd`'s startup goes through [`acquire_with_retry`], so a probe-contended attempt no
/// longer makes a healthy daemon refuse to start.
///
/// (`fcntl(F_GETLK)` IS a true peek and `rustix` exposes it, but it is a different lock
/// namespace: on Linux `flock(2)` and `fcntl(2)` record locks are independent, so
/// `F_GETLK` cannot see this lock at all. Using it would mean migrating the whole lease
/// protocol across both binaries — rejected as far more risk than the race it removes.)
/// [`try_acquire`], retried a few times before concluding somebody else owns the lock.
///
/// This exists because **a contended attempt is not proof of a live owner.** `flock` has
/// no peek, so every liveness probe in this codebase tests the lock by TAKING it and
/// releasing it microseconds later — [`is_held`] (which pmtui's status bar now runs every
/// few seconds for as long as the dashboard is open) and `pmtui::ensure_daemon` (acquire,
/// drop, spawn). A single-shot `try_acquire` landing inside one of those windows makes a
/// perfectly healthy `pmd` exit as "already running", leaving the human in front of a
/// session that nothing drives — the exact symptom this whole change set is about.
///
/// A retry cannot weaken the singleton: a REAL owner holds the lock for its entire life,
/// so it wins every attempt and this still returns `Ok(None)`. Only a transient µs-scale
/// probe is filtered out, which is precisely the false positive.
pub fn acquire_with_retry(
    lock_path: &Path,
    tries: u32,
    delay: std::time::Duration,
) -> io::Result<Option<ProjectLease>> {
    for attempt in 1..=tries.max(1) {
        match try_acquire(lock_path)? {
            Some(l) => return Ok(Some(l)),
            None if attempt < tries => std::thread::sleep(delay),
            None => {}
        }
    }
    Ok(None)
}

pub fn is_held(path: &Path) -> io::Result<Option<bool>> {
    // Read-only, `create(false)` by omission: an absent lock file stays absent.
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // Taking it proves it was FREE: release IMMEDIATELY (dropping `file` would too, but being
    // explicit keeps the hold as short as the code can make it).
    lock_outcome(flock(&file, FlockOperation::NonBlockingLockExclusive)).map(|taken| {
        if taken {
            let _ = flock(&file, FlockOperation::Unlock);
        }
        Some(!taken)
    })
}

#[cfg(test)]
mod tests;
