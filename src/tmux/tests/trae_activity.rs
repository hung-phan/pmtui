use super::{
    FIXTURE_REAL_DIALOG_PERMISSION, FIXTURE_TRAE_BUSY, FIXTURE_TRAE_DIALOG_HOOK_TRUST,
    FIXTURE_TRAE_DIALOG_SIGNIN,
};
use crate::registry::Engine;
use crate::tmux::{
    PaneActivity, classify_dialog, classify_pane_for_engine, has_composer_for_engine,
    idle_fingerprint_for_engine, progress_fingerprint_for_engine,
};

fn input_box(text: &str) -> String {
    format!("\u{2500}\u{2500}\u{2500}\u{2500}\n\u{276f} {text}\n\u{2500}\u{2500}\u{2500}\u{2500}\n")
}

#[test]
fn trae_captured_busy_composer_accepts_text_without_becoming_idle() {
    assert!(has_composer_for_engine(FIXTURE_TRAE_BUSY, Engine::Trae));
    assert_eq!(
        classify_pane_for_engine(FIXTURE_TRAE_BUSY, Engine::Trae),
        PaneActivity::Busy
    );
    assert!(classify_dialog(FIXTURE_TRAE_BUSY).is_none());
}

#[test]
fn trae_observed_placeholders_in_the_input_box_are_idle() {
    for hint in ["Explain this codebase", "Ask TRAE CLI to do anything"] {
        let pane = format!("Finished response\n{}  ? for shortcuts\n", input_box(hint));
        assert!(has_composer_for_engine(&pane, Engine::Trae), "{hint}");
        assert_eq!(
            classify_pane_for_engine(&pane, Engine::Trae),
            PaneActivity::Idle,
            "{hint}"
        );
    }
}

#[test]
fn trae_transcript_prose_and_incomplete_boxes_do_not_establish_generic_idle() {
    for pane in [
        "\u{276f} Explain this codebase\n".to_string(),
        "The prompt says \u{276f} Explain this codebase\n".to_string(),
        "\u{2500}\u{2500}\u{2500}\u{2500}\n\u{276f} Explain this codebase\n".to_string(),
        "\u{276f} Explain this codebase\n\u{2500}\u{2500}\u{2500}\u{2500}\n".to_string(),
        input_box("1. Explain this codebase"),
    ] {
        assert!(!has_composer_for_engine(&pane, Engine::Trae), "{pane}");
        assert_eq!(
            classify_pane_for_engine(&pane, Engine::Trae),
            PaneActivity::Busy,
            "{pane}"
        );
    }
}

#[test]
fn trae_boxed_non_placeholder_draft_is_busy_not_a_composer() {
    for pane in [
        input_box("The worker is still reading files"),
        input_box("Explain this codebase and then edit it"),
    ] {
        assert!(!has_composer_for_engine(&pane, Engine::Trae), "{pane}");
        assert_eq!(
            classify_pane_for_engine(&pane, Engine::Trae),
            PaneActivity::Busy,
            "{pane}"
        );
    }
}

#[test]
fn trae_dialogs_veto_a_visible_placeholder_composer() {
    let menu =
        "Which path?\n\u{276f} 1. First\n  2. Second\nEnter to select \u{b7} Esc to cancel\n";
    for dialog in [
        FIXTURE_TRAE_DIALOG_SIGNIN,
        FIXTURE_TRAE_DIALOG_HOOK_TRUST,
        FIXTURE_REAL_DIALOG_PERMISSION,
        menu,
    ] {
        let pane = format!("{}{dialog}", input_box("Explain this codebase"));
        assert!(classify_dialog(&pane).is_some(), "{pane}");
        assert!(!has_composer_for_engine(&pane, Engine::Trae), "{pane}");
        assert_eq!(
            classify_pane_for_engine(&pane, Engine::Trae),
            PaneActivity::Busy,
            "{pane}"
        );
    }
}

#[test]
fn trae_placeholder_and_footer_churn_do_not_count_as_progress() {
    let first = format!(
        "Response\n{}Context 94% left\n",
        input_box("Explain this codebase")
    );
    let second = format!(
        "Response\n{}Context 93% left\n",
        input_box("Ask TRAE CLI to do anything")
    );
    assert_eq!(
        idle_fingerprint_for_engine(&first, Engine::Trae),
        idle_fingerprint_for_engine(&second, Engine::Trae)
    );
    assert_eq!(
        progress_fingerprint_for_engine(&first, Engine::Trae),
        progress_fingerprint_for_engine(&second, Engine::Trae)
    );
    let growing = format!("New output\n{second}");
    assert_ne!(
        idle_fingerprint_for_engine(&second, Engine::Trae),
        idle_fingerprint_for_engine(&growing, Engine::Trae)
    );
    assert_ne!(
        progress_fingerprint_for_engine(&second, Engine::Trae),
        progress_fingerprint_for_engine(&growing, Engine::Trae)
    );
}

#[test]
fn trae_busy_markers_win_over_the_placeholder_composer() {
    for marker in [
        "Working (esc to interrupt)",
        "Running (ctrl+c to interrupt)",
        "\u{28fb} Working",
        "\u{273b} Working\u{2026}",
    ] {
        let pane = format!("{marker}\n{}", input_box("Explain this codebase"));
        assert!(has_composer_for_engine(&pane, Engine::Trae));
        assert_eq!(
            classify_pane_for_engine(&pane, Engine::Trae),
            PaneActivity::Busy,
            "{marker}"
        );
    }
}
