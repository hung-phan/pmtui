//! Replacing a running process with the build that is now on disk.
//!
//! An update writes new binaries and nothing else: the processes already loaded keep running the
//! old image, which is a half-finished update — a dashboard reporting the new version while `pmd`
//! drives with the old code. Both binaries need the same two things, so they live here rather than
//! twice: a way to notice that the file underneath them changed, and a way to become it.
//!
//! The two callers reach it differently, on purpose. `pmtui` re-execs because it RAN the installer
//! and knows the moment. `pmd` cannot be told — one `pmd` owns one socket, so there may be several
//! and no dashboard has authority over another pair's daemon — so it watches its own executable
//! instead. Watching is what makes the guarantee hold for EVERY `pmd`, including ones this
//! dashboard never started.

use std::path::{Path, PathBuf};

/// Which file an executable path currently resolves to: `(device, inode)`.
///
/// NOT the modification time. `install.sh` stages a download and `mv -f`s it into place, so the
/// replacement is a rename: the path gains a new inode atomically, and no reader can ever see a
/// half-written binary. An mtime would also work on that path but would be fooled by a `touch` and
/// has a coarser resolution; identity is the thing actually being asked about.
///
/// `None` when the path cannot be stat'ed, which reads as "nothing to compare" everywhere below —
/// never as "it changed", because re-execing on a failed stat would be a restart loop driven by a
/// permissions error.
pub fn identity(exe: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(exe).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// The running executable, or an empty path when the kernel will not say.
///
/// An empty path fails every `stat` and every `exec`, which is exactly the honest outcome: a process
/// that cannot name itself cannot replace itself, and it reports that rather than guessing at a name
/// on `PATH` that might belong to someone else's build.
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

/// Whether `exe` is now a different file than `baseline` recorded.
///
/// Two `None`s are equal and so are two matching pairs, so a process that cannot stat itself simply
/// never restarts.
pub fn replaced(exe: &Path, baseline: Option<(u64, u64)>) -> bool {
    match (baseline, identity(exe)) {
        (Some(before), Some(now)) => before != now,
        _ => false,
    }
}

/// Replace this process with `exe`, keeping the arguments it was given.
///
/// RETURNS ONLY ON FAILURE: on success there is no caller left. The arguments are carried over so a
/// scratch `--registry`/`--socket` process comes back as itself rather than as the default one.
///
/// Callers must hand the terminal back first where they hold it: the new image builds its own
/// alternate screen, and exec'ing out of the old one leaves a screen nothing will ever pop. Open
/// locks are released by the exec itself, because Rust opens files `CLOEXEC` — which is what lets
/// the new image re-acquire the singleton it is replacing.
pub fn relaunch(exe: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec()
}

/// Become the build on disk if it replaced the one running, logging both outcomes as `who`.
///
/// Returns at all only when nothing happened or the restart FAILED — on success there is no caller
/// left. It reports rather than returning a verdict so that the caller is a single statement: the
/// only safe moment to call this is inside a long-running loop that no in-process test can enter, so
/// every branch left at the call site would be a branch nothing can ever cover.
///
/// A failure is logged and execution continues on purpose: for a daemon, the sessions it supervises
/// outrank the build it runs. Both lines go out BEFORE and AFTER the exec respectively, because
/// afterwards there is nobody left to log the first one.
pub fn refresh_or_log(exe: &Path, baseline: Option<(u64, u64)>, who: &str) {
    if !replaced(exe, baseline) {
        return;
    }
    eprintln!("{who}: a new build replaced {} — restarting", exe.display());
    let failure = relaunch(exe);
    eprintln!("{who}: could not restart onto the new build ({failure}) — carrying on");
}

#[cfg(test)]
mod tests;
