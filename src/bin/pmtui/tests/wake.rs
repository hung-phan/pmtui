//! Wake-follow state labels, colours, and empty-log rendering.

use super::*;

#[test]
fn every_wake_state_has_the_expected_word_colour_and_emphasis() {
    for (state, word, colour) in [
        (
            job::WakeState::Working,
            "working",
            agent_manager::theme::live(),
        ),
        (
            job::WakeState::Monitoring,
            "monitoring",
            agent_manager::theme::accent(),
        ),
        (
            job::WakeState::Blocked,
            "blocked",
            agent_manager::theme::soft(),
        ),
    ] {
        assert_eq!(wake_state_str(state), word);
        let _ = colour;
    }
}

#[test]
fn empty_wake_log_uses_a_placeholder_and_preserves_scroll_status() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), "bot");
    std::fs::create_dir_all(paths.steps_dir()).unwrap();
    std::fs::write(paths.step_log(1), "").unwrap();
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    let mut max = usize::MAX;

    terminal
        .draw(|frame| {
            max = render_wake_view(frame, "bot", &paths, 3, frame.area());
        })
        .unwrap();

    let screen = screen_text(&terminal);
    assert_eq!(max, 0);
    assert!(
        screen.contains("Wake: bot") && screen.contains("#1"),
        "{screen}"
    );
    assert!(screen.contains('—'), "{screen}");
    assert!(
        screen.contains("scrolled") && screen.contains('3'),
        "{screen}"
    );
}
