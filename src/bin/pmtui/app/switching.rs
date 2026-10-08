//! `/` quick switching over the existing session rows. Search state is ephemeral;
//! selection commits by stable session id against the latest refreshed project list.

use crate::*;

pub(crate) fn switch_matches(items: &[SwitchItem], query: &str) -> Vec<usize> {
    let needle = query.trim().to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            needle.is_empty()
                || item.id.to_lowercase().contains(&needle)
                || item.label.to_lowercase().contains(&needle)
                || item.root.to_lowercase().contains(&needle)
        })
        .map(|(index, _)| index)
        .collect()
}

impl App {
    pub(crate) fn begin_switcher(&mut self) {
        if self.projects.is_empty() {
            self.status = "/ switches sessions (there are no sessions)".into();
            return;
        }
        let registry = Registry::load(&self.registry_path).unwrap_or_default();
        let items = self
            .projects
            .iter()
            .map(|view| SwitchItem {
                id: view.id.clone(),
                label: view.label().to_string(),
                root: registry
                    .projects
                    .iter()
                    .find(|entry| entry.id == view.id)
                    .map(|entry| entry.root.display().to_string())
                    .unwrap_or_default(),
            })
            .collect();
        self.mode = UiMode::Switching {
            query: Field::new(),
            cursor: self.selected,
            items,
        };
    }

    pub(crate) fn switch_match_count(&self) -> usize {
        match &self.mode {
            UiMode::Switching { query, items, .. } => switch_matches(items, query.as_str()).len(),
            _ => 0,
        }
    }

    pub(crate) fn move_switch_cursor(&mut self, delta: isize) {
        let count = self.switch_match_count();
        let UiMode::Switching { cursor, .. } = &mut self.mode else {
            return;
        };
        if count == 0 {
            *cursor = 0;
            return;
        }
        *cursor = (*cursor as isize + delta).clamp(0, count as isize - 1) as usize;
    }

    pub(crate) fn choose_switch_result(&mut self, result: usize) {
        let id = match &self.mode {
            UiMode::Switching { query, items, .. } => switch_matches(items, query.as_str())
                .get(result)
                .and_then(|index| items.get(*index))
                .map(|item| item.id.clone()),
            _ => None,
        };
        let Some(id) = id else {
            self.status = "no session matches the search".into();
            return;
        };
        let Some(index) = self.projects.iter().position(|view| view.id == id) else {
            self.mode = if self.return_to_board_after_switch {
                UiMode::Board
            } else {
                UiMode::Normal
            };
            self.return_to_board_after_switch = false;
            self.status = format!("{id} is gone from the list");
            return;
        };
        self.mode = if self.return_to_board_after_switch {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.return_to_board_after_switch = false;
        self.select_project_index(index);
        self.status = format!("selected {id}");
    }

    pub(crate) fn choose_switch_cursor(&mut self) {
        let result = match self.mode {
            UiMode::Switching { cursor, .. } => cursor,
            _ => return,
        };
        self.choose_switch_result(result);
    }
}
