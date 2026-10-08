//! `s` — the send field, from opening it to the bytes reaching the pane. The target
//! is resolved once when the field opens and probed only at submit, because a human
//! may spend a minute typing; the decision itself is `send_decision`'s, so this is the
//! impure half: probe, deliver under the chat interlock, and report.

use crate::*;

impl App {
    /// `s` — open the send field for the selected row.
    ///
    /// Resolves the target ONCE here (registry lookup + the deterministic session name)
    /// and probes NOTHING: liveness is checked at submit time, because the human may
    /// spend a minute typing and a probe taken now would be stale by then.
    pub(crate) fn begin_send(&mut self) {
        let Some(v) = self.selected_view() else {
            self.status = "s sends text to a session's agent (nothing is selected)".into();
            return;
        };
        let (id, mode, tier) = (v.id.clone(), v.mode, v.tier);
        let reg = Registry::load(&self.registry_path).unwrap_or_default();
        let Some(e) = reg.projects.iter().find(|p| p.id == id) else {
            // Same shape as `request_attach`/`cycle_tier`: drop the stale row, or every
            // later press takes this branch forever with nothing changing.
            self.status = format!("{id} is gone from the list");
            self.refresh();
            return;
        };
        if self.refuse_unstartable(e) {
            return;
        }
        let session = session_name(&id, &e.root);
        let input = self.message_drafts.remove(&id).unwrap_or_default();
        self.mode = UiMode::Sending {
            target: SendTarget {
                id,
                root: e.root.clone(),
                session,
                agent_loop: true,
                driven: daemon::pmd_drives_row(mode, tier),
                // Resolved at SUBMIT, not here: a chat can open or close while the human types.
                in_chat: false,
            },
            input,
        };
    }

    /// Close the inline composer without losing its unsent text. Empty fields do not
    /// create placeholder drafts; non-empty fields retain the caret as well as the bytes.
    pub(crate) fn park_send_draft(&mut self) {
        let draft = match &self.mode {
            UiMode::Sending { target, input } => Some((target.id.clone(), input.clone())),
            _ => None,
        };
        let Some((id, input)) = draft else {
            return;
        };
        if input.is_empty() {
            self.message_drafts.remove(&id);
            self.status = "message cancelled".into();
        } else {
            self.message_drafts.insert(id.clone(), input);
            self.status = format!("draft saved for {id}");
        }
        self.mode = if self.return_to_board_after_send {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.return_to_board_after_send = false;
    }

    pub(crate) fn restore_send_draft(&mut self, id: String, input: Composer) {
        if !input.is_empty() {
            self.message_drafts.insert(id, input);
        }
    }

    /// Point `t.session` at the pane that currently HOLDS this conversation.
    ///
    /// THE BUG THIS CLOSES: *"when i try to send, it displays this 'a chat holds this session —
    /// type it there' but the chat is idle"*. `s` always aimed at the daemon's `pmloop-` pane, but
    /// on a Standard row Enter creates the conversation in a `pmchat-` pane, and that pane STAYS
    /// ALIVE after Ctrl+q — that is the survive/re-attach model, not a leak. `chat_lock::is_active`
    /// defers on any live chat session ("ALIVE => defer, full stop. No age cap, no reap"), so `s`
    /// refused forever and pointed the human at a pane nobody was sitting in.
    ///
    /// The interlock was right that two writers on one conversation is a hazard; it was aiming at
    /// the wrong pane. So: the loop pane wins when it is up (pmd owns it), else a live chat pane
    /// holds the conversation and gets the bytes. Mirrors `preview_log_source`, which already picks
    /// between these two panes for the preview. A human ATTACHED to whichever pane we land on is
    /// still refused by `send_decision` — that guard is about not mangling a half-typed draft, and
    /// it is unchanged.
    fn resolve_send_pane(&self, t: &mut SendTarget) {
        if !t.agent_loop {
            return;
        }
        let d = &*self.agent_tmux;
        if d.is_alive(&t.session).unwrap_or(false) {
            return; // the loop pane is up and owns the conversation
        }
        let chat = session_name(&t.id, &t.root);
        if d.is_alive(&chat).unwrap_or(false) {
            t.session = chat;
            t.in_chat = true;
        }
    }

    /// Probe the target and decide. Split from [`App::submit_send`] so the decision is
    /// one pure function ([`send_decision`]) over observed facts.
    pub(crate) fn plan_send(&mut self, t: &SendTarget, text: &str) -> SendPlan {
        if let Some(refusal) = send_precheck(text) {
            return refusal;
        }
        // AN OPEN STOP IS A REFUSAL, not a warning, and this is the one place `s` could
        // wedge autopilot. While `run == Blocked`, every tick re-emits `Escalated` until
        // `answers.json` carries an answer for the open stop id; only `nudge` clears
        // `open_stops`, and `on_blocked` never consults the chat marker. So a human who
        // ANSWERS THE QUESTION with `s` gets a happy agent and a row that stays "needs
        // you" while pmd never nudges again — the exact complaint this project started
        // from, reproduced by its newest feature. Entry through `s` now routes to the
        // Answer surface; this guard still closes the race where a stop opens after
        // the Message composer was already open.
        if self
            .projects
            .iter()
            .find(|view| view.id == t.id)
            .is_some_and(|view| !view.stops.is_empty())
        {
            return SendPlan::Refuse("stop open — cancel, then press s to answer it".into());
        }
        // The attach-intent marker closes the brief mark-before-client window and
        // self-clears after its grace period. Refuse direct input while that window
        // is active so a send cannot race an attach.
        //
        // Deliberately NOT the driver lease: pmd acquires `driver.lock` once and holds it
        // for the runner's whole life, so gating on it would make `s` refuse exactly when
        // autopilot is ON. A reviewer will propose it; this is why not.
        // Only reachable now when a chat MARKER is active but its session is not yet alive —
        // `resolve_send_pane` has already redirected us into any live chat pane. That leaves exactly
        // one case: the mark-before-launch window, where a REPL is starting on this conversation.
        // Typing into the loop pane then would race a launch, so it still refuses — but it says what
        // is actually true instead of pointing at an idle pane.
        if t.agent_loop && !t.in_chat {
            let paths = ProjectPaths::for_session(&t.root, &t.id);
            if chat_lock::is_active(&paths, SystemClock.now()) {
                return SendPlan::Refuse("a chat is opening on this session — try again".into());
            }
        }
        let d = &*self.agent_tmux;
        let probe = SendProbe {
            alive: d.is_alive(&t.session).ok(),
            pane_dead: d.pane_dead(&t.session).unwrap_or(false),
            // `Driver::has_clients` DEFAULTS to `Ok(true)` (assume attached), which is
            // right for the reaper and wrong here — a probe error must not make a
            // human-pressed key dead. An error reads as "not attached"; the pane-state
            // gate below is the one that fails closed.
            attached: d.has_clients(&t.session).unwrap_or(false),
            in_mode: d.pane_in_mode(&t.session).unwrap_or(false),
            capture: d.capture_tail(&t.session, SEND_GATE_LINES).ok(),
        };
        send_decision(t, &probe)
    }

    /// Enter in the send field: plan, then deliver. The field STAYS OPEN with the text
    /// intact unless the bytes actually reached the pane.
    pub(crate) fn submit_send(&mut self) {
        let (target, text) = match &self.mode {
            UiMode::Sending { target, input } => (target.clone(), input.text()),
            _ => return,
        };
        let mut target = target;
        self.resolve_send_pane(&mut target);
        match self.plan_send(&target, &text) {
            SendPlan::Refuse(why) => self.status = why,
            SendPlan::Send(receipt) => match self.deliver(&target, &text) {
                Ok(()) => {
                    self.message_drafts.remove(&target.id);
                    self.mode = if self.return_to_board_after_send {
                        UiMode::Board
                    } else {
                        UiMode::Normal
                    };
                    self.return_to_board_after_send = false;
                    self.status = receipt;
                    self.refresh();
                }
                // Reported VERBATIM. Every keybar status is drawn green, which would
                // paint a failure as a success, so `keybar_line` now colours a failure red.
                Err(e) => self.status = format!("send failed: {e}"),
            },
        }
    }

    /// Write the bytes. The ONE place `s` touches a pane.
    ///
    /// Claims the chat interlock, waits out any nudge already in flight (see
    /// [`SEND_QUIESCE_MS`]), sends, and releases the interlock on EVERY exit path —
    /// success, failure, or an empty payload — so a marker can never leak from here.
    ///
    /// C1 ("one conversation, one writer") is not in play: `s` launches nothing and
    /// resumes nothing. It writes bytes to the pty of the single already-running process,
    /// so there is still exactly one agent, one transcript, one appender, and the human's
    /// text becomes one ordinary user turn. The sharper form: if `s` violated C1 then so
    /// would `JobScheduler::nudge`, which is the identical `send_keys` on the identical
    /// pane, and so would `attach_loop`, where a human types into that pane with no
    /// marker at all. `s` writes NO ledger and NO `answers.json`.
    pub(crate) fn deliver(&mut self, t: &SendTarget, text: &str) -> Result<()> {
        // `tmux::sanitize_send_text`, not `advise::sanitize_control_bytes`: newlines are
        // legitimate here (`send_keys` routes them through `load-buffer`/`paste-buffer`),
        // but a raw ESC arrives as shift+tab and cycles claude's permission mode, and a
        // lone CR submits a second message. The inline field cannot produce either, but
        // the `$EDITOR` buffer and a bracketed paste both can.
        let payload = tmux::sanitize_send_text(text);
        if payload.is_empty() {
            anyhow::bail!("nothing left after removing control characters");
        }
        // NOT when we are typing into the chat pane itself: that pane's own liveness is already
        // what makes pmd defer, and claiming/releasing the marker here would CLEAR the real chat's
        // marker on the way out — turning a send into a way to unpark the poll under a live REPL.
        let paths = ProjectPaths::for_session(&t.root, &t.id);
        let _input_lease =
            lease::acquire_with_retry(&paths.input_lock(), 40, Duration::from_millis(25))?
                .ok_or_else(|| {
                    anyhow::anyhow!("another input is still being delivered — try again")
                })?;
        if self.agent_tmux.is_alive(&t.session).ok() == Some(false) {
            anyhow::bail!("the agent's session ended while we waited");
        }
        if self.agent_tmux.has_clients(&t.session).unwrap_or(true) {
            anyhow::bail!("a client attached while the message was waiting");
        }
        let capture = self
            .agent_tmux
            .capture_tail(&t.session, SEND_GATE_LINES)
            .context("re-read pane before send")?;
        if tmux::classify_dialog(&capture).is_some() || !tmux::has_composer(&capture) {
            anyhow::bail!("the pane changed while the message was waiting");
        }
        self.agent_tmux.send_keys(&t.session, &payload)
    }

    /// Ctrl+E in the send field: hand off to `$EDITOR`, seeded with what has been typed.
    ///
    /// Mirrors the goal field's escalation, with two deliberate differences. It needs its
    /// OWN parser: a message goes through `tmux::sanitize_send_text`, which maps the raw ESC
    /// (arrives as shift+tab and cycles claude's permission mode) and the lone CR (submits a
    /// second turn) that a pane-bound message must not carry — control handling the goal's
    /// plain-text parser has no reason to do. And its own temp filename, so a concurrent goal
    /// edit cannot collide with it.
    pub(crate) fn escalate_send(&mut self) {
        let (target, input, cursor) = match &self.mode {
            UiMode::Sending { target, input } => (target.clone(), input.text(), input.cursor()),
            _ => return,
        };
        // The overlay closes first: `run()` restores the terminal into whatever mode is
        // current when the editor returns.
        self.mode = if self.return_to_board_after_send {
            UiMode::Board
        } else {
            UiMode::Normal
        };
        self.pending_send = Some(SendReq {
            target,
            seed: input,
            cursor,
        });
    }
}
