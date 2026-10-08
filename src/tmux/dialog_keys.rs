//! Pure key planning and active-probe checks for terminal choice menus.

use std::collections::BTreeSet;

use super::{PaneDialog, PaneDialogMode};

pub(super) fn single_selection_keys(
    current: usize,
    target: usize,
) -> Result<Vec<&'static str>, String> {
    let steps = current.abs_diff(target);
    if steps > 12 {
        return Err(format!(
            "dialog selection requires {steps} key steps (max 12)"
        ));
    }
    let key = if target >= current { "Down" } else { "Up" };
    let mut keys = vec![key; steps];
    keys.push("Enter");
    Ok(keys)
}

pub(super) fn probe_plan(
    current: usize,
    options: usize,
) -> Result<(usize, &'static str, &'static str), String> {
    if current + 1 < options {
        Ok((current + 1, "Down", "Up"))
    } else if current > 0 {
        Ok((current - 1, "Up", "Down"))
    } else {
        Err("dialog has no second option to probe".to_string())
    }
}

pub(super) fn probe_matches(expected: &PaneDialog, moved: &PaneDialog, target: usize) -> bool {
    moved.question == expected.question
        && moved.options == expected.options
        && moved.mode == expected.mode
        && moved.checked_indices == expected.checked_indices
        && moved.class == expected.class
        && moved.context_hash == expected.context_hash
        && moved.selected_index == Some(target)
}

pub(super) fn selection_keys(
    dialog: &PaneDialog,
    targets: &[usize],
) -> Result<Vec<&'static str>, String> {
    let current = dialog
        .selected_index
        .ok_or_else(|| "dialog has no selected option".to_string())?;
    if targets.is_empty()
        || targets.len() > dialog.options.len()
        || targets.iter().any(|index| *index >= dialog.options.len())
    {
        return Err("dialog targets are empty or out of range".to_string());
    }
    let target_set: BTreeSet<usize> = targets.iter().copied().collect();
    if target_set.len() != targets.len() {
        return Err("dialog targets contain duplicate indices".to_string());
    }
    if dialog.mode == PaneDialogMode::Single {
        if targets.len() != 1 {
            return Err("single-choice dialog requires exactly one target".to_string());
        }
        return single_selection_keys(current, targets[0]);
    }

    let checked: BTreeSet<usize> = dialog.checked_indices.iter().copied().collect();
    let toggles: Vec<usize> = checked.symmetric_difference(&target_set).copied().collect();
    let mut at = current;
    let mut keys = Vec::new();
    for target in toggles {
        let key = if target >= at { "Down" } else { "Up" };
        keys.extend(std::iter::repeat_n(key, at.abs_diff(target)));
        keys.push("Enter");
        // Claude advances to the next row after toggling a checkbox. The next row may be
        // another option or the Submit row (represented by options.len()).
        at = (target + 1).min(dialog.options.len());
    }
    keys.extend(std::iter::repeat_n(
        "Down",
        dialog.options.len().saturating_sub(at),
    ));
    keys.push("Enter");
    if keys.len() > 64 {
        return Err("dialog key sequence exceeds 64 keys".to_string());
    }
    Ok(keys)
}
