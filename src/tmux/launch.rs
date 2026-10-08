//! The typed vocabulary of an interactive launch: the [`ManagedEnv`] every managed terminal
//! is started with, the [`LaunchOutcome`] of a launch that went through, and the
//! [`LaunchError`] of one that did not.
//!
//! The error is typed, not a string, because the one question a caller about to retry must
//! answer is "could the agent have started?". [`LaunchError::proven_not_started`] answers it:
//! only a launch refused before tmux ever saw our argv is proven harmless to redo. Every other
//! failure may have left an agent that read its initial Message, so a caller must never
//! launch that Message again.

use std::fmt;
use std::path::{Path, PathBuf};

use super::real::LAUNCH_COMMAND_MAX_BYTES;

/// The environment variable naming the managed session's stable id.
pub const ENV_SESSION: &str = "PMTUI_SESSION";
/// The environment variable naming the managed session's state directory
/// (`<root>/.project-state/sessions/<id>-<fnv8>`).
pub const ENV_STATE_DIR: &str = "PMTUI_STATE_DIR";
/// The environment variable naming the canonical `pmtui` executable.
pub const ENV_BIN: &str = "PMTUI_BIN";

/// Who a managed terminal belongs to, handed to the agent inside it through its environment.
///
/// Routing, not authentication: an agent learns which session it is in and where that
/// session's state lives, so it can address requests to its own state directory. The driver
/// cannot infer any of it (a [`TmuxDriver`](super::TmuxDriver) knows only its tmux binary and
/// socket), so every caller of [`Driver::launch_interactive`](super::Driver::launch_interactive)
/// supplies it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManagedEnv {
    pub session_id: String,
    pub state_dir: PathBuf,
    pub pmtui_bin: Option<PathBuf>,
}

impl ManagedEnv {
    /// The `(KEY, VALUE)` pairs applied with `tmux new-session -e KEY=VALUE`, in this order:
    /// PMTUI_SESSION, PMTUI_STATE_DIR, then PMTUI_BIN when `pmtui_bin` is Some.
    pub fn vars(&self) -> Vec<(String, String)> {
        let mut vars = vec![
            (ENV_SESSION.to_string(), self.session_id.clone()),
            (
                ENV_STATE_DIR.to_string(),
                self.state_dir.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(bin) = &self.pmtui_bin {
            vars.push((ENV_BIN.to_string(), bin.to_string_lossy().into_owned()));
        }
        vars
    }
}

/// A launch that did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// A new session was created with our argv and was alive on the re-check.
    Started,
    /// A session with that name already existed (before our `new-session`, or created by a
    /// racing creator after it), so our argv was not used.
    AlreadyAlive,
}

/// A launch that failed, classified by whether an agent could have started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// No argv to run; tmux was never invoked.
    EmptyArgv,
    /// The engine binary (the payload) is not on PATH; tmux was never invoked.
    NotOnPath(String),
    /// The shell-quoted command exceeds [`LAUNCH_COMMAND_MAX_BYTES`]; tmux was never invoked.
    CommandTooLong { session: String, bytes: usize },
    /// `tmux new-session` exited non-zero and the session is absent. Ambiguous: the session
    /// may have existed briefly. The payload is the message shown to the human.
    NewSessionFailed(String),
    /// The session was created, then was gone on the re-check. Ambiguous: the agent may have
    /// read its argv. The payload is the message shown to the human.
    ExitedAfterStart(String),
    /// An is_alive/tmux I/O error: ambiguous. The payload is the whole cause chain.
    Probe(String),
}

impl LaunchError {
    /// True only when tmux was never invoked with our argv: EmptyArgv, NotOnPath, CommandTooLong.
    pub fn proven_not_started(&self) -> bool {
        matches!(
            self,
            Self::EmptyArgv | Self::NotOnPath(_) | Self::CommandTooLong { .. }
        )
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArgv => f.write_str("launch_interactive: empty argv"),
            Self::NotOnPath(bin) => write!(f, "{bin:?} was not found on PATH — is it installed?"),
            Self::CommandTooLong { session, bytes } => write!(
                f,
                "launch command for {session} is {bytes} bytes after shell quoting; tmux accepts at most {LAUNCH_COMMAND_MAX_BYTES}"
            ),
            Self::NewSessionFailed(message)
            | Self::ExitedAfterStart(message)
            | Self::Probe(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Canonical pmtui path for a pmtui process (`current_exe`) or pmd (its sibling `pmtui`, None if absent).
pub fn pmtui_bin_for_current_process() -> Option<PathBuf> {
    pmtui_bin_for_exe(&std::env::current_exe().ok()?)
}

/// [`pmtui_bin_for_current_process`] for the executable at `exe`: `exe` itself when it is
/// named `pmtui`, otherwise the `pmtui` beside it. Either way the answer is the canonical path
/// of an executable regular file, or `None`, so a stale or missing install is omitted rather
/// than handed to an agent that would fail to run it.
pub(crate) fn pmtui_bin_for_exe(exe: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidate = if exe.file_name().is_some_and(|name| name == "pmtui") {
        exe.to_path_buf()
    } else {
        exe.with_file_name("pmtui")
    };
    let canonical = std::fs::canonicalize(candidate).ok()?;
    let runnable = std::fs::metadata(&canonical)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0);
    runnable.then_some(canonical)
}
