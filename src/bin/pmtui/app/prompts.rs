//! The overlays that ask the human for one thing and write it: Answer (`s` on an open
//! stop) and `g` (the session's goal). Both open and both refuse when nothing would act
//! on what was typed. Marker stops keep the field open when text is required; unsafe
//! pane dialogs close it and direct the human to attach.

use crate::*;
use agent_manager::pmstate::{OpenStop, StopStatus};

enum AnswerResolution {
    Answer(String),
    NeedText,
    RequireAttach,
}

fn load_agent_loop_stop(entry: &ProjectEntry, stop_id: &str) -> Option<OpenStop> {
    job::load(&entry_state_paths(entry))
        .ok()
        .flatten()?
        .open_stops
        .into_iter()
        .find(|stop| stop.id == stop_id)
}

fn pane_dialog_requires_attach(stop: &OpenStop) -> bool {
    stop.is_pane_dialog()
        && (stop.status == StopStatus::Held || !stop.dashboard_answerable_dialog())
}

fn resolve_answer(
    input: String,
    choice: usize,
    options: &[String],
    pane_dialog: bool,
) -> AnswerResolution {
    if pane_dialog {
        if !input.is_empty() {
            return AnswerResolution::RequireAttach;
        }
        return match options.get(choice) {
            Some(option) if agent_manager::tmux::dialog_option_is_concrete(option) => {
                AnswerResolution::Answer(option.clone())
            }
            _ => AnswerResolution::RequireAttach,
        };
    }
    if !input.is_empty() {
        AnswerResolution::Answer(input)
    } else if let Some(option) = options.get(choice.min(options.len().saturating_sub(1))) {
        AnswerResolution::Answer(option.clone())
    } else {
        AnswerResolution::NeedText
    }
}

fn attach_for_dialog_status(id: &str) -> String {
    format!("{id}: press Enter to attach and finish this dialog")
}

impl App {
    /// `s` on an open stop: open the answer overlay for the selected row's open stop.
    ///
    /// REFUSES when nothing would deliver the answer. With autopilot OFF, pmd does not
    /// drive an agent-loop row at all (`daemon::pmd_drives_row`), so nobody ever runs
    /// `JobScheduler::on_blocked` — the one place an answer is picked up and typed into
    /// the session. Accepting the answer anyway would write it to `answers.json`, report
    /// "answered stop-…", and leave the agent waiting: the exact "it says it worked and
    /// nothing happens" failure this milestone exists to remove. So say so instead, and
    /// name the way that DOES work (Enter attaches the live session; the human answers
    /// the agent directly, which is what "you drive it" means).
    ///
    /// A GUARD, no longer a path `s` takes. `message_route` sends an undriven row's open stop to the
    /// ordinary composer now, so pressing `s` never lands in that refusal — asking a human to change
    /// a session's autonomy before a key would work was the dead end that change removed. The check
    /// stays because it is what makes writing `answers.json` here safe rather than incidentally safe.
    ///
    /// The deliverability check comes first so an undriven row that visibly needs the
    /// human explains how to make progress instead of contradicting itself with "no question".
    ///
    /// Only `begin_answer` sets `UiMode::Answering`, and the selection cannot move while
    /// that overlay is up (every `Char` there goes into the input). Submission still
    /// re-reads the ledger because the daemon can move a pane dialog to `Held` while the
    /// overlay is open.
    pub(crate) fn begin_answer(&mut self) {
        self.answering_stop_id = None;
        let selected = self.selected_view().map(|view| {
            (
                view.id.clone(),
                view.mode,
                view.tier,
                view.stops.first().cloned(),
            )
        });
        match selected {
            // DELIVERABILITY IS TESTED BEFORE EMPTINESS, and the order is the fix.
            //
            // `view::read_agent_loop` now reads `needs-you.json` on an autopilot-OFF row
            // and reports `NeedsYou` while deliberately leaving `stops` EMPTY (pmd skips
            // the whole tick for such a row, so nothing is answerable — only the glyph and
            // the tally become honest). With the emptiness arm first, that row answered `a`
            // with "no question to answer" while the row itself displayed "needs you" — a
            // flat contradiction, and the least useful thing to say to somebody looking at
            // an agent that is visibly waiting on them.
            Some((id, mode, tier, _)) if !answer_reaches_the_agent(mode, tier) => {
                // SHORT and front-loaded: `keybar_line` reserves at most a third of the
                // bar and truncates the TAIL, so the verdict has to come first.
                // Points at `m`, not Enter. `a` writes `answers.json`, which only pmd reads, so
                // turning the dial back on is what makes an answer deliverable AND what clears the
                // stop (only a nudge clears `open_stops`). Enter does reach the agent — you can
                // settle it in conversation — but the row keeps reading "needs you" afterwards,
                // which is the wrong place to send somebody looking at a blocked session.
                //
                // Only reachable when the HUMAN turned autopilot off with a stop open: since m45 the
                // harness never flips the dial itself (that was the m36 stand-down, and it made this
                // refusal fire on a question the harness had just raised).
                self.status = format!("{id}: autopilot off — press m, then s");
            }
            Some((_, _, _, None)) => self.status = "no question to answer".to_string(),
            Some((id, mode, _, Some(display_stop))) => {
                let reg = Registry::load(&self.registry_path).unwrap_or_default();
                let Some(entry) = reg.projects.iter().find(|entry| entry.id == id) else {
                    self.status = format!("{id} is gone from the list — nothing was answered");
                    self.refresh();
                    return;
                };
                if mode == Mode::AgentLoop {
                    let Some(stop) = load_agent_loop_stop(entry, &display_stop.id) else {
                        self.status = attach_for_dialog_status(&id);
                        return;
                    };
                    if pane_dialog_requires_attach(&stop) {
                        self.status = attach_for_dialog_status(&id);
                        return;
                    }
                }
                self.answering_stop_id = Some(display_stop.id.clone());
                self.answer_scroll_top.set(0);
                self.mode = UiMode::Answering {
                    input: Field::new(),
                    choice: 0,
                    scroll: 0,
                };
            }
            // Nothing selected — the same formerly-silent dead end as `Enter`/`d`/`m`.
            None => self.status = "s answers a session's question (nothing is selected)".into(),
        }
    }

    pub(crate) fn submit_answer(&mut self) {
        let (input, choice) = match &self.mode {
            UiMode::Answering { input, choice, .. } => (input.trim().to_string(), *choice),
            _ => return,
        };
        let expected_stop_id = self.answering_stop_id.take();
        let return_to_board = self.return_to_board_after_answer;
        self.mode = if return_to_board {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.return_to_board_after_answer = false;
        // Both dead ends are reachable because the overlay outlives one refresh tick:
        // the stop can be answered/cleared, or the whole row deleted, while it is open.
        // Neither wrote anything, so say so — a typed answer that silently evaporates
        // is the same "it said nothing and nothing happened" failure as a dead key.
        let Some((id, mode, stop)) = self.selected_view().and_then(|view| {
            let stop = match expected_stop_id.as_deref() {
                Some(expected) => view.stops.iter().find(|stop| stop.id == expected),
                None => view.stops.first(),
            }?;
            Some((view.id.clone(), view.mode, stop.clone()))
        }) else {
            self.status = "that question is already answered — nothing was sent".into();
            return;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list — nothing was answered");
            self.refresh();
            return;
        };
        let ledger_stop = if mode == Mode::AgentLoop {
            let Some(ledger_stop) = load_agent_loop_stop(entry, &stop.id) else {
                self.status = attach_for_dialog_status(&id);
                self.refresh();
                return;
            };
            if pane_dialog_requires_attach(&ledger_stop) {
                self.status = attach_for_dialog_status(&id);
                self.refresh();
                return;
            }
            Some(ledger_stop)
        } else {
            None
        };
        let (stop_id, options, pane_dialog) = if let Some(ledger_stop) = ledger_stop.as_ref() {
            (
                ledger_stop.id.clone(),
                ledger_stop.options.as_slice(),
                ledger_stop.is_pane_dialog(),
            )
        } else {
            (stop.id.clone(), stop.options.as_slice(), false)
        };
        // WHAT gets sent, in the order the human's intent is expressed:
        //   1. a pane dialog accepts only its highlighted concrete option, verbatim;
        //   2. otherwise typed marker-stop text wins ("none of these, do X instead");
        //   3. otherwise send the highlighted marker-stop option, verbatim;
        //   4. with no marker-stop option and nothing typed, REFUSE.
        //
        // (3) replaces a literal `"ok"`. That default was a real footgun on the exact screen
        // the user was looking at: `a` then a reflex Enter on a `hard confirm_done` sent "ok"
        // — approving a session's completion with a keystroke that looks like "open the
        // field". A decision this size must be something the human actually said.
        let answer = match resolve_answer(input, choice, options, pane_dialog) {
            AnswerResolution::Answer(answer) => answer,
            AnswerResolution::NeedText => {
                self.status = format!("type an answer for {stop_id} (esc cancels)");
                // Reopen on the SAME stop rather than dropping to the dashboard: the human meant
                // to answer, and making them press `s` again to be told the same thing is a dead
                // end wearing a status line.
                self.mode = UiMode::Answering {
                    input: Field::new(),
                    choice,
                    scroll: 0,
                };
                self.return_to_board_after_answer = return_to_board;
                self.answering_stop_id = Some(stop_id);
                return;
            }
            AnswerResolution::RequireAttach => {
                self.status = attach_for_dialog_status(&id);
                self.refresh();
                return;
            }
        };
        let ans = Answer {
            stop_id: stop_id.clone(),
            answer,
            note: None,
            answered_by: "user".to_string(),
            answered_at: SystemClock.now(),
        };
        // Append to the per-session answers.json so `JobScheduler::on_blocked` observes
        // the answer at/after the park's `since` and resumes the loop.
        match state::append_answer(&entry_state_paths(entry), &ans) {
            Ok(()) => self.status = format!("answered {stop_id}"),
            Err(e) => self.status = format!("answer failed: {e}"),
        }
        self.refresh();
    }

    /// Ctrl+E on the create form's Message/Goal field: queue an edit in the
    /// external editor (drained by `run()`), seeded with the current text so the
    /// user can extend a partial one-liner rather than start over. A no-op off the
    /// Goal field, so the key can only fire where inline typing already lives —
    /// which is what makes writing a real multi-paragraph brief easy.
    pub(crate) fn request_brief_edit(&mut self) {
        if let UiMode::Creating(form) = &self.mode
            && form.field == CreateForm::GOAL
        {
            self.pending_brief_edit = Some(BriefEdit {
                goal: form.goal.as_str().to_string(),
                target: BriefEditTarget::CreateForm,
            });
        }
    }

    /// Resolve the selected row to `(id, brief path, goal on disk)` — the ONE place
    /// either goal key decides WHICH file it edits, so `g` ($EDITOR) and `G` (inline)
    /// can never target different paths for the same row.
    ///
    /// Returns `None` after leaving an explanatory status (never a bare return) on the
    /// dead ends — nothing selected, or a selected row with no registry entry and
    /// therefore no brief path to write — matching `cycle_tier`'s style. `what`
    /// describes the pressed key's job, so each key's refusal names itself.
    pub(crate) fn selected_brief(&mut self, what: &str) -> Option<(String, PathBuf, String)> {
        let Some(id) = self.selected_view().map(|v| v.id.clone()) else {
            self.status = format!("{what} (nothing is selected)");
            return None;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list — no brief to edit");
            self.refresh();
            return None;
        };
        // Per-session brief for an AgentLoop session (root brief for others) — the
        // same `entry_state_paths` subtree the loop itself reads each wake.
        let brief = entry_state_paths(entry).brief();
        // A missing/unreadable brief is not a refusal: the field/editor simply opens
        // empty and the save creates it (the create path seeds one, but a hand-made
        // session may not have it, and an empty goal is exactly what this fixes).
        let goal = std::fs::read_to_string(&brief).unwrap_or_default();
        Some((id, brief, goal))
    }

    /// `g` on a Normal-mode row: open the INLINE goal field for that session.
    ///
    /// The field is SINGLE-LINE, like the create form's Goal field, which is what makes it
    /// a quick tweak rather than a second brief composer.
    ///
    /// A multi-line brief therefore opens the field EMPTY rather than seeded: pushing
    /// and popping characters at the end of a five-line mandate is not editing it, and
    /// since an empty save keeps the previous goal, opening empty cannot lose it. The
    /// overlay says how many lines are on disk and points at `Ctrl+E`, which hands the
    /// whole thing to `$EDITOR` — the only surface that can really edit it.
    ///
    /// There was a `request_goal_edit` here — the straight-to-`$EDITOR` half of a shifted
    /// `g`/`G` pair. It is gone, not merely unbound: the ONE goal key opens the field, and
    /// `^E` inside it reaches the editor through [`App::escalate_goal_edit`], so a second
    /// entry point could only drift from it.
    pub(crate) fn begin_goal_edit(&mut self) {
        // AUTOPILOT-ONLY, matching the chip (`Applies::Autopilot`) and the create form, whose
        // shared intent row is Goal only on Autopilot (Standard uses it for the launch Message).
        // The goal is `brief.md`, and the only thing that reads it is
        // the harness-owned nudge prompt pmd injects each heartbeat — which never runs on a row pmd
        // does not drive. On Standard the human at the keyboard IS the steering, so this field would
        // edit a file nothing consults. Refuses and names the key that makes it matter, exactly as
        // `c` does, because a hidden chip over a working key is the same lie in reverse.
        //
        // `m`'s own goal prompt is a SEPARATE entry point ([`App::begin_autopilot_goal`]), so
        // turning autopilot on can still ask for a goal while the row is still Standard.
        let undriven = self
            .selected_view()
            .filter(|v| !agent_manager::daemon::pmd_drives_row(v.mode, v.tier))
            .map(|v| v.id.clone());
        if let Some(id) = undriven {
            self.status = format!("{id}: the goal steers autopilot — press m to turn it on");
            return;
        }
        let Some((id, brief, current)) = self.selected_brief("g edits a session's goal") else {
            return;
        };
        let input = Composer::seeded(current.trim().to_string(), composer::GOAL);
        self.mode = UiMode::EditingGoal {
            id,
            brief,
            current,
            input,
            then_autopilot: false,
        };
    }

    /// Enter in the inline goal field: apply the typed goal to `brief.md` and close.
    ///
    /// Routes through [`apply_goal_edit`] — the SAME (and only) `brief.md` write path
    /// the editor half uses, so the atomic write that keeps a concurrent nudge from
    /// reading a half-written mandate is not duplicated or forgotten here.
    ///
    /// An empty/whitespace-only save KEEPS the previous goal. That is the editor path's
    /// rule too (`edit_brief` returns `Ok(None)` for an empty buffer, and its seed
    /// comment promises it), and diverging would make `G` the one key that can blank a
    /// session's mandate by accident.
    pub(crate) fn submit_goal_edit(&mut self) {
        let UiMode::EditingGoal {
            id,
            brief,
            input,
            current,
            then_autopilot,
        } = &self.mode
        else {
            return;
        };
        let (id, brief, typed, then_autopilot) = (
            id.clone(),
            brief.clone(),
            input.text().trim().to_string(),
            *then_autopilot,
        );
        let had_goal = !current.trim().is_empty();
        // AUTOPILOT WITH NO DIRECTION IS REFUSED, and the field STAYS OPEN — the one case where
        // this does not close. Closing on a refusal would drop the human back to a dashboard that
        // looks unchanged, having silently swallowed the only thing they were asked for.
        if then_autopilot && typed.is_empty() && !had_goal {
            self.status = format!(
                "{id}: autopilot needs a goal to steer by — type one, or esc to stay on Standard"
            );
            return;
        }
        self.mode = if self.return_to_board_after_action {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        // THE GOAL FIRST, then the flip, so pmd can never take a sweep between the two and nudge
        // with the mandate the human was in the middle of replacing. An empty save KEEPS what is
        // on disk (a multi-line brief opens the field empty on purpose), so it is not a failure —
        // on the autopilot path it means "the old goal still stands".
        // EXACTLY ONE write attempt, whatever the status ends up saying: `apply_goal_edit` is not
        // idempotent bookkeeping, it is the file write.
        let mut write_failed = None;
        self.status = if typed.is_empty() {
            format!("{id} goal kept (empty save changes nothing)")
        } else {
            let outcome = apply_goal_edit(&brief, &typed);
            if let Err(e) = &outcome {
                write_failed = Some(e.to_string());
            }
            self.goal_edit_status(&id, outcome)
        };
        if then_autopilot {
            self.status = match write_failed {
                // A FAILED WRITE MUST NOT FLIP. Turning autopilot on with the new goal unwritten
                // is precisely the stale-mandate failure this prompt exists to prevent: pmd would
                // start nudging the OLD direction, under a status that said the goal was saved.
                Some(e) => format!("{id}: goal write failed ({e}) — autopilot NOT turned on"),
                // The flip leads: it is what the human pressed `m` for. The goal half is named
                // exactly (`saved` vs `kept`) rather than assumed.
                None => {
                    let goal_half = if typed.is_empty() {
                        "goal kept"
                    } else {
                        "goal saved"
                    };
                    // NO CADENCE RECORDED ⇒ ASK FOR IT, and let THAT submit do the flip. User:
                    // *"we need to prompt user for cadence if the value is not set"*. A Standard
                    // session has none (the form does not ask), so turning the heartbeat on is
                    // exactly when the number starts to matter. The goal is already written at this
                    // point, which is what makes the hand-off safe: cancelling the cadence prompt
                    // loses the FLIP, never the goal.
                    if let Some(root) = self
                        .session_root(&id)
                        .filter(|_| self.recorded_cadence(&id).is_none())
                    {
                        self.begin_autopilot_cadence(&id, &root);
                        self.status = format!("{id}: {goal_half} — now set how often it checks in");
                        return;
                    }
                    format!("{}; {goal_half}", self.turn_autopilot_on(&id))
                }
            };
        }
        self.refresh();
        self.finish_board_action();
    }

    /// `^X^E` in the inline goal field: hand off to the SAME external-editor path the brief-edit
    /// drain runs, seeded with the buffer.
    ///
    /// Inline for the edit itself, `$EDITOR` for anything bigger. The overlay closes first, because
    /// `run()` restores the terminal into whatever mode is current when the editor returns.
    pub(crate) fn escalate_goal_edit(&mut self) {
        let UiMode::EditingGoal {
            id,
            brief,
            input,
            then_autopilot,
            ..
        } = &self.mode
        else {
            return;
        };
        // The BUFFER is the seed, whatever is in it: it opened on what is on disk, so handing the
        // editor anything else would discard edits made on the way to pressing the chord. (It used to
        // fall back to `current`, because a multi-line brief opened the field empty and escalating an
        // empty buffer would have replaced the mandate with a blank one. There is no empty-by-default
        // state left to compensate for.)
        let goal = input.text();
        let (id, brief, then_autopilot) = (id.clone(), brief.clone(), *then_autopilot);
        self.mode = if self.return_to_board_after_action {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.pending_brief_edit = Some(BriefEdit {
            goal,
            target: BriefEditTarget::Session {
                id,
                brief,
                then_autopilot,
            },
        });
    }

    /// The status line for an applied goal edit, shared by BOTH halves (`g` via
    /// `run()`'s brief-edit drain, `G` via [`App::submit_goal_edit`]) so the two cannot
    /// report the same file write differently.
    ///
    /// SHORT and front-loaded, because `keybar_line` reserves at most a third of the bar
    /// and truncates the TAIL. The "next nudge" half matters: nothing happens at the
    /// moment of the write — `JobScheduler::nudge` re-reads the brief on its heartbeat —
    /// and a parked session stays parked, which is why the blocked suffix is here at all
    /// (a goal edit is not an answer).
    pub(crate) fn goal_edit_status(&self, id: &str, outcome: Result<GoalEdit>) -> String {
        match outcome {
            Ok(GoalEdit::Written { .. }) => {
                let mut s = format!("{id} goal updated — the agent sees it at its next check-in");
                if self
                    .selected_view()
                    .is_some_and(|v| v.id == id && !v.stops.is_empty())
                {
                    s.push_str(" (still blocked — press s to answer)");
                }
                s
            }
            Ok(GoalEdit::Unchanged) => format!("{id} goal unchanged — nothing written"),
            Err(e) => format!("{id} goal write failed: {e}"),
        }
    }

    /// Resolve the selected row to `(id, directive path, directive on disk)` — the directive
    /// twin of [`App::selected_brief`], and the ONE place `i` (and its `^E`/`^X`) decide WHICH
    /// `directive.md` they touch, so a selection change or registry rewrite while the overlay
    /// is up can never re-target them.
    ///
    /// Returns `None` after an explanatory status (never a bare return) on the dead ends —
    /// nothing selected, or a selected row whose registry entry is gone (so there is no path to
    /// write). A missing/unreadable `directive.md` is NOT a refusal: the field opens empty (no
    /// directive yet) and a save creates it. `what` names the pressed key's job.
    pub(crate) fn selected_directive(&mut self, what: &str) -> Option<(String, PathBuf, String)> {
        let Some(id) = self.selected_view().map(|v| v.id.clone()) else {
            self.status = format!("{what} (nothing is selected)");
            return None;
        };
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(entry) = reg.projects.iter().find(|p| p.id == id) else {
            self.status = format!("{id} is gone from the list — no directive to edit");
            self.refresh();
            return None;
        };
        // Per-session directive for an AgentLoop session (root directive for others) — the
        // same `entry_state_paths` subtree the decider reads fresh at each consult.
        let directive = entry_state_paths(entry).directive();
        let current = std::fs::read_to_string(&directive).unwrap_or_default();
        Some((id, directive, current))
    }

    /// `i` on a Normal-mode row: open the INLINE directive field for that session.
    ///
    /// AUTOPILOT-ONLY, the same gate `g` and `c` follow and for the same reason: `directive.md`
    /// is read by exactly one thing — the decider consult, which runs only when pmd drives the
    /// row (`daemon::pmd_drives_row`). On Standard the human at the keyboard IS the decider, so
    /// this field would edit a file nothing consults. Refuses and names the key that makes it
    /// matter, exactly as `g` does.
    ///
    /// A [`Composer`] like the goal field, SEEDED with `directive.md` as it stands — multi-line and
    /// all. An empty save still keeps the previous directive; `^X^R` is the deliberate clear.
    pub(crate) fn begin_directive_edit(&mut self) {
        let undriven = self
            .selected_view()
            .filter(|v| !agent_manager::daemon::pmd_drives_row(v.mode, v.tier))
            .map(|v| v.id.clone());
        if let Some(id) = undriven {
            self.status =
                format!("{id}: the directive limits autopilot's decider — press m to turn it on");
            return;
        }
        let Some((id, directive, current)) =
            self.selected_directive("i sets a session's directive")
        else {
            return;
        };
        let input = Composer::seeded(current.trim().to_string(), composer::DIRECTIVE);
        // `current` is not kept beside the buffer: it seeded it, and a second copy of the same text
        // could only drift from the one on screen. "Did this change anything?" is `apply_directive_edit`'s
        // question, answered against the file itself.
        self.mode = UiMode::EditingDirective {
            id,
            directive,
            input,
        };
    }

    /// Enter in the inline directive field: apply the typed directive to `directive.md` and close.
    ///
    /// Routes through [`apply_directive_edit`] — the SAME (and only) atomic `directive.md` write
    /// path the `^E` editor half uses. An empty/whitespace-only save KEEPS the previous directive
    /// (the anti-wipe guard the goal field shares): clearing the text is NOT rescinding, because
    /// rescind is the deliberate `^X` action. So an empty save here says "kept" and points at `^X`.
    pub(crate) fn submit_directive_edit(&mut self) {
        let UiMode::EditingDirective {
            id,
            directive,
            input,
            ..
        } = &self.mode
        else {
            return;
        };
        let (id, directive, typed) = (
            id.clone(),
            directive.clone(),
            input.text().trim().to_string(),
        );
        self.mode = UiMode::Normal;
        self.status = if typed.is_empty() {
            format!("{id} directive kept (empty save changes nothing — press ^X to rescind)")
        } else {
            let outcome = apply_directive_edit(&directive, &typed);
            self.directive_edit_status(&id, outcome)
        };
        self.refresh();
    }

    /// `^X^E` in the inline directive field: hand off to the external editor, seeded with the buffer.
    /// Mirrors [`App::escalate_goal_edit`]; the overlay closes first, because `run()` restores
    /// the terminal into whatever mode is current when the editor returns.
    pub(crate) fn escalate_directive_edit(&mut self) {
        let UiMode::EditingDirective {
            id,
            directive,
            input,
            ..
        } = &self.mode
        else {
            return;
        };
        // The buffer is the seed — see [`App::escalate_goal_edit`].
        let seed = input.text();
        let (id, directive) = (id.clone(), directive.clone());
        self.mode = UiMode::Normal;
        self.pending_directive_edit = Some(DirectiveEditReq {
            id,
            directive,
            current: seed,
        });
    }

    /// `^X^R` in the inline directive field: RESCIND the standing directive — the deliberate,
    /// explicit clear (never an empty save). Removes `directive.md` via [`rescind_directive`] so
    /// the decider's next consult sees no directive, closes the field, and says which happened.
    pub(crate) fn rescind_directive_from_field(&mut self) {
        let UiMode::EditingDirective { id, directive, .. } = &self.mode else {
            return;
        };
        let (id, directive) = (id.clone(), directive.clone());
        self.mode = UiMode::Normal;
        self.status = match rescind_directive(&directive) {
            Ok(true) => format!("{id} directive rescinded — the decider no longer sees it"),
            Ok(false) => format!("{id} had no directive — nothing to rescind"),
            Err(e) => format!("{id} directive rescind failed: {e}"),
        };
        self.refresh();
    }

    /// The status line for an applied directive edit, shared by BOTH halves (`i` inline via
    /// [`App::submit_directive_edit`], and `^E` via `run()`'s directive-edit drain) so the two
    /// cannot report the same file write differently. SHORT and front-loaded, because the keybar
    /// reserves at most a third of the bar and truncates the TAIL.
    pub(crate) fn directive_edit_status(&self, id: &str, outcome: Result<DirectiveEdit>) -> String {
        match outcome {
            Ok(DirectiveEdit::Written { .. }) => {
                format!("{id} directive set — the decider applies it at its next consult")
            }
            Ok(DirectiveEdit::Unchanged) => format!("{id} directive unchanged — nothing written"),
            Err(e) => format!("{id} directive write failed: {e}"),
        }
    }
}
