//! Pure key planning and active-probe checks for terminal choice menus.

use crate::tmux::dialog_keys::{probe_matches, probe_plan, selection_keys, single_selection_keys};
use crate::tmux::{PaneDialog, PaneDialogClass, PaneDialogMode, classify_dialog};

fn single(current: Option<usize>, options: usize) -> PaneDialog {
    PaneDialog {
        question: "Which option?".into(),
        options: (0..options).map(|i| format!("option {i}")).collect(),
        selected_index: current,
        mode: PaneDialogMode::Single,
        checked_indices: Vec::new(),
        class: PaneDialogClass::DelegableChoice,
        context_hash: 0,
    }
}

#[test]
fn single_selection_moves_in_either_direction_then_submits() {
    assert_eq!(
        single_selection_keys(1, 3).unwrap(),
        ["Down", "Down", "Enter"]
    );
    assert_eq!(single_selection_keys(3, 1).unwrap(), ["Up", "Up", "Enter"]);
    assert_eq!(single_selection_keys(2, 2).unwrap(), ["Enter"]);
    assert!(single_selection_keys(0, 13).unwrap_err().contains("max 12"));
}

#[test]
fn probe_plan_moves_to_a_neighbor_or_refuses_a_one_row_menu() {
    assert_eq!(probe_plan(0, 3).unwrap(), (1, "Down", "Up"));
    assert_eq!(probe_plan(2, 3).unwrap(), (1, "Up", "Down"));
    assert_eq!(
        probe_plan(0, 1),
        Err("dialog has no second option to probe".into())
    );
}

#[test]
fn probe_match_requires_every_stable_dialog_field_and_the_target_cursor() {
    let expected = single(Some(0), 2);
    let mut moved = expected.clone();
    moved.selected_index = Some(1);
    assert!(probe_matches(&expected, &moved, 1));

    moved.question.push('!');
    assert!(!probe_matches(&expected, &moved, 1));

    moved = expected.clone();
    moved.selected_index = Some(1);
    moved.options[0].push('!');
    assert!(!probe_matches(&expected, &moved, 1));

    moved = expected.clone();
    moved.selected_index = Some(1);
    moved.mode = PaneDialogMode::Multiple;
    assert!(!probe_matches(&expected, &moved, 1));

    moved = expected.clone();
    moved.selected_index = Some(1);
    moved.checked_indices.push(0);
    assert!(!probe_matches(&expected, &moved, 1));

    moved = expected.clone();
    moved.selected_index = Some(1);
    moved.class = PaneDialogClass::HumanOnly;
    assert!(!probe_matches(&expected, &moved, 1));

    moved = expected.clone();
    moved.selected_index = Some(1);
    moved.context_hash = 1;
    assert!(!probe_matches(&expected, &moved, 1));

    moved = expected.clone();
    moved.selected_index = Some(0);
    assert!(!probe_matches(&expected, &moved, 1));
}

#[test]
fn selection_plan_rejects_missing_invalid_and_duplicate_targets() {
    assert_eq!(
        selection_keys(&single(None, 2), &[0]),
        Err("dialog has no selected option".into())
    );
    for targets in [vec![], vec![2]] {
        assert_eq!(
            selection_keys(&single(Some(0), 2), &targets),
            Err("dialog targets are empty or out of range".into())
        );
    }
    assert_eq!(
        selection_keys(&single(Some(0), 2), &[0, 0]),
        Err("dialog targets contain duplicate indices".into())
    );
    assert_eq!(
        selection_keys(&single(Some(0), 2), &[0, 1]),
        Err("single-choice dialog requires exactly one target".into())
    );
    assert_eq!(
        selection_keys(&single(Some(1), 3), &[0]).unwrap(),
        ["Up", "Enter"]
    );
}

#[test]
fn multi_selection_refuses_an_unreasonably_long_key_sequence() {
    let mut dialog = single(Some(0), 70);
    dialog.mode = PaneDialogMode::Multiple;
    let targets: Vec<usize> = (0..70).step_by(2).collect();
    assert_eq!(
        selection_keys(&dialog, &targets),
        Err("dialog key sequence exceeds 64 keys".into())
    );
}

#[test]
fn multi_selection_toggles_the_delta_then_submits() {
    let dialog = classify_dialog(concat!(
        " Which layers?\n",
        " ❯ 1. [✔] unit\n",
        "   2. [ ] integration\n",
        "   3. [ ] Type something\n",
        "      Submit\n",
        " Enter to select · Esc to cancel\n",
    ))
    .unwrap();
    assert_eq!(
        selection_keys(&dialog, &[1]).unwrap(),
        ["Enter", "Enter", "Down", "Enter"]
    );
}

#[test]
fn multi_selection_accounts_for_checkbox_auto_advance_before_submit() {
    let mut dialog = classify_dialog(concat!(
        " Which layers?\n",
        " ❯ 1. [ ] unit\n",
        "   2. [ ] integration\n",
        "   3. [ ] real tmux\n",
        "   4. [ ] Type something\n",
        "      Submit\n",
        " Enter to select · Esc to cancel\n",
    ))
    .unwrap();
    // The active interactivity probe moved the cursor from option 1 to option 2.
    dialog.selected_index = Some(1);
    assert_eq!(
        selection_keys(&dialog, &[2]).unwrap(),
        ["Down", "Enter", "Down", "Enter"]
    );
}
