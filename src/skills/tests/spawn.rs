//! The pmtui-spawn skill: the procedure an agent follows to hand independent work to a new
//! Standard session, and the shared pre-launch installer that ships it into a session's project
//! root only when the terminal names pmtui (`PMTUI_BIN`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::Engine;
use crate::state::ProjectPaths;
use crate::tmux::ManagedEnv;

use super::{
    CLAUDE_SPAWN_SKILL_LINK_REL_PATH, CLAUDE_SPAWN_SKILL_LINK_TARGET,
    CLAUDE_WORKER_SKILL_LINK_TARGET, SPAWN_SKILL_MD, SPAWN_SKILL_NAME, SPAWN_SKILL_REL_PATH,
    WORKER_SKILL_MD, claude_skill_link_target, ensure_claude_worker_skill_link,
    install_spawn_skill, install_spawn_skill_for_launch,
};

fn launch_env(bin: Option<&str>) -> ManagedEnv {
    ManagedEnv {
        session_id: "bot".into(),
        state_dir: PathBuf::from("/unused/.project-state/sessions/bot-00000000"),
        pmtui_bin: bin.map(PathBuf::from),
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

#[test]
fn spawn_skill_body_names_the_command_request_id_and_every_state() {
    let s = SPAWN_SKILL_MD;
    let prose = s.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(s.starts_with("---\n"), "has YAML frontmatter");
    assert!(
        s.contains("\nname: pmtui-spawn\n"),
        "frontmatter names the skill"
    );
    assert!(
        s.contains("\ndescription: "),
        "has a description for auto-invocation"
    );

    // Step 3: one generated request id, then the exact command with that id.
    assert!(s.contains("id=$(cat /proc/sys/kernel/random/uuid"));
    // `--wait` is IN the command: it is what makes one call come back with the child's result instead
    // of with "it started", which is the difference between reading a receipt and reading a transcript.
    assert!(s.contains(
        "\"$PMTUI_BIN\" spawn --request-id \"$id\" --title \"<short title>\" --message \
         \"<Target / Change / Constraints / Ownership / Acceptance>\" --wait 120 --json"
    ));
    // A lowercase id on a host without /proc (macOS): `is_valid_request_id` is lowercase only.
    assert!(s.contains("uuidgen | tr 'A-F' 'a-f'"));

    // Step 4: stdout JSON is the answer whatever the exit code, and every printed state has a rule.
    // EVERY JOB OUTCOME IS HERE, because the whole point of the receipt is that a parent reads the
    // answer from it. A skill that stopped at `ready` is what sent one to read its child's transcript.
    assert!(prose.contains("Parse the JSON on stdout even when the exit code is nonzero."));
    for state in [
        "`done`",
        "`needs_human`",
        "`ended_without_result`",
        "`cancelled`",
        "`queued`",
        "`in_progress`",
        "`needs_attention`",
        "`failed`",
        "`outcome_unknown`",
    ] {
        assert!(
            s.contains(state),
            "the procedure must say what {state} means"
        );
    }
    // What to DO with the answer, not merely that it exists.
    assert!(
        prose.contains("report `result.summary` to the human"),
        "a finished job's summary must be reported"
    );
    assert!(
        prose.contains("relay `result.summary` as the child's question"),
        "a job that needs a person must be relayed, not re-dispatched"
    );
    // HOW TO CHECK BACK when the length is unknown: one listing for every child, and the same id when
    // it waits again. A skill that only said "wait longer" would be asking a parent to guess.
    // FAN OUT, THEN ASK. Each dispatch waits, so a skill that only showed the single-child call had an
    // agent run five independent children in series — seen in a real session.
    assert!(
        prose.contains("dispatch them all first"),
        "spawning several must say to dispatch before waiting"
    );
    assert!(
        prose.contains("--wait 0"),
        "the fan-out needs the flag that does not wait"
    );
    assert!(
        prose.contains("--status"),
        "the procedure must say how to ask about every child"
    );
    assert!(
        prose.contains("no ids to remember"),
        "the listing's point is that it needs no bookkeeping"
    );
    assert!(
        prose.contains("never make a new id for the same task"),
        "an unfinished job is checked again under its own id"
    );
    assert!(prose.contains("Never spawn a replacement."));
    assert!(s.contains("`error.code`"));

    // `failed` is acted on by its code: only a fixed argument earns a new id.
    assert!(!prose.contains("A new id is fine"), "no blanket retry rule");
    for (codes, rule) in [
        (
            &["`request_conflict`"][..],
            "Rerun the command with the original arguments and the same `--request-id` to check \
             on it. Never make a new id for the same task.",
        ),
        (
            &["`request_unwritable`"][..],
            "rerun the same command, with the same `--request-id`, later.",
        ),
        (
            &[
                "`nested_spawn_refused`",
                "`child_limit_reached`",
                "`dir_not_allowed`",
            ][..],
            "stop and ask the human.",
        ),
        (
            &[
                "`invalid_argument`",
                "`dir_not_found`",
                "`message_too_long`",
                "`invalid_request`",
            ][..],
            "nothing ran. Fix the argument, then retry with a new id.",
        ),
    ] {
        let line = prose
            .split(" - ")
            .find(|item| codes.iter().all(|code| item.contains(code)))
            .unwrap_or_else(|| panic!("no rule names {codes:?}"));
        assert!(line.contains(rule), "{codes:?} must say {rule:?}: {line}");
    }
    assert!(prose.contains("Any other code: stop and tell the human the code and its message."));

    // Steps 1, 2 and 5.
    assert!(prose.contains("Spawn only independent work that can proceed without you."));
    assert!(prose.contains("Do the rest yourself."));
    for part in [
        "target",
        "change",
        "constraints",
        "ownership",
        "observable acceptance",
    ] {
        assert!(
            prose.contains(part),
            "the Message spec must name its {part}"
        );
    }
    assert!(prose.contains("A job cannot spawn."));
    // THE LIMIT IS CONCURRENCY, not a cap on asking: over it, a request waits. Telling an agent to stop
    // and ask a human made it choose between doing that work itself and interrupting someone.
    assert!(
        prose.contains("wait their turn"),
        "the skill must say that extra children queue"
    );
    assert!(
        !prose.contains("stop and ask the human.\n"),
        "and must not still send an agent to a human at the fifth child"
    );
    // A WORKTREE IS WHERE THE WORK IS. A parent that does not know its child committed elsewhere reports
    // "done" and leaves a human looking at an unchanged checkout.
    assert!(
        prose.contains("work.commit") && prose.contains("work.branch"),
        "the skill must say where a child's commit is"
    );
    assert!(
        prose.contains("NOT in\n     the human's checkout") || prose.contains("NOT in the human's"),
        "and that applying it is the human's step"
    );
    // Cancel is part of the procedure: a parent that cannot stop its child has no bound on it at all.
    assert!(
        prose.contains("--cancel"),
        "the procedure must say how to stop a child"
    );

    // The skill is shared by every session in a project root: no per-session path is baked in.
    assert!(
        !s.contains("/.project-state/"),
        "no baked-in per-session path"
    );
}

#[test]
fn spawn_skill_is_exactly_its_seven_numbered_steps() {
    let numbered: Vec<&str> = SPAWN_SKILL_MD
        .lines()
        .filter_map(|line| {
            let (number, _) = line.trim_start().split_once(". ")?;
            (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then_some(number)
        })
        .collect();
    assert_eq!(numbered, ["1", "2", "3", "4", "5", "6", "7"]);
}

#[test]
fn spawn_skill_name_and_paths_are_stable() {
    assert_eq!(SPAWN_SKILL_NAME, "pmtui-spawn");
    assert_eq!(SPAWN_SKILL_REL_PATH, ".agents/skills/pmtui-spawn/SKILL.md");
    assert_eq!(
        CLAUDE_SPAWN_SKILL_LINK_REL_PATH,
        ".claude/skills/pmtui-spawn"
    );
    assert_eq!(
        CLAUDE_SPAWN_SKILL_LINK_TARGET,
        "../../.agents/skills/pmtui-spawn"
    );
    assert_eq!(
        claude_skill_link_target(SPAWN_SKILL_NAME),
        Path::new(CLAUDE_SPAWN_SKILL_LINK_TARGET)
    );
    assert_eq!(
        claude_skill_link_target(super::WORKER_SKILL_NAME),
        Path::new(CLAUDE_WORKER_SKILL_LINK_TARGET),
        "the generalized link helper keeps the worker's exact target"
    );
}

#[test]
fn claude_install_writes_the_canonical_body_and_its_exact_alias() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");

    install_spawn_skill(&paths, Engine::Claude).unwrap();

    assert_eq!(
        fs::read_to_string(paths.canonical_spawn_skill_file()).unwrap(),
        SPAWN_SKILL_MD
    );
    let link = paths.claude_spawn_skill_dir();
    assert!(
        is_symlink(&link),
        "Claude reaches the canonical copy through a symlink"
    );
    assert_eq!(
        fs::read_link(&link).unwrap(),
        PathBuf::from(CLAUDE_SPAWN_SKILL_LINK_TARGET)
    );
    assert_eq!(
        fs::read_to_string(link.join("SKILL.md")).unwrap(),
        SPAWN_SKILL_MD
    );
    assert!(
        !dir.path().join("AGENTS.md").exists(),
        "the installer never creates the user's AGENTS.md"
    );
}

#[test]
fn codex_install_writes_only_the_canonical_body() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");

    install_spawn_skill(&paths, Engine::Codex).unwrap();

    assert_eq!(
        fs::read_to_string(paths.canonical_spawn_skill_file()).unwrap(),
        SPAWN_SKILL_MD
    );
    assert!(
        !paths.claude_root_dir().exists(),
        "Codex discovers .agents/skills natively and needs no Claude directory"
    );
}

#[test]
fn install_refreshes_a_stale_body_and_leaves_the_worker_skill_alone() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    let worker = paths.canonical_worker_skill_file();
    fs::create_dir_all(worker.parent().unwrap()).unwrap();
    fs::write(&worker, WORKER_SKILL_MD).unwrap();
    ensure_claude_worker_skill_link(&paths).unwrap();
    fs::create_dir_all(paths.canonical_spawn_skill_file().parent().unwrap()).unwrap();
    fs::write(paths.canonical_spawn_skill_file(), "an older spawn skill").unwrap();

    install_spawn_skill(&paths, Engine::Claude).unwrap();
    install_spawn_skill(&paths, Engine::Claude).unwrap();

    assert_eq!(
        fs::read_to_string(paths.canonical_spawn_skill_file()).unwrap(),
        SPAWN_SKILL_MD
    );
    assert_eq!(
        fs::read_to_string(paths.claude_project_skill_file()).unwrap(),
        WORKER_SKILL_MD,
        "the worker skill and its alias are untouched"
    );
    assert_eq!(
        fs::read_link(paths.claude_project_skill_dir()).unwrap(),
        PathBuf::from(CLAUDE_WORKER_SKILL_LINK_TARGET)
    );
}

#[test]
fn install_reports_a_blocked_skill_directory_file_or_claude_alias() {
    // `.agents` is a regular file: the canonical directory cannot be created.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    fs::write(dir.path().join(".agents"), "keep").unwrap();
    let error = install_spawn_skill(&paths, Engine::Codex).unwrap_err();
    assert!(format!("{error:#}").contains("pmtui-spawn"), "{error:#}");
    assert_eq!(
        fs::read_to_string(dir.path().join(".agents")).unwrap(),
        "keep"
    );

    // The SKILL.md path is a directory: the atomic replace cannot land.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    fs::create_dir_all(paths.canonical_spawn_skill_file()).unwrap();
    let error = install_spawn_skill(&paths, Engine::Codex).unwrap_err();
    assert!(format!("{error:#}").contains("SKILL.md"), "{error:#}");

    // `.claude` is a regular file: the canonical copy lands, the alias is refused.
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    fs::write(paths.claude_root_dir(), "keep").unwrap();
    let error = install_spawn_skill(&paths, Engine::Claude).unwrap_err();
    assert!(format!("{error:#}").contains("Claude"), "{error:#}");
    assert_eq!(
        fs::read_to_string(paths.canonical_spawn_skill_file()).unwrap(),
        SPAWN_SKILL_MD
    );
    assert_eq!(fs::read_to_string(paths.claude_root_dir()).unwrap(), "keep");
}

#[test]
fn launch_installer_ships_the_skill_only_with_a_terminal_that_names_pmtui() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());

    install_spawn_skill_for_launch(dir.path(), Engine::Claude, &launch_env(None)).unwrap();
    assert!(
        !paths.canonical_spawn_skill_file().exists(),
        "without PMTUI_BIN the skill's command could not run, so nothing is installed"
    );
    assert!(!paths.claude_spawn_skill_dir().exists());

    install_spawn_skill_for_launch(dir.path(), Engine::Claude, &launch_env(Some("/opt/pmtui")))
        .unwrap();
    assert_eq!(
        fs::read_to_string(paths.canonical_spawn_skill_file()).unwrap(),
        SPAWN_SKILL_MD
    );
    assert!(is_symlink(&paths.claude_spawn_skill_dir()));
}
