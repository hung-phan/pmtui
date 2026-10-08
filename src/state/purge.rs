//! Removing ONE session's state subtree, when its row is gone for good.
//!
//! The rest of this module writes; this is the only thing that deletes a tree, so the guard against
//! deleting the wrong one lives here rather than at the call site. The guard is the LAYOUT itself: the
//! directory must be `<root>/.project-state/sessions/<segment>`, a shape only
//! [`ProjectPaths::for_session`] produces. A root `ProjectPaths` resolves to `.project-state/` — every
//! sibling session's state plus the project's own ledger — and is refused outright.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use super::{ProjectPaths, STATE_DIR};

/// Delete this session's own `.project-state/sessions/<segment>/` subtree.
///
/// Already gone is success: the caller's intent is that it not be there. Anything that is not a real
/// directory — a symlink most of all — is refused rather than followed, because a session's state
/// directory is writable by that session's own agent.
pub fn purge_session_state(paths: &ProjectPaths) -> Result<()> {
    let dir = paths.state_dir();
    if !is_session_subtree(&dir) {
        bail!(
            "{} is not a session's own state directory, so it is not this function's to delete",
            dir.display()
        );
    }
    match std::fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context(format!("inspect {}", dir.display())),
        Ok(meta) if !meta.is_dir() => bail!("{} is not a real directory", dir.display()),
        Ok(_) => {}
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))
}

/// Whether `dir` is `…/.project-state/sessions/<segment>` — the only shape this module deletes.
fn is_session_subtree(dir: &Path) -> bool {
    let named =
        |path: Option<&Path>, name: &str| path.and_then(Path::file_name) == Some(OsStr::new(name));
    let parent = dir.parent();
    dir.file_name().is_some()
        && named(parent, "sessions")
        && named(parent.and_then(Path::parent), STATE_DIR)
}
