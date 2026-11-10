//! A fork of a spawned child stays under its parent: it inherits `spawned_by`, so the depth rule
//! and the per-parent cap apply to it, and only the spawn broker ever records a launch.

use super::*;

/// Mark the fork source as a spawned child of `parent`, launched and finished by its broker.
fn make_source_a_spawned_child(fixture: &ForkFixture, parent: &str) {
    Registry::update(&fixture.registry, |registry| {
        let source = &mut registry.projects[0];
        source.spawned_by = Some(parent.into());
        source.launch = Some(agent_manager::registry::LaunchRecord {
            request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            args_hash: "hash".into(),
            state: agent_manager::registry::LaunchState::Started,
            outcome: Some(agent_manager::registry::SpawnOutcome::Ready),
            kind: agent_manager::registry::LaunchKind::Chat,
            branch: None,
            base_commit: None,
        });
    })
    .unwrap();
}

#[test]
fn a_fork_of_a_spawned_child_copies_spawned_by() {
    let mut fixture = fork_fixture(Engine::Claude);
    make_source_a_spawned_child(&fixture, "parent");

    fixture.app.fork_selected();

    assert_forked(&fixture, Engine::Claude);
    let child = Registry::load(&fixture.registry)
        .unwrap()
        .projects
        .into_iter()
        .find(|entry| entry.id == "bot-fork")
        .unwrap();
    assert_eq!(
        child.spawned_by.as_deref(),
        Some("parent"),
        "a fork of a child stays under the parent's depth rule and cap"
    );
    assert_eq!(child.launch, None, "only the spawn broker records a launch");
}

/// INVERTED when the five-child cap was removed. This used to assert that `f` on a child whose
/// parent already had five children was refused with "parent parent already has 5 spawned
/// sessions". The cap counted ROWS, not running children — a sibling that failed or was cancelled
/// held a place for as long as its row did — so the refusal fired on corpses, and `f` is a human's
/// own keypress. Fan-out is bounded by `spawn::MAX_UNFINISHED_PER_PARENT` at discovery instead.
#[test]
fn forking_a_child_is_allowed_when_its_parent_already_has_five_children() {
    let mut fixture = fork_fixture(Engine::Claude);
    make_source_a_spawned_child(&fixture, "parent");
    Registry::update(&fixture.registry, |registry| {
        let source = registry.projects[0].clone();
        for sibling in 1..5 {
            let mut row = source.clone();
            row.id = format!("sibling-{sibling}");
            row.conversation_id = None;
            row.launch = None;
            registry.projects.push(row);
        }
    })
    .unwrap();
    assert_eq!(Registry::load(&fixture.registry).unwrap().projects.len(), 5);

    fixture.app.fork_selected();

    assert!(
        !fixture.app.status.contains("already has"),
        "no cap refusal survives: {}",
        fixture.app.status
    );
    let after = Registry::load(&fixture.registry).unwrap().projects;
    assert_eq!(after.len(), 6, "the fork was staged: {after:?}");
    assert!(
        after.iter().any(|row| row.id == "bot-fork"),
        "the fork row is there: {after:?}"
    );
}
