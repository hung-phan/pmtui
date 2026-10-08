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
mod tests {
    use super::*;

    // The skill loader parses the frontmatter as YAML. A prose `description:` invites the classic
    // "mapping values are not allowed in this context" trap: an UNQUOTED value that itself contains
    // a colon-space ("the operating rules: you reach ...") is read as a nested mapping and the whole
    // skill fails to load. There is no YAML parser in this crate, so pin the one rule that bites:
    // every frontmatter value is either quoted or free of a `": "`. This is the guard that would
    // have caught the worker skill shipping with an unparseable description.
    #[test]
    fn embedded_skill_frontmatter_has_no_unquoted_colon_in_a_value() {
        for (name, md) in [
            ("worker", WORKER_SKILL_MD),
            ("decider", DECIDER_SKILL_MD),
            ("spawn", SPAWN_SKILL_MD),
        ] {
            let mut lines = md.lines();
            assert_eq!(
                lines.next(),
                Some("---"),
                "{name}: frontmatter opens with ---"
            );
            for line in lines {
                if line == "---" {
                    break; // end of frontmatter
                }
                // Only `key: value` lines carry a value to check; skip blanks / mapping headers.
                let Some((_key, value)) = line.split_once(": ") else {
                    continue;
                };
                let quoted = (value.starts_with('\'') && value.ends_with('\''))
                    || (value.starts_with('"') && value.ends_with('"'));
                assert!(
                    quoted || !value.contains(": "),
                    "{name}: an unquoted colon-space in a frontmatter value breaks YAML \
                     (\"mapping values are not allowed\") — remove the colon or quote the value: \
                     {line}"
                );
            }
        }
    }

    // NON-VACUITY: this fails today because `WORKER_SKILL_MD` does not exist / is empty. It
    // pins that EVERY non-negotiable operating rule and the FULL machine-report schema that
    // Milestone E removes from the nudge actually survives in the skill body — so relocating
    // the protocol out of the nudge cannot silently lose a rule.
    #[test]
    fn worker_skill_carries_the_non_negotiables_and_the_full_schema() {
        let s = WORKER_SKILL_MD;
        // Frontmatter so claude can auto-invoke on the trigger.
        assert!(s.starts_with("---\n"), "has YAML frontmatter");
        assert!(
            s.contains("description:"),
            "has a description for auto-invocation"
        );
        // Comms ownership — the non-negotiable rule that must never be lost. The harness is
        // the channel; NO external integration (e.g. a Slack MCP) may be assumed.
        assert!(s.contains("sends no messages on your behalf"));
        assert!(s.contains("channel to the human"));
        assert!(
            !s.contains("Slack MCP"),
            "the worker skill must not assume a Slack MCP integration exists"
        );
        assert!(
            !s.contains("own all communication"),
            "neither the body nor the frontmatter description may reinstate the 'agent owns \
             comms' framing — the harness is the channel"
        );
        assert!(
            s.contains("working toward the session goal on a heartbeat"),
            "the worker skill must frame the whole recorded brief without implying one atomic \
             objective"
        );
        assert!(
            !s.contains("working ONE goal"),
            "a goal brief may contain multiple objectives"
        );
        assert!(
            s.contains("the whole goal is now SATISFIED"),
            "completion must cover the whole recorded brief"
        );
        assert!(
            !s.contains("the goal's rule"),
            "completion must not imply one atomic rule"
        );
        // You do not decide when done — verbatim invariants relocated from the nudge.
        assert!(s.contains("There is no \"done\" you can set"));
        assert!(s.contains("A human decides when this session is complete"));
        assert!(s.contains("never mark the project finished yourself"));
        assert!(s.contains("or abandon it"));
        assert!(s.contains("Ending a finite wake"));
        // The confirm_done sanction (moved out of the nudge).
        assert!(s.contains("confirm_done"));
        assert!(s.contains("\"state\": \"blocked\""));
        assert!(s.contains("REQUEST FOR CONFIRMATION"));
        // The FULL WakeReport schema fields (moved out of the nudge).
        assert!(s.contains("\"seq\""));
        assert!(s.contains("\"next_step\""));
        assert!(s.contains("\"cadence_s\""));
        assert!(s.contains("\"next_check_s\""));
        assert!(s.contains("working") && s.contains("monitoring") && s.contains("blocked"));
        // Atomicity + audit-only seq + codex conversation_id rules.
        assert!(s.contains("tmp") && s.contains("rename"));
        assert!(s.contains("diagnostic stamp"));
        assert!(s.contains("assigns its own report generation"));
        assert!(s.contains("backward timestamp cannot block"));
        assert!(s.contains("conversation_id"));
        // Pick up your own plan.
        assert!(s.contains("your OWN plan"));
        // The skill must NOT hardcode a marker path (that is per-session, supplied by the nudge).
        assert!(
            !s.contains("/.project-state/"),
            "no baked-in per-session path"
        );
    }

    #[test]
    fn worker_skill_name_and_relpath_are_stable() {
        assert_eq!(WORKER_SKILL_NAME, "agent-manager-worker");
        assert_eq!(
            WORKER_SKILL_REL_PATH,
            ".agents/skills/agent-manager-worker/SKILL.md"
        );
        assert_eq!(
            CODEX_WORKER_SKILL_REL_PATH,
            ".agents/skills/agent-manager-worker/SKILL.md"
        );
        assert_eq!(CLAUDE_SKILLS_REL_PATH, ".claude/skills");
        assert_eq!(
            CLAUDE_WORKER_SKILL_LINK_REL_PATH,
            ".claude/skills/agent-manager-worker"
        );
        assert_eq!(
            CLAUDE_WORKER_SKILL_LINK_TARGET,
            "../../.agents/skills/agent-manager-worker"
        );
    }

    #[test]
    fn decider_skill_formalizes_the_consult_ladder() {
        let s = DECIDER_SKILL_MD;
        assert!(s.starts_with("---\n"), "has YAML frontmatter");
        assert!(s.contains("Auto-approve (act)"));
        assert!(s.contains("refuse"));
        assert!(s.contains("has NOT approved"));
        assert!(s.contains("validated verdict is required"));
        assert!(!s.contains("ALREADY approved"));
        assert!(
            s.contains("SITUATION"),
            "explains how to read the C situation block"
        );
        // verify-don't-defer-to-precedent (C's core anti-rubber-stamp rule).
        assert!(s.contains("verify"));
        assert!(s.contains("precedent"));
        // It must not weaken the read-only, two-writers rule.
        assert!(s.contains("READ-ONLY"));
        assert!(s.contains("cannot write"));
    }
}
