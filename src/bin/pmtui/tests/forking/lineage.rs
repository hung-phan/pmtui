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

#[test]
fn forking_a_child_is_refused_when_its_parent_has_five_children() {
    let mut fixture = fork_fixture(Engine::Claude);
    make_source_a_spawned_child(&fixture, "parent");
    Registry::update(&fixture.registry, |registry| {
        let source = registry.projects[0].clone();
        for sibling in 1..agent_manager::registry::MAX_CHILDREN_PER_PARENT {
            let mut row = source.clone();
            row.id = format!("sibling-{sibling}");
            row.conversation_id = None;
            row.launch = None;
            registry.projects.push(row);
        }
    })
    .unwrap();
    let before = Registry::load(&fixture.registry).unwrap().projects;
    assert_eq!(before.len(), 5);

    fixture.app.fork_selected();

    assert_eq!(
        fixture.app.status,
        "parent parent already has 5 spawned sessions"
    );
    assert!(fixture.pane.launches().is_empty());
    assert_eq!(
        Registry::load(&fixture.registry).unwrap().projects,
        before,
        "nothing is staged"
    );
    assert!(
        !ProjectPaths::for_session(&fixture.root, "bot-fork")
            .state_dir()
            .exists(),
        "no child id is reserved"
    );
}
