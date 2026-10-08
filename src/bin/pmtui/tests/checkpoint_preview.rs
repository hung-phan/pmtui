use super::*;

fn write_checkpoint(paths: &ProjectPaths, body: &str) {
    std::fs::write(paths.checkpoint(), body).unwrap();
}

fn checkpoint() -> state::WorkerCheckpoint {
    state::WorkerCheckpoint {
        version: 1,
        seq: 12,
        done: vec!["implemented recovery".into()],
        in_progress: vec!["running verification".into()],
        decisions: vec!["preserve the terminal".into()],
        blockers: Vec::new(),
        activities: vec![state::CheckpointActivity {
            id: "tests".into(),
            status: state::CheckpointActivityStatus::Running,
            handle: Some("pid:1234@start:1787800000".into()),
            output_ref: Some("/tmp/tests.log".into()),
            started_unix_s: Some(1787800000),
            deadline_unix_s: Some(1787803600),
        }],
        next: vec!["inspect failures".into()],
        important_files: vec!["src/job_engine/session.rs".into()],
    }
}

fn lines_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn preview_shows_a_compact_checkpoint_without_replacing_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let (app, paths) = app_with_wake_fixture(
        dir.path(),
        Some(r#"{ "state": "monitoring", "seq": 8, "status": "tests are running" }"#),
    );
    write_checkpoint(
        &paths,
        r#"{
          "version": 1,
          "seq": 12,
          "done": ["implemented recovery", "added regression tests"],
          "in_progress": ["running real tmux verification"],
          "activities": [{
            "id": "tmux-tests",
            "status": "running",
            "handle": "pid:1234@start:1787800000",
            "output_ref": "/tmp/tmux-tests.log",
            "started_unix_s": 1787800000,
            "deadline_unix_s": 1787803600
          }],
          "next": ["inspect failures"],
          "important_files": ["src/job_engine/session.rs"]
        }"#,
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render preview checkpoint");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("tests are running"),
        "report missing: {screen}"
    );
    assert!(
        screen.contains("checkpoint #12"),
        "checkpoint missing: {screen}"
    );
    assert!(
        screen.contains("done 2"),
        "checkpoint counts missing: {screen}"
    );
    assert!(
        screen.contains("now 1") && screen.contains("activities 1"),
        "current work and detached activity counts must be independent: {screen}"
    );
    assert!(
        screen.contains("running real tmux verification"),
        "current checkpoint work missing: {screen}"
    );
    assert!(
        screen.contains("activities 1"),
        "background activity count missing: {screen}"
    );
    assert!(
        !screen.contains("pid:1234") && !screen.contains("/tmp/tmux-tests.log"),
        "sensitive activity references must stay out of the default preview: {screen}"
    );
}

#[test]
fn malformed_and_oversized_checkpoints_are_cosmetic() {
    let dir = tempfile::tempdir().unwrap();
    let (app, paths) = app_with_wake_fixture(dir.path(), None);
    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| render(f, app)).expect("render");
        screen_text(&terminal)
    };

    write_checkpoint(&paths, "{not json");
    let malformed = draw(&app);
    assert!(!malformed.contains("checkpoint #"), "{malformed}");
    assert!(malformed.contains("All done here"), "{malformed}");

    std::fs::write(paths.checkpoint(), vec![b' '; 64 * 1024 + 1]).unwrap();
    let oversized = draw(&app);
    assert!(!oversized.contains("checkpoint #"), "{oversized}");
    assert!(oversized.contains("All done here"), "{oversized}");
}

#[test]
fn short_preview_preserves_transcript_space_over_checkpoint_detail() {
    let dir = tempfile::tempdir().unwrap();
    let (app, paths) = app_with_wake_fixture(dir.path(), None);
    write_checkpoint(
        &paths,
        r#"{"version":1,"seq":2,"in_progress":["running verification"]}"#,
    );

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render short");
    let screen = screen_text(&terminal);
    assert!(!screen.contains("checkpoint #"), "{screen}");
    assert!(screen.contains("All done here"), "{screen}");
}

#[test]
fn checkpoint_projection_prioritizes_blockers_then_current_work_then_next() {
    let mut value = checkpoint();
    value.blockers = vec!["waiting for a safe local resource".into()];
    let blocked = lines_text(&preview_checkpoint_lines(Some(&value), 100, 30));
    assert!(blocked.contains("note: waiting for a safe local resource"));
    assert!(blocked.contains("activity: tests · running"));
    assert!(!blocked.contains("pid:1234"));
    assert!(!blocked.contains("/tmp/tests.log"));

    value.blockers.clear();
    value.in_progress.clear();
    value.activities.clear();
    let next = lines_text(&preview_checkpoint_lines(Some(&value), 100, 29));
    assert!(next.contains("next: inspect failures"));
    assert!(!next.contains("activity:"));
}

#[test]
fn checkpoint_projection_counts_activity_without_an_in_progress_duplicate() {
    let mut value = checkpoint();
    value.in_progress.clear();

    let text = lines_text(&preview_checkpoint_lines(Some(&value), 100, 30));
    assert!(text.contains("now 0 · activities 1"), "{text}");
    assert!(text.contains("activity: tests · running"), "{text}");
}

#[test]
fn checkpoint_projection_is_empty_without_space_or_state() {
    let value = checkpoint();
    assert!(preview_checkpoint_lines(None, 100, 40).is_empty());
    assert!(preview_checkpoint_lines(Some(&value), 100, 23).is_empty());
    assert!(preview_checkpoint_lines(Some(&value), 0, 40).is_empty());

    let narrow = lines_text(&preview_checkpoint_lines(Some(&value), 4, 40));
    assert!(narrow.contains("che…"), "{narrow}");
}
