//! Confirmed handoff of the single dashboard lock.

use std::fs;
#[cfg(not(test))]
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use agent_manager::clock::{Clock, SystemClock};
use agent_manager::{job_engine, lease, state};

const OWNER_SCHEMA_VERSION: u32 = 1;
const OWNER_MAX_BYTES: u64 = 4 * 1024;
#[cfg(not(test))]
const HANDOFF_TRIES: u32 = 100;
#[cfg(test)]
const HANDOFF_TRIES: u32 = 2;
#[cfg(not(test))]
const FORCE_TRIES: u32 = 60;
#[cfg(test)]
const FORCE_TRIES: u32 = 2;
const HANDOFF_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DashboardOwner {
    schema_version: u32,
    pub(crate) pid: u32,
    pub(crate) nonce: String,
    pub(crate) started_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TakeoverRequest {
    owner_nonce: String,
    requested_by_pid: u32,
    requested_at: i64,
}

#[derive(Debug)]
pub(crate) struct DashboardOwnership {
    _lease: lease::ProjectLease,
    pub(crate) owner: DashboardOwner,
}

#[derive(Debug)]
pub(crate) struct DashboardClaim {
    pub(crate) proceed: bool,
    pub(crate) ownership: Option<DashboardOwnership>,
}

pub(crate) type ForceTakeover<'a> =
    dyn FnMut(&Path, &str, &DashboardOwner) -> Result<Option<DashboardOwnership>> + 'a;

impl DashboardOwner {
    fn current() -> Self {
        Self {
            schema_version: OWNER_SCHEMA_VERSION,
            pid: std::process::id(),
            nonce: job_engine::mint_uuid_v4(),
            started_at: SystemClock.now(),
        }
    }
}

pub(crate) fn acquire_dashboard(
    registry: &Path,
    socket: &str,
    confirm: &mut dyn FnMut(&str) -> Result<bool>,
    force: &mut ForceTakeover<'_>,
) -> Result<DashboardClaim> {
    match claim(registry, socket) {
        Ok(Some(ownership)) => Ok(DashboardClaim {
            proceed: true,
            ownership: Some(ownership),
        }),
        Ok(None) => Ok(
            match confirmed_dashboard_takeover(registry, socket, confirm, force)? {
                Some(ownership) => DashboardClaim {
                    proceed: true,
                    ownership: Some(ownership),
                },
                None => DashboardClaim {
                    proceed: false,
                    ownership: None,
                },
            },
        ),
        Err(error) => {
            eprintln!("pmtui: could not check the single-instance lock ({error}); continuing");
            Ok(DashboardClaim {
                proceed: true,
                ownership: None,
            })
        }
    }
}

#[cfg(not(test))]
pub(crate) fn confirm_terminal(prompt: &str) -> Result<bool> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprintln!(
            "Only one dashboard may drive a registry at a time. Run pmtui from an interactive \
             terminal to confirm takeover."
        );
        return Ok(false);
    }
    eprint!("{prompt}");
    std::io::stderr().flush().context("flush takeover prompt")?;
    let mut answer = String::new();
    stdin
        .read_line(&mut answer)
        .context("read takeover confirmation")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
pub(crate) fn confirm_terminal(_prompt: &str) -> Result<bool> {
    Ok(false)
}

pub(crate) fn confirmed_dashboard_takeover(
    registry: &Path,
    socket: &str,
    confirm: &mut dyn FnMut(&str) -> Result<bool>,
    force: &mut ForceTakeover<'_>,
) -> Result<Option<DashboardOwnership>> {
    let owner = match read_owner(registry, socket) {
        Ok(Some(owner)) => owner,
        Ok(None) => {
            eprintln!(
                "pmtui is already running for registry {} on socket {socket}.\n\
                 Its owner predates confirmed takeover metadata, so it cannot be stopped safely.\n\
                 Attach to it or terminate that pmtui once manually.",
                registry.display()
            );
            return Ok(None);
        }
        Err(error) => {
            eprintln!(
                "pmtui is already running for registry {} on socket {socket}, but its \
                 owner could not be identified: {error}",
                registry.display()
            );
            return Ok(None);
        }
    };

    eprintln!(
        "pmtui is already running (PID {}) for registry {} on socket {socket}.",
        owner.pid,
        registry.display()
    );
    if !confirm("Stop it cleanly and take over? [y/N] ")? {
        eprintln!("Takeover cancelled; the running pmtui was left unchanged.");
        return Ok(None);
    }

    request_handoff(registry, socket, &owner)?;
    eprintln!("Waiting for the running pmtui to hand over...");
    match wait_for_handoff(registry, socket) {
        Ok(Some(ownership)) => return Ok(Some(ownership)),
        Ok(None) => {}
        Err(error) => {
            let _ = cancel_handoff(registry, socket, &owner);
            return Err(error);
        }
    }

    eprintln!("The running pmtui did not respond to the clean handoff request.");
    let force_confirmed = match confirm(
        "Force-terminate that pmtui and take over? This may require `reset` in its old terminal. [y/N] ",
    ) {
        Ok(confirmed) => confirmed,
        Err(error) => {
            let _ = cancel_handoff(registry, socket, &owner);
            return Err(error);
        }
    };
    if !force_confirmed {
        cancel_handoff(registry, socket, &owner)?;
        eprintln!("Force takeover cancelled; the running pmtui was left unchanged.");
        return Ok(None);
    }
    if let Some(ownership) = claim(registry, socket)? {
        return Ok(Some(ownership));
    }
    let forced = force(registry, socket, &owner);
    let forced = match forced {
        Ok(ownership) => ownership,
        Err(error) => {
            let _ = cancel_handoff(registry, socket, &owner);
            return Err(error);
        }
    };
    match forced {
        Some(ownership) => Ok(Some(ownership)),
        None => {
            cancel_handoff(registry, socket, &owner)?;
            eprintln!(
                "The old pmtui did not release its lock; refusing to start a rival dashboard."
            );
            Ok(None)
        }
    }
}

pub(crate) fn claim(registry: &Path, socket: &str) -> Result<Option<DashboardOwnership>> {
    let lock = lease::pmtui_lock_path(registry, socket);
    let request = lease::pmtui_takeover_path(registry, socket);
    let lease = lease::try_acquire(&lock).context("check the pmtui single-instance lock")?;
    lease
        .map(|lease| finish_acquire(lease, &lock, &request))
        .transpose()
}

pub(crate) fn read_owner(registry: &Path, socket: &str) -> Result<Option<DashboardOwner>> {
    let path = lease::pmtui_lock_path(registry, socket);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect the running pmtui owner"),
    };
    if metadata.len() == 0 {
        return Ok(None);
    }
    if metadata.len() > OWNER_MAX_BYTES {
        bail!("pmtui owner metadata exceeds {OWNER_MAX_BYTES} bytes");
    }
    let bytes = fs::read(&path).context("read the running pmtui owner")?;
    let owner = serde_json::from_slice(&bytes).context("parse the running pmtui owner")?;
    Ok(Some(owner))
}

pub(crate) fn request_handoff(registry: &Path, socket: &str, owner: &DashboardOwner) -> Result<()> {
    let request = TakeoverRequest {
        owner_nonce: owner.nonce.clone(),
        requested_by_pid: std::process::id(),
        requested_at: SystemClock.now(),
    };
    state::write_json_atomic(&lease::pmtui_takeover_path(registry, socket), &request)
        .context("write the pmtui takeover request")
}

pub(crate) fn requested_for(registry: &Path, socket: &str, owner_nonce: &str) -> bool {
    state::read_json_opt::<TakeoverRequest>(&lease::pmtui_takeover_path(registry, socket))
        .ok()
        .flatten()
        .is_some_and(|request| request.owner_nonce == owner_nonce)
}

pub(crate) fn cancel_handoff(registry: &Path, socket: &str, owner: &DashboardOwner) -> Result<()> {
    let path = lease::pmtui_takeover_path(registry, socket);
    let request = state::read_json_opt::<TakeoverRequest>(&path)
        .context("read the pmtui takeover request before cancellation")?;
    let ours = request.is_some_and(|request| {
        request.owner_nonce == owner.nonce && request.requested_by_pid == std::process::id()
    });
    if !ours {
        return Ok(());
    }
    clear_request_path(&path)
}

pub(crate) fn clear_request_path(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("clear the pmtui takeover request"),
    }
}

pub(crate) fn wait_for_handoff(
    registry: &Path,
    socket: &str,
) -> Result<Option<DashboardOwnership>> {
    wait_for_lock(registry, socket, HANDOFF_TRIES)
}

pub(crate) fn force_owner_and_wait(
    registry: &Path,
    socket: &str,
    expected: &DashboardOwner,
) -> Result<Option<DashboardOwnership>> {
    if expected.pid <= 1 || expected.pid > i32::MAX as u32 {
        bail!(
            "refusing to signal unsafe recorded pmtui PID {}",
            expected.pid
        );
    }
    force_owner_and_wait_with(registry, socket, expected, |pid| {
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .context("send SIGTERM to the old pmtui")?;
        if !status.success() {
            bail!("kill -TERM {pid} exited with {status}");
        }
        Ok(())
    })
}

pub(crate) fn force_owner_and_wait_with(
    registry: &Path,
    socket: &str,
    expected: &DashboardOwner,
    mut signal: impl FnMut(u32) -> Result<()>,
) -> Result<Option<DashboardOwnership>> {
    let current = read_owner(registry, socket)?;
    if current.as_ref() != Some(expected) {
        bail!("the running pmtui owner changed before force termination");
    }
    if lease::is_held(&lease::pmtui_lock_path(registry, socket))? != Some(true) {
        bail!("the pmtui lock is no longer held by the expected owner");
    }
    signal(expected.pid)?;
    wait_for_lock(registry, socket, FORCE_TRIES)
}

fn wait_for_lock(registry: &Path, socket: &str, tries: u32) -> Result<Option<DashboardOwnership>> {
    let lock = lease::pmtui_lock_path(registry, socket);
    let request = lease::pmtui_takeover_path(registry, socket);
    let lease = lease::acquire_with_retry(&lock, tries, HANDOFF_POLL)
        .context("wait for the old pmtui to release its lock")?;
    lease
        .map(|lease| finish_acquire(lease, &lock, &request))
        .transpose()
}

fn finish_acquire(
    lease: lease::ProjectLease,
    lock_path: &Path,
    request_path: &Path,
) -> Result<DashboardOwnership> {
    let owner = DashboardOwner::current();
    let bytes = serde_json::to_vec(&owner).context("serialize the pmtui owner")?;
    fs::write(lock_path, bytes).context("record the pmtui lock owner")?;
    clear_request_path(request_path)?;
    Ok(DashboardOwnership {
        _lease: lease,
        owner,
    })
}
