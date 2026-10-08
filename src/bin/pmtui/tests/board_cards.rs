use super::*;

/// Each screen row's text beside its per-cell `(fg, modifier)` styles.
type StyledRows = Vec<(String, Vec<(Color, Modifier)>)>;

fn card_context(view: ProjectView) -> (String, StyledRows) {
    let app = app_with(vec![view], UiMode::Board);
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    (screen_text(&terminal), screen_rows_styled(&terminal))
}

/// The card said "terminal offline" only for Standard rows, so an Autopilot card whose terminal
/// had exited (pmd down, or before its next tick) read BETWEEN TURNS with nothing saying no agent
/// was running. Terminal availability is card-level truth for every enabled session.
#[test]
fn every_enabled_card_without_a_live_terminal_says_so() {
    let mut autopilot = autopilot_loop_view("auto");
    autopilot.posture = Posture::Monitoring;
    autopilot.agent_working = Some(false);
    let mut standard = agent_loop_view("standard");
    standard.posture = Posture::Fresh;
    let mut needs = view("needs", Posture::NeedsYou, vec![]);
    needs.tier = Some(Tier::Standard);

    for view in [autopilot, standard, needs] {
        let id = view.id.clone();
        let (screen, rows) = card_context(view);
        assert!(screen.contains("terminal offline"), "{id}: {screen}");
        let styles = rows
            .iter()
            .find(|(text, _)| text.contains("terminal offline"))
            .map(|(text, styles)| {
                let start = text[..text.find("terminal offline").unwrap()]
                    .chars()
                    .count();
                styles[start..start + "terminal offline".len()].to_vec()
            })
            .unwrap();
        assert!(
            styles
                .iter()
                .all(|(fg, _)| *fg == agent_manager::theme::hard()),
            "{id}: offline is an availability warning, not dim metadata: {styles:?}"
        );
    }
}

#[test]
fn a_fork_card_keeps_its_lineage_beside_the_offline_cue() {
    let mut fork = autopilot_loop_view("child");
    fork.posture = Posture::Monitoring;
    fork.agent_working = Some(false);
    fork.forked_from = Some("parent".into());

    let (screen, _) = card_context(fork.clone());
    assert!(
        screen.contains("terminal offline · fork of parent"),
        "{screen}"
    );

    fork.session_live = true;
    let (screen, _) = card_context(fork);
    assert!(!screen.contains("terminal offline"), "{screen}");
    assert!(screen.contains("fork of parent · shared dir"), "{screen}");
}

#[test]
fn live_and_paused_cards_carry_no_offline_cue() {
    let mut live = autopilot_loop_view("live");
    live.posture = Posture::Monitoring;
    live.session_live = true;
    live.agent_working = Some(false);
    let mut paused = agent_loop_view("paused");
    paused.enabled = false;

    for view in [live, paused] {
        let id = view.id.clone();
        let (screen, _) = card_context(view);
        assert!(!screen.contains("offline"), "{id}: {screen}");
    }
}

/// The context line alone left the headline cue claiming a turn cycle (BETWEEN TURNS) or a ready
/// session (READY) with no terminal under it, while the Session list calls the same row offline.
#[test]
fn an_offline_card_leads_with_offline_rather_than_a_turn_or_ready_cue() {
    let now = SystemClock.now();
    let mut autopilot = autopilot_loop_view("auto");
    autopilot.posture = Posture::Monitoring;
    autopilot.agent_working = Some(false);
    let mut standard = agent_loop_view("standard");
    standard.posture = Posture::Fresh;

    for (view, live_cue) in [(autopilot, "BETWEEN TURNS"), (standard, "READY")] {
        let id = view.id.clone();
        assert_eq!(board_card_cue(&view, now).0, "OFFLINE", "{id}");
        let (screen, _) = card_context(view.clone());
        assert!(screen.contains("OFFLINE"), "{id}: {screen}");
        assert!(!screen.contains(live_cue), "{id}: {screen}");

        let mut live = view;
        live.session_live = true;
        assert_eq!(board_card_cue(&live, now).0, live_cue, "{id}");
    }

    // Human-owned work outranks availability, exactly as in the Session list.
    let mut needs = view("needs", Posture::NeedsYou, vec![]);
    needs.tier = Some(Tier::Standard);
    assert_eq!(board_card_cue(&needs, now).0, "ACTION REQUIRED");
}

/// The `(fg, modifier)` cells under `needle` on the first rendered row that contains it.
fn styles_of(rows: &StyledRows, needle: &str) -> Vec<(Color, Modifier)> {
    rows.iter()
        .find_map(|(text, styles)| {
            let byte = text.find(needle)?;
            let start = text[..byte].chars().count();
            Some(styles[start..start + needle.chars().count()].to_vec())
        })
        .unwrap_or_else(|| panic!("{needle:?} never rendered"))
}

#[test]
fn a_spawned_card_shows_from_parent_label_in_its_context_line() {
    let mut child = agent_loop_view("child");
    child.posture = Posture::Fresh;
    child.spawned_by = Some("parent-id".into());
    child.spawned_by_label = Some("Release lead".into());

    let (screen, rows) = card_context(child.clone());
    assert!(
        screen.contains("terminal offline · from Release lead"),
        "{screen}"
    );
    assert!(
        !screen.contains("parent-id"),
        "the label, not the raw id: {screen}"
    );

    child.session_live = true;
    let (screen, live_rows) = card_context(child);
    assert!(!screen.contains("terminal offline"), "{screen}");
    assert!(screen.contains("from Release lead"), "{screen}");

    // The same slot and style as fork lineage: dim metadata, never a warning hue or a fill.
    let mut fork = agent_loop_view("fork");
    fork.posture = Posture::Fresh;
    fork.session_live = true;
    fork.forked_from = Some("source".into());
    let (_, fork_rows) = card_context(fork);
    let fork_style = styles_of(&fork_rows, "fork of source")[0];
    for (rows, label) in [(&rows, "offline"), (&live_rows, "live")] {
        let styles = styles_of(rows, "from Release lead");
        assert!(
            styles.iter().all(|style| *style == fork_style),
            "{label}: spawn lineage must wear the fork lineage style: {styles:?} vs {fork_style:?}"
        );
    }
}

#[test]
fn fork_lineage_wins_over_spawned_by_on_the_card() {
    // A fork of a spawned child copies its source's `spawned_by`, so it carries both links.
    let mut fork = agent_loop_view("child-fork");
    fork.posture = Posture::Fresh;
    fork.session_live = true;
    fork.forked_from = Some("child".into());
    fork.spawned_by = Some("parent".into());
    fork.spawned_by_label = Some("Release lead".into());

    let (screen, _) = card_context(fork);
    assert!(screen.contains("fork of child · shared dir"), "{screen}");
    assert!(!screen.contains("from Release lead"), "{screen}");
}

/// A registry with a live-less `parent` row (labelled when `parent_label` is set) and a `child`
/// row that names `spawned_by`, loaded the way the dashboard loads it.
fn spawn_lineage_app(dir: &Path, spawned_by: &str, parent_label: Option<&str>) -> App {
    let (registry, root) = reg_with_agent_loop(dir, "parent");
    Registry::update(&registry, |registry| {
        registry.projects[0].display_name = parent_label.map(str::to_string);
        let mut child = registry.projects[0].clone();
        child.id = "child".into();
        child.display_name = None;
        child.spawned_by = Some(spawned_by.into());
        registry.projects.push(child);
    })
    .unwrap();
    seed_agent_loop(
        &ProjectPaths::for_session(&root, "child"),
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "goal",
        Some(300),
        1000,
    )
    .unwrap();
    let mut app = app_with(Vec::new(), UiMode::Board);
    app.registry_path = registry;
    app.refresh();
    let child = app
        .projects
        .iter()
        .position(|view| view.id == "child")
        .expect("the child row loads");
    app.select_project_index(child);
    app
}

fn child_view(app: &App) -> &ProjectView {
    app.projects
        .iter()
        .find(|view| view.id == "child")
        .expect("the child row loads")
}

#[test]
fn a_deleted_parent_shows_the_raw_id() {
    let dir = tempfile::tempdir().unwrap();
    let labelled = spawn_lineage_app(dir.path(), "parent", Some("Release lead"));
    let child = child_view(&labelled);
    assert_eq!(child.spawned_by.as_deref(), Some("parent"));
    assert_eq!(child.spawned_by_label.as_deref(), Some("Release lead"));
    assert!(!child.spawn_staged && !child.incomplete_fork);
    let parent = labelled
        .projects
        .iter()
        .find(|view| view.id == "parent")
        .unwrap();
    assert_eq!(
        (
            parent.spawned_by.as_deref(),
            parent.spawned_by_label.as_deref()
        ),
        (None, None),
        "a root row has no lineage"
    );

    let dir = tempfile::tempdir().unwrap();
    let unlabelled = spawn_lineage_app(dir.path(), "parent", None);
    assert_eq!(
        child_view(&unlabelled).spawned_by_label.as_deref(),
        Some("parent"),
        "a parent without a display name is named by its id"
    );

    let dir = tempfile::tempdir().unwrap();
    let orphan = spawn_lineage_app(dir.path(), "gone-parent", Some("Release lead"));
    let child = child_view(&orphan);
    assert_eq!(child.spawned_by.as_deref(), Some("gone-parent"));
    assert_eq!(child.spawned_by_label.as_deref(), Some("gone-parent"));
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal.draw(|frame| render(frame, &orphan)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("from gone-parent"), "{screen}");
    assert!(!screen.contains("from Release lead"), "{screen}");
}
