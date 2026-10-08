use std::fs;

use crate::state::ProjectPaths;

use super::{CLAUDE_WORKER_SKILL_LINK_TARGET, WORKER_SKILL_MD, ensure_claude_worker_skill_link};

fn write_current(path: &std::path::Path) {
    fs::create_dir_all(path.parent().expect("skill path has a parent")).unwrap();
    fs::write(path, WORKER_SKILL_MD).unwrap();
}

#[test]
fn claude_uses_the_canonical_skill_through_its_exact_alias() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");

    write_current(&paths.canonical_worker_skill_file());

    ensure_claude_worker_skill_link(&paths).unwrap();
    ensure_claude_worker_skill_link(&paths).unwrap();
    assert_eq!(
        fs::read_link(paths.claude_project_skill_dir()).unwrap(),
        std::path::PathBuf::from(CLAUDE_WORKER_SKILL_LINK_TARGET)
    );
}

#[test]
fn codex_uses_the_canonical_skill_without_a_claude_alias() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "codex-session");

    write_current(&paths.canonical_worker_skill_file());
    assert_eq!(
        fs::read_to_string(paths.canonical_worker_skill_file()).unwrap(),
        WORKER_SKILL_MD
    );
    assert!(!paths.claude_project_skill_dir().exists());
}

#[test]
fn legacy_claude_skill_directory_is_replaced_by_the_canonical_alias() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_project_skill_dir()).unwrap();
    fs::write(paths.claude_project_skill_file(), "legacy or customized").unwrap();
    write_current(&paths.canonical_worker_skill_file());

    ensure_claude_worker_skill_link(&paths).unwrap();
    assert_eq!(
        fs::read_to_string(paths.claude_project_skill_file()).unwrap(),
        WORKER_SKILL_MD
    );
}

#[test]
fn empty_legacy_claude_skill_directory_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_project_skill_dir()).unwrap();

    ensure_claude_worker_skill_link(&paths).unwrap();
    assert!(
        fs::symlink_metadata(paths.claude_project_skill_dir())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn stale_claude_skill_symlink_is_replaced() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_skills_dir()).unwrap();
    symlink("../wrong", paths.claude_project_skill_dir()).unwrap();

    ensure_claude_worker_skill_link(&paths).unwrap();
    assert_eq!(
        fs::read_link(paths.claude_project_skill_dir()).unwrap(),
        std::path::PathBuf::from(CLAUDE_WORKER_SKILL_LINK_TARGET)
    );
}

#[test]
fn whole_skills_alias_is_rejected() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    write_current(&paths.canonical_worker_skill_file());
    fs::create_dir_all(paths.claude_root_dir()).unwrap();
    symlink("../.agents/skills", paths.claude_skills_dir()).unwrap();

    assert!(ensure_claude_worker_skill_link(&paths).is_err());
}

#[test]
fn regular_claude_project_path_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::write(paths.claude_root_dir(), "keep").unwrap();

    assert!(ensure_claude_worker_skill_link(&paths).is_err());
    assert_eq!(fs::read_to_string(paths.claude_root_dir()).unwrap(), "keep");
}

#[test]
fn symlinked_claude_root_is_rejected() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, paths.claude_root_dir()).unwrap();

    assert!(ensure_claude_worker_skill_link(&paths).is_err());
    assert!(fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn noncanonical_whole_skills_alias_is_rejected() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::create_dir_all(paths.claude_root_dir()).unwrap();
    symlink(&outside, paths.claude_skills_dir()).unwrap();

    assert!(ensure_claude_worker_skill_link(&paths).is_err());
    assert!(fs::read_dir(outside).unwrap().next().is_none());
}

#[test]
fn regular_file_at_alias_path_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_skills_dir()).unwrap();
    fs::write(paths.claude_project_skill_dir(), "keep").unwrap();

    ensure_claude_worker_skill_link(&paths).unwrap();
    assert_eq!(
        fs::read_link(paths.claude_project_skill_dir()).unwrap(),
        std::path::PathBuf::from(CLAUDE_WORKER_SKILL_LINK_TARGET)
    );
}

#[test]
fn skills_parent_blocker_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_root_dir()).unwrap();
    fs::write(paths.claude_skills_dir(), "keep").unwrap();

    assert!(ensure_claude_worker_skill_link(&paths).is_err());
    assert_eq!(
        fs::read_to_string(paths.claude_skills_dir()).unwrap(),
        "keep"
    );
}

#[test]
fn inaccessible_project_root_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o000)).unwrap();
    let result = ensure_claude_worker_skill_link(&paths);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
}

#[test]
fn skills_directory_creation_failure_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir(paths.claude_root_dir()).unwrap();
    fs::set_permissions(paths.claude_root_dir(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = ensure_claude_worker_skill_link(&paths);
    fs::set_permissions(paths.claude_root_dir(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
}

#[test]
fn inaccessible_skills_directory_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_root_dir()).unwrap();
    fs::set_permissions(paths.claude_root_dir(), fs::Permissions::from_mode(0o000)).unwrap();
    let result = ensure_claude_worker_skill_link(&paths);
    fs::set_permissions(paths.claude_root_dir(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
}

#[test]
fn inaccessible_alias_path_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_skills_dir()).unwrap();
    fs::set_permissions(paths.claude_skills_dir(), fs::Permissions::from_mode(0o000)).unwrap();
    let result = ensure_claude_worker_skill_link(&paths);
    fs::set_permissions(paths.claude_skills_dir(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
}

#[test]
fn symlink_creation_failure_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "claude-session");
    fs::create_dir_all(paths.claude_skills_dir()).unwrap();
    fs::set_permissions(paths.claude_skills_dir(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = ensure_claude_worker_skill_link(&paths);
    fs::set_permissions(paths.claude_skills_dir(), fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err());
    assert!(!paths.claude_project_skill_dir().exists());
}
