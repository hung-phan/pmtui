//! Which conversation the fork branches: the saved id must still be the live one, a Standard Codex
//! session that saved none is read off its own process, and a source that never spoke cannot fork.

use super::*;

const OTHER_ID: &str = "22222222-aaaa-4bbb-8ccc-666666666666";

/// Forget every saved source identity, as for a Standard Codex session pmtui started.
fn forget_source_identity(fixture: &ForkFixture) {
    Registry::update(&fixture.registry, |registry| {
        registry.projects[0].conversation_id = None;
    })
    .unwrap();
    let paths = ProjectPaths::for_session(&fixture.root, "bot");
    let mut ledger = job::load(&paths).unwrap().unwrap();
    ledger.conversation_id = None;
    job::save(&paths, &ledger).unwrap();
}

#[test]
fn standard_codex_fork_branches_the_live_conversation_when_none_was_saved() {
    let mut fixture = fork_fixture(Engine::Codex);
    forget_source_identity(&fixture);
    let driver = live_codex_source(&fixture, Some(SOURCE_ID));
    install_driver(&mut fixture, driver);

    fixture.app.fork_selected();

    let registry = Registry::load(&fixture.registry).unwrap();
    let source = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot")
        .unwrap();
    assert_eq!(
        source.conversation_id, None,
        "the fork reads the live source identity but never writes the source row"
    );
    let child = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot-fork")
        .expect("forked child");
    assert!(child.enabled);
    assert_eq!(child.conversation_id.as_deref(), Some(CHILD_ID));
    let argv = &fixture.pane.launches()[0].2;
    let fork = argv.iter().position(|arg| arg == "fork").unwrap();
    assert_eq!(argv[fork + 1], SOURCE_ID);
    assert_eq!(
        fixture.pane.codex_probes().first(),
        Some(&session_name("bot", &fixture.root)),
        "the live source is identified before the child launches"
    );
    assert!(
        fixture.app.status.contains("ready for Enter or Message"),
        "{}",
        fixture.app.status
    );
}

#[test]
fn codex_fork_refuses_a_saved_id_the_live_conversation_has_left() {
    let mut fixture = fork_fixture(Engine::Codex);
    let driver = live_codex_source(&fixture, Some(OTHER_ID));
    install_driver(&mut fixture, driver);

    fixture.app.fork_selected();

    assert_refused_without_launch(&fixture, "does not match");
    assert!(fixture.app.status.contains(OTHER_ID) && fixture.app.status.contains(SOURCE_ID));
}

#[test]
fn codex_fork_needs_a_provable_source_conversation() {
    let mut failing = fork_fixture(Engine::Codex);
    let source_session = session_name("bot", &failing.root);
    let driver = live_codex_source(&failing, None).with(|inner| {
        inner.fail_codex_probe.insert(source_session);
    });
    install_driver(&mut failing, driver);
    failing.app.fork_selected();
    assert_refused_without_launch(&failing, "could not identify the live Codex conversation");

    let mut unstarted = fork_fixture(Engine::Codex);
    forget_source_identity(&unstarted);
    let driver = live_codex_source(&unstarted, None);
    install_driver(&mut unstarted, driver);
    unstarted.app.fork_selected();
    assert_refused_without_launch(&unstarted, "send its first message first");

    let mut paused = fork_fixture(Engine::Codex);
    forget_source_identity(&paused);
    paused.app.fork_selected();
    assert_refused_without_launch(&paused, "send its first message first");

    let mut unproven = fork_fixture(Engine::Codex);
    let driver = live_codex_source(&unproven, None);
    install_driver(&mut unproven, driver);
    unproven.app.fork_selected();
    assert_forked(&unproven, Engine::Codex);
    let argv = &unproven.pane.launches()[0].2;
    let fork = argv.iter().position(|arg| arg == "fork").unwrap();
    assert_eq!(
        argv[fork + 1],
        SOURCE_ID,
        "an unprovable live probe keeps the saved id"
    );
}

#[test]
fn claude_fork_refuses_a_source_whose_transcript_was_never_written() {
    let mut ghost = fork_fixture(Engine::Claude);
    let claude_home = ghost._dir.path().join("claude-home");
    std::fs::create_dir_all(claude_home.join("projects")).unwrap();
    ghost.app.claude_home = Some(claude_home);

    ghost.app.fork_selected();

    assert_refused_without_launch(&ghost, "send its first message first");

    let mut started = fork_fixture(Engine::Claude);
    let claude_home = started._dir.path().join("claude-home");
    let slug: String = started
        .root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let transcripts = claude_home.join("projects").join(slug);
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SOURCE_ID}.jsonl")), "{}\n").unwrap();
    started.app.claude_home = Some(claude_home);

    started.app.fork_selected();

    assert_forked(&started, Engine::Claude);
}
