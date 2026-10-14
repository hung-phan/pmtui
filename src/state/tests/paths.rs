use super::super::*;

#[test]
fn project_paths_have_expected_suffixes() {
    let paths = ProjectPaths::new("/tmp/proj");
    assert!(paths.brief().ends_with(".project-state/brief.md"));
    assert!(paths.decisions().ends_with(".project-state/decisions.md"));
    assert!(paths.answers().ends_with(".project-state/answers.json"));
    assert!(paths.needs_you().ends_with(".project-state/needs-you.json"));
    assert!(
        paths
            .checkpoint()
            .ends_with(".project-state/checkpoint.json")
    );
    assert!(paths.raw_jsonl().ends_with(".project-state/raw.jsonl"));
    assert!(
        paths
            .raw_jsonl_rotated()
            .ends_with(".project-state/raw.jsonl.1")
    );
    assert!(
        paths
            .decisions_rotated()
            .ends_with(".project-state/decisions.md.1")
    );
}

#[test]
fn per_step_paths() {
    let paths = ProjectPaths::new("/tmp/proj");
    assert!(
        paths
            .done_signal(7)
            .ends_with(".project-state/steps/7.done")
    );
    assert!(paths.step_log(7).ends_with(".project-state/steps/7.log"));
}

#[test]
fn pmstate_path_is_state_json() {
    let path = ProjectPaths::new("/tmp/proj");
    assert!(path.pmstate().ends_with(".project-state/state.json"));
}

#[test]
fn for_session_rebases_state_under_sessions_id() {
    let paths = ProjectPaths::for_session("/tmp/proj", "bot-1");
    assert_eq!(paths.root, std::path::PathBuf::from("/tmp/proj"));

    let segment = paths
        .state_dir()
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap()
        .to_string();
    assert!(segment.starts_with("bot-1-"), "readable prefix: {segment}");
    assert!(
        segment.len() > "bot-1-".len(),
        "hash suffix appended: {segment}"
    );
    assert!(
        paths
            .state_dir()
            .starts_with("/tmp/proj/.project-state/sessions/")
    );
    assert!(paths.config().ends_with("config.json"));
    assert!(paths.driver().ends_with("driver.json"));
    assert!(paths.answers().ends_with("answers.json"));
    assert!(paths.checkpoint().ends_with("checkpoint.json"));
    assert!(paths.daemon_dir().ends_with(".daemon"));
}

#[test]
fn paths_can_be_recovered_from_a_managed_state_dir() {
    let root = ProjectPaths::from_state_dir("/tmp/proj/.project-state").unwrap();
    assert_eq!(
        root.state_dir(),
        std::path::PathBuf::from("/tmp/proj/.project-state")
    );
    assert_eq!(
        root.turn_signal(),
        std::path::PathBuf::from("/tmp/proj/.project-state/.daemon/turn-complete")
    );

    let session = ProjectPaths::for_session("/tmp/proj", "bot-1");
    let recovered = ProjectPaths::from_state_dir(session.state_dir()).unwrap();
    assert_eq!(recovered.state_dir(), session.state_dir());
    assert_eq!(recovered.turn_signal(), session.turn_signal());
    assert!(ProjectPaths::from_state_dir("/tmp/proj/not-state/bot").is_none());
}

#[test]
fn state_dir_recovery_rejects_incomplete_or_unrelated_layouts() {
    // A malformed hook path must not redirect identity writes into an unrelated project.
    for path in [
        "",
        "/",
        ".",
        "..",
        "session-id",
        "sessions/session-id",
        "/sessions/session-id",
        "/tmp/proj/not-state/sessions/session-id",
        "/tmp/proj/.project-state/session-id",
        "/tmp/proj/.project-state/sessions/session-id/.daemon",
    ] {
        assert!(
            ProjectPaths::from_state_dir(path).is_none(),
            "accepted unrelated state directory {path:?}"
        );
    }
}

#[test]
#[cfg(unix)]
fn state_dir_recovery_rejects_non_utf8_layout_names_but_preserves_project_roots() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    let non_utf8 = OsString::from_vec(b"project-\xff".to_vec());
    for path in [
        PathBuf::from("/tmp/proj/.project-state/sessions").join(&non_utf8),
        PathBuf::from("/tmp/proj")
            .join(&non_utf8)
            .join("session-id"),
        PathBuf::from("/tmp/proj")
            .join(&non_utf8)
            .join("sessions/session-id"),
    ] {
        assert!(
            ProjectPaths::from_state_dir(&path).is_none(),
            "accepted an invalid state layout: {path:?}"
        );
    }

    // The project root is an OS path, not a protocol name, so it must survive unchanged.
    let root = PathBuf::from("/tmp").join(non_utf8);
    for original in [
        ProjectPaths::new(&root),
        ProjectPaths::for_session(&root, "worker"),
    ] {
        let recovered = ProjectPaths::from_state_dir(original.state_dir()).unwrap();
        assert_eq!(recovered.root, root);
        assert_eq!(
            recovered.codex_conversation_id(),
            original.codex_conversation_id()
        );
        assert_eq!(recovered.turn_signal(), original.turn_signal());
    }
}

#[test]
fn recovering_runtime_paths_keeps_session_identity_separate_from_state_files() {
    let first = ProjectPaths::for_session("/tmp/proj", "a/b");
    let second = ProjectPaths::for_session("/tmp/proj", "a.b");
    for original in [&first, &second] {
        let recovered = ProjectPaths::from_state_dir(original.state_dir()).unwrap();
        assert_eq!(recovered.pmstate(), original.pmstate());
        assert_eq!(
            recovered.codex_conversation_id(),
            original.codex_conversation_id()
        );
        // A file or hook output is not a state directory, even inside a valid session.
        for file in [
            original.pmstate(),
            original.checkpoint(),
            original.turn_signal(),
            original.codex_conversation_id(),
        ] {
            assert!(ProjectPaths::from_state_dir(file).is_none());
        }
    }
    assert_ne!(
        first.codex_conversation_id(),
        second.codex_conversation_id()
    );
}

#[test]
fn worker_skill_paths_are_correct() {
    let paths = ProjectPaths::for_session("/proj", "s1");
    assert_eq!(
        paths.canonical_worker_skill_file(),
        paths
            .root
            .join(".agents/skills/agent-manager-worker/SKILL.md")
    );
    assert_eq!(
        paths.canonical_skills_dir(),
        paths.root.join(".agents/skills")
    );
    assert_eq!(paths.claude_root_dir(), paths.root.join(".claude"));
    assert_eq!(paths.claude_skills_dir(), paths.root.join(".claude/skills"));
    assert_eq!(
        paths.claude_project_skill_dir(),
        paths.root.join(".claude/skills/agent-manager-worker")
    );
    assert_eq!(
        paths.claude_project_skill_file(),
        paths
            .root
            .join(".claude/skills/agent-manager-worker/SKILL.md")
    );
    assert!(paths.claude_project_skill_file().starts_with(&paths.root));
    assert_eq!(
        paths.codex_project_skill_file(),
        paths
            .root
            .join(".agents/skills/agent-manager-worker/SKILL.md")
    );
    assert!(paths.codex_project_skill_file().starts_with(&paths.root));
    assert_eq!(
        paths.native_worker_skill_file(crate::registry::Engine::Claude),
        paths.canonical_worker_skill_file()
    );
    assert_eq!(
        paths.native_worker_skill_file(crate::registry::Engine::Codex),
        paths.canonical_worker_skill_file()
    );
    assert_eq!(
        paths.native_worker_skill_file(crate::registry::Engine::Trae),
        paths.canonical_worker_skill_file()
    );
}

#[test]
fn spawn_skill_paths_are_siblings_of_the_worker_skill() {
    let paths = ProjectPaths::for_session("/proj", "s1");
    assert_eq!(
        paths.canonical_spawn_skill_file(),
        paths.root.join(".agents/skills/pmtui-spawn/SKILL.md")
    );
    assert_eq!(
        paths.canonical_spawn_skill_file(),
        paths
            .canonical_skills_dir()
            .join(crate::skills::SPAWN_SKILL_NAME)
            .join("SKILL.md")
    );
    assert_eq!(
        paths.claude_spawn_skill_dir(),
        paths.root.join(".claude/skills/pmtui-spawn")
    );
    assert_eq!(
        paths.claude_skill_dir(crate::skills::WORKER_SKILL_NAME),
        paths.claude_project_skill_dir(),
        "the worker alias is the same per-skill alias under a fixed name"
    );
    // The skill is project-wide: every session in one root shares it.
    assert_eq!(
        ProjectPaths::new("/proj").canonical_spawn_skill_file(),
        paths.canonical_spawn_skill_file()
    );
}

#[test]
fn chat_lock_lives_under_daemon_dir() {
    let paths = ProjectPaths::new("/tmp/proj");
    assert_eq!(paths.chat_lock(), paths.daemon_dir().join("chat.json"));
    assert!(
        paths
            .chat_lock()
            .ends_with(".project-state/.daemon/chat.json")
    );

    let session_paths = ProjectPaths::for_session("/tmp/proj", "bot-1");
    assert_eq!(
        session_paths.chat_lock(),
        session_paths.daemon_dir().join("chat.json")
    );
    assert!(
        session_paths
            .chat_lock()
            .starts_with("/tmp/proj/.project-state/sessions/")
    );
    assert!(session_paths.chat_lock().ends_with(".daemon/chat.json"));
}

#[test]
fn new_paths_unchanged_by_for_session_addition() {
    let paths = ProjectPaths::new("/tmp/proj");
    assert!(paths.state_dir().ends_with(".project-state"));
    assert!(paths.config().ends_with(".project-state/config.json"));
}

#[test]
fn distinct_session_ids_get_distinct_subtrees_but_share_root() {
    let first = ProjectPaths::for_session("/tmp/proj", "bot-1");
    let second = ProjectPaths::for_session("/tmp/proj", "bot-2");
    assert_ne!(
        first.state_dir(),
        second.state_dir(),
        "no clobber in one folder"
    );
    assert_eq!(first.root, second.root, "same folder / cwd");
}

#[test]
fn distinct_ids_that_sanitize_alike_still_get_distinct_subtrees() {
    let first = ProjectPaths::for_session("/tmp/proj", "a.b").state_dir();
    let second = ProjectPaths::for_session("/tmp/proj", "a/b").state_dir();
    assert_ne!(first, second, "sanitize-collision must not clobber");
}

#[test]
fn session_seg_sanitizes_unsafe_chars() {
    let segment = |id: &str| -> String {
        ProjectPaths::for_session("/r", id)
            .state_dir()
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .to_string()
    };

    assert!(segment("a/b c").starts_with("a-b-c-"));
    assert!(segment("///").starts_with("session-"));
}
