use crate::*;

pub(crate) fn board_column(view: &ProjectView) -> BoardColumn {
    if !view.enabled {
        BoardColumn::Paused
    } else if view.posture.needs_attention() {
        BoardColumn::NeedsYou
    } else if board_is_confirmed_working(view) {
        BoardColumn::Working
    } else if view.tier == Some(Tier::Autopilot) {
        BoardColumn::Autopilot
    } else {
        BoardColumn::Pending
    }
}

fn board_is_confirmed_working(view: &ProjectView) -> bool {
    status_is_working(view) && view.agent_working == Some(true)
}

/// Whether lane-view `s` acts on this card, and therefore whether the lane publishes its chip.
/// An open stop routes to Answer from any lane; a Needs You card without one routes to Message,
/// which opens in Task detail. Other lanes reach Message only after Enter opens detail.
pub(crate) fn board_lane_message_applies(view: &ProjectView) -> bool {
    !view.stops.is_empty() || board_column(view) == BoardColumn::NeedsYou
}

impl App {
    /// CHANGING VIEW WRITES NO STATUS. `record_status` files the status line into the log at the end
    /// of every key, so a line here is a log entry — and `1`/`2` are the keys a human presses most,
    /// which meant simply looking around spent the log on where you already are (user: *"when we swap
    /// between 1, and 2. i see the status get log, can we remove that?"*). The tabs in the top bar
    /// already say which view you are in. `open_board_session` was written to this rule; the whole
    /// view-navigation family now follows it.
    ///
    /// The status that was already there SURVIVES the switch: it is the outcome of the last thing the
    /// human did, and changing view is not a reason to drop it.
    pub(crate) fn open_board(&mut self) {
        self.mode = UiMode::Board;
        self.board_detail_open = false;
    }

    pub(crate) fn close_board(&mut self) {
        self.board_detail_open = false;
        self.mode = UiMode::Normal;
    }

    pub(crate) fn board_move_vertical(&mut self, delta: isize) {
        let Some(selected) = self.selected_view() else {
            return;
        };
        let column = board_column(selected);
        let members: Vec<usize> = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(index, view)| (board_column(view) == column).then_some(index))
            .collect();
        let current = members
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, members.len().saturating_sub(1) as isize);
        if let Some(index) = members.get(next as usize) {
            self.select_project_index(*index);
        }
    }

    pub(crate) fn board_move_horizontal(&mut self, delta: isize) {
        let Some(selected) = self.selected_view() else {
            return;
        };
        let current_column = board_column(selected);
        let current_members: Vec<usize> = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(index, view)| (board_column(view) == current_column).then_some(index))
            .collect();
        let row = current_members
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let mut column = current_column.index() as isize + delta.signum();
        while (0..BoardColumn::ALL.len() as isize).contains(&column) {
            let target = BoardColumn::ALL[column as usize];
            let members: Vec<usize> = self
                .projects
                .iter()
                .enumerate()
                .filter_map(|(index, view)| (board_column(view) == target).then_some(index))
                .collect();
            if !members.is_empty() {
                self.select_project_index(members[row.min(members.len() - 1)]);
                return;
            }
            column += delta.signum();
        }
    }

    pub(crate) fn open_board_session(&mut self, id: &str) {
        if let Some(index) = self.projects.iter().position(|view| view.id == id) {
            // No status line: keyboard selection writes none, and every status lands in the
            // Status log, so a click must not push the failures that log keeps visible out of view.
            self.select_project_index(index);
            self.board_detail_open = false;
        }
    }

    /// Leaving a card's detail is navigation too (see [`App::open_board`]), so it says nothing: the
    /// card you came from is on screen, and `Task Board` was a log line that told you only that.
    pub(crate) fn close_board_detail(&mut self) {
        self.board_detail_open = false;
    }

    pub(crate) fn finish_board_action(&mut self) {
        if self.return_to_board_after_action {
            self.mode = UiMode::Board;
        }
        self.return_to_board_after_action = false;
    }

    /// One wheel tick over `column`: the first tick over another lane selects that lane's first
    /// card, later ticks move within it, and a lane with no cards changes nothing — the selection
    /// never moves in a lane the pointer is not over. Returns whether the selected id changed.
    pub(crate) fn board_wheel_column(&mut self, column: BoardColumn, down: bool) -> bool {
        let before = self.selected_view().map(|view| view.id.clone());
        let Some(first) = self
            .projects
            .iter()
            .position(|view| board_column(view) == column)
        else {
            return false;
        };
        if self
            .selected_view()
            .is_some_and(|view| board_column(view) == column)
        {
            self.board_move_vertical(if down { 1 } else { -1 });
        } else {
            self.select_project_index(first);
        }
        self.selected_view().map(|view| view.id.as_str()) != before.as_deref()
    }
}
