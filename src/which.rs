//! "Is this program here?" — asked once, for the whole tool.
//!
//! There were THREE of these, and two of them answered the same question differently. The supervisor's
//! copy returned "yes" when `PATH` could not be read at all, on purpose: *I could not tell* must not
//! become *switch the feature off*. The tmux driver's copy returned "no" in the same situation, and it
//! gates a launch — so a `pmd` started from a systemd or launchd unit with no `PATH` in its environment
//! got both answers inside one process: the launch pre-check reported the agent was not installed while
//! the decider kept consulting as though it were. The doctor's copy made three, and `executable_file`
//! was duplicated word for word between two of them.
//!
//! So the unreadable-`PATH` case is a VALUE here, not a convention each caller re-chooses in silence.
//! Returning a plain `bool` is what let the two drift; [`Found::PathUnknown`] makes every caller say
//! out loud what it does with "I could not tell".
//!
//! Hand-rolled rather than shelling out to `which`/`command -v`: the daemon asks this inside its 500ms
//! sweep, and spawning a subprocess to find out whether we can spawn a subprocess is the wrong trade.

use std::ffi::OsStr;
use std::path::Path;

/// What a `PATH` search found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// An executable file of that name is there.
    Yes,
    /// `PATH` was searched and holds no such executable.
    No,
    /// There was no `PATH` to search, so this is not an answer about the program at all. Every caller
    /// decides what to do with it; none may pretend it is [`Found::No`].
    PathUnknown,
}

impl Found {
    /// Read "I could not tell" as `yes`. The FAIL-OPEN reading, for a caller whose alternative is to
    /// disable itself: refusing to launch an agent, or switching off a consult, because this process's
    /// environment is missing `PATH` is worse than trying and reporting the real error.
    pub fn or_unknown(self) -> bool {
        matches!(self, Self::Yes | Self::PathUnknown)
    }

    /// Read "I could not tell" as `no`. For a caller that must not claim a program is present on
    /// evidence it does not have.
    pub fn is_yes(self) -> bool {
        self == Self::Yes
    }
}

/// Whether `name` resolves to an executable file, searching `path`.
///
/// A `name` containing a separator is probed as-is, mirroring how a shell resolves anything that looks
/// like a location rather than a bare command — so it never reports [`Found::PathUnknown`].
pub fn on_path(name: &str, path: Option<&OsStr>) -> Found {
    if name.contains(std::path::MAIN_SEPARATOR) {
        return if is_executable(Path::new(name)) {
            Found::Yes
        } else {
            Found::No
        };
    }
    let Some(path) = path else {
        return Found::PathUnknown;
    };
    if std::env::split_paths(path).any(|dir| is_executable(&dir.join(name))) {
        Found::Yes
    } else {
        Found::No
    }
}

/// [`on_path`] against this process's own `PATH`.
pub fn on_env_path(name: &str) -> Found {
    on_path(name, std::env::var_os("PATH").as_deref())
}

/// Whether `path` is a file this process could execute.
///
/// The executable BIT is checked on unix, because a same-named directory or a non-executable data file
/// sitting on `PATH` would otherwise read as "the program is installed".
pub fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && executable_bit(&metadata)
}

/// The execute-permission half of [`is_executable`], split out so the `cfg` covers one expression
/// instead of a function body with two tails.
#[cfg(unix)]
fn executable_bit(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_bit(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests;
