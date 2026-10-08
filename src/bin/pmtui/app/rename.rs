use crate::*;

impl App {
    pub(crate) fn begin_rename(&mut self) {
        let Some(view) = self.selected_view() else {
            self.status = "R renames a session (nothing is selected)".into();
            return;
        };
        let id = view.id.clone();
        let current = view.display_name.clone();
        let input = Field::from(current.clone().unwrap_or_default());
        self.mode = UiMode::Renaming { id, current, input };
    }

    pub(crate) fn submit_rename(&mut self) {
        let (id, input) = match &self.mode {
            UiMode::Renaming { id, input, .. } => (id.clone(), input.as_str().to_string()),
            _ => return,
        };
        let name = match agent_manager::registry::normalize_display_name(&input) {
            Ok(name) => name,
            Err(error) => {
                self.status = error.into();
                return;
            }
        };
        let mut found = false;
        let stored = name.clone();
        if let Err(error) = Registry::update(&self.registry_path, |registry| {
            if let Some(entry) = registry.projects.iter_mut().find(|entry| entry.id == id) {
                entry.display_name = stored;
                found = true;
            }
        }) {
            self.status = format!("could not rename {id}: {error}");
            return;
        }
        if !found {
            self.status = format!("{id} is gone from the session list");
            self.finish_rename();
            self.refresh();
            return;
        }
        self.finish_rename();
        self.refresh();
        self.status = match name {
            Some(name) => format!("renamed {id} to {name}"),
            None => format!("cleared the name for {id}"),
        };
    }

    pub(crate) fn cancel_rename(&mut self) {
        self.finish_rename();
        self.status = "name unchanged".into();
    }

    fn finish_rename(&mut self) {
        self.mode = if self.return_to_board_after_action {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.return_to_board_after_action = false;
    }
}
