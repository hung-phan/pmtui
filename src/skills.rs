//! Skill bodies shipped with agent-manager, embedded at build time (mirrors
//! `src/prompt.rs`'s `playbook/*.md`). The markdown under `skills/` is the reviewable source
//! of truth; these consts are how the daemon and dashboard deliver it (worker and spawn: written
//! natively into the session root's canonical `.agents/skills` directory, plus a Claude per-skill
//! symlink; decider: appended to the consult system prompt).

use std::io;
use std::path::{Path, PathBuf};

use crate::registry::Engine;
use crate::state::ProjectPaths;
use crate::tmux::ManagedEnv;

/// The worker loop protocol Milestone E moves OUT of the per-wake nudge and into the canonical
/// project skill auto-discovered by either engine.
pub const WORKER_SKILL_MD: &str = include_str!("../skills/agent-manager-worker/SKILL.md");

/// The supervisor-consult reasoning protocol (Milestone E, Task 5). Appended to the consult's
/// `--system-prompt` because the consult runs `--bare` (no skill auto-discovery).
pub const DECIDER_SKILL_MD: &str = include_str!("../skills/agent-manager-decider/SKILL.md");

/// The worker skill name the nudge triggers by.
pub const WORKER_SKILL_NAME: &str = "agent-manager-worker";

/// The canonical worker SKILL.md path relative to a project root. Codex discovers this directly;
/// Claude reaches the same directory through [`CLAUDE_WORKER_SKILL_LINK_REL_PATH`].
pub const WORKER_SKILL_REL_PATH: &str = ".agents/skills/agent-manager-worker/SKILL.md";

/// Codex's canonical repository skill path.
pub const CODEX_WORKER_SKILL_REL_PATH: &str = ".agents/skills/agent-manager-worker/SKILL.md";

/// Claude's project skills directory relative to the project root.
pub const CLAUDE_SKILLS_REL_PATH: &str = ".claude/skills";

/// Claude's project-skill directory alias relative to the project root.
pub const CLAUDE_WORKER_SKILL_LINK_REL_PATH: &str = ".claude/skills/agent-manager-worker";

/// Relative target from `.claude/skills/` to the canonical skill directory.
pub const CLAUDE_WORKER_SKILL_LINK_TARGET: &str = "../../.agents/skills/agent-manager-worker";

/// The procedure an agent inside a managed session follows to hand independent work to a new
/// Standard session through `"$PMTUI_BIN" spawn`. Shipped by the process that launches the
/// terminal, only when that launch names pmtui ([`install_spawn_skill_for_launch`]).
pub const SPAWN_SKILL_MD: &str = include_str!("../skills/pmtui-spawn/SKILL.md");

/// The spawn skill's name, and its directory under `.agents/skills/` and `.claude/skills/`.
pub const SPAWN_SKILL_NAME: &str = "pmtui-spawn";

/// The canonical spawn SKILL.md path relative to a project root, shared by Claude and Codex.
pub const SPAWN_SKILL_REL_PATH: &str = ".agents/skills/pmtui-spawn/SKILL.md";

/// Claude's spawn-skill directory alias relative to the project root.
pub const CLAUDE_SPAWN_SKILL_LINK_REL_PATH: &str = ".claude/skills/pmtui-spawn";

/// Relative target from `.claude/skills/` to the canonical spawn skill directory.
pub const CLAUDE_SPAWN_SKILL_LINK_TARGET: &str = "../../.agents/skills/pmtui-spawn";

/// Relative target from `.claude/skills/` to the canonical directory of the skill `name`.
pub fn claude_skill_link_target(name: &str) -> PathBuf {
    Path::new("../../.agents/skills").join(name)
}

/// Whether Claude reaches the canonical `.agents` skill `name` through a safe compatibility link.
fn claude_skill_link_is_current(paths: &ProjectPaths, name: &str) -> bool {
    if is_symlink(&paths.claude_root_dir()) {
        return false;
    }
    if is_symlink(&paths.claude_skills_dir()) {
        return false;
    }
    std::fs::read_link(paths.claude_skill_dir(name))
        .is_ok_and(|target| target == claude_skill_link_target(name))
}

/// Ensure Claude discovers the canonical worker skill without maintaining a second copy.
///
/// pmd owns the exact `agent-manager-worker` skill path and replaces any previous file, directory,
/// or stale link there. Parent Claude configuration and sibling skills are never modified.
pub fn ensure_claude_worker_skill_link(paths: &ProjectPaths) -> io::Result<()> {
    ensure_claude_skill_link(paths, WORKER_SKILL_NAME)
}

/// Ensure Claude discovers the canonical skill `name` through its exact per-skill alias
/// `.claude/skills/<name>` → `../../.agents/skills/<name>`, without maintaining a second copy.
///
/// The launching process owns that exact alias path and replaces any previous file, directory, or
/// stale link there. A symlinked or non-directory `.claude` or `.claude/skills` is refused, and
/// parent Claude configuration and sibling skills are never modified.
pub fn ensure_claude_skill_link(paths: &ProjectPaths, name: &str) -> io::Result<()> {
    if claude_skill_link_is_current(paths, name) {
        return Ok(());
    }

    let claude_root = paths.claude_root_dir();
    match std::fs::symlink_metadata(&claude_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(path_conflict(
                "symlinked Claude project directory",
                &claude_root,
            ));
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(path_conflict(
                "non-directory Claude project path",
                &claude_root,
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&claude_root)?;
        }
        Err(error) => return Err(error),
    }

    let skills_dir = paths.claude_skills_dir();
    match std::fs::symlink_metadata(&skills_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(path_conflict(
                "noncanonical Claude skills symlink",
                &skills_dir,
            ));
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(path_conflict(
                "non-directory Claude skills path",
                &skills_dir,
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir(&skills_dir)?;
        }
        Err(error) => return Err(error),
    }

    let link = paths.claude_skill_dir(name);
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&link)?,
        Ok(_) => std::fs::remove_file(&link)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // Direct creation is atomic and fails without overwriting if another path appears first.
    std::os::unix::fs::symlink(claude_skill_link_target(name), &link).map_err(|error| {
        path_io_error(&format!("create Claude {name} skill symlink"), &link, error)
    })?;
    Ok(())
}

/// Install the spawn skill in `paths.root`: write the canonical
/// `.agents/skills/pmtui-spawn/SKILL.md` (atomically, replacing a stale copy) and, for Claude,
/// ensure its exact `.claude/skills/pmtui-spawn` alias. NEVER creates or modifies the user's
/// `AGENTS.md`, and never touches the worker skill or any sibling skill.
pub fn install_spawn_skill(paths: &ProjectPaths, engine: Engine) -> anyhow::Result<()> {
    // Creates `.agents/skills/pmtui-spawn/` as needed; the temp+rename never tears the file.
    crate::state::write_text_atomic(&paths.canonical_spawn_skill_file(), SPAWN_SKILL_MD)?;
    if engine == Engine::Claude {
        ensure_claude_skill_link(paths, SPAWN_SKILL_NAME)?;
    }
    Ok(())
}

/// The shared pre-launch installer every pmtui and pmd launch site calls just before
/// `launch_interactive`: install the spawn skill into `root` for a terminal about to start with
/// `env`, but only when `env` names pmtui (`PMTUI_BIN`). The skill's command runs `"$PMTUI_BIN"`,
/// so a launch without it gets nothing and returns `Ok(())`.
///
/// Callers treat a failure as non-fatal: the launch proceeds, and `pmd doctor` reports the
/// missing or stale skill.
pub fn install_spawn_skill_for_launch(
    root: &Path,
    engine: Engine,
    env: &ManagedEnv,
) -> anyhow::Result<()> {
    if env.pmtui_bin.is_none() {
        return Ok(());
    }
    install_spawn_skill(&ProjectPaths::new(root), engine)
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

fn path_conflict(kind: &str, path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("refusing to replace {kind}: {}", path.display()),
    )
}

fn path_io_error(action: &str, path: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{action} at {}: {error}", path.display()),
    )
}

#[cfg(test)]
#[path = "skills/tests/freshness.rs"]
mod freshness_tests;

#[cfg(test)]
#[path = "skills/tests/worker_protocol.rs"]
mod worker_protocol_tests;

#[cfg(test)]
#[path = "skills/tests/option_voice.rs"]
mod option_voice_tests;

#[cfg(test)]
#[path = "skills/tests/spawn.rs"]
mod spawn_tests;

#[cfg(test)]
#[path = "skills/tests/skill_files.rs"]
mod tests;
