//! The PREVIEW's head: selected-session health and input ownership first, then the
//! agent's current report and the next concrete action. Rows are line-based and clipped
//! at the pane's right edge; only the current report has a bounded wrap budget because
//! every head row comes out of the transcript.

use crate::*;

/// Rows the agent's own `status` line may wrap over at most.
///
/// It used to be ONE, truncated — and the agent writes sentences: user, on a real screen, *"status  New
/// 10-joke target met: round 4 delivered a labeled 2-per-style sampler … beca…"* → *"It gets cutoff. I
/// think we should have a better status line"*. This is the ONLY channel for "what is the agent actually
/// doing / why did it stop", so losing its second half is losing the answer.
pub(crate) const PREVIEW_STATUS_MAX_ROWS: usize = 3;

/// How many rows this pane can spare for that status.
///
/// A BUDGET, not a preference: the head's rows come out of the log region, which is `Constraint::Min(0)`
/// between the head and the reserved stops block. At 100x14 one extra head row collapses the transcript
/// to its rule with nothing under it — measured, and guarded by
/// `preview_question_never_pushes_the_transcript_off_screen`. So a short pane keeps the single truncated
/// row it always had, and only a pane with room to spare gets the wrap.
pub(crate) const fn preview_status_rows(inner_h: u16) -> usize {
    if inner_h < 16 {
        1
    } else if inner_h < 18 {
        2
    } else {
        PREVIEW_STATUS_MAX_ROWS
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LoopPreviewState<'a> {
    pub(crate) entry: Option<&'a (ProjectPaths, String, String)>,
    pub(crate) report: Option<&'a job::WakeReport>,
    pub(crate) ledger: Option<&'a AgentLoopState>,
    pub(crate) checkpoint: Option<&'a state::WorkerCheckpoint>,
}

/// The PREVIEW's fixed head rows: selected-session health and input ownership, followed by
/// the agent's current report and the next action. The pane title already carries the session
/// identity and attach target, so the body does not repeat either one.
///
/// `lvl` is passed in rather than resolved here because the `Stops` rule wears the SAME
/// attention level (see [`render_preview`]), and two reads of it could disagree about one
/// row inside one frame.
pub(crate) fn preview_head(
    v: &ProjectView,
    inner_w: u16,
    inner_h: u16,
    lvl: attention::Level,
    loop_state: LoopPreviewState<'_>,
) -> Vec<Line<'static>> {
    let LoopPreviewState {
        entry: loop_entry,
        report,
        ledger,
        checkpoint,
    } = loop_state;
    let dim = Style::default().add_modifier(Modifier::DIM);
    let (glyph, gcolor) = status_glyph(v);
    // A staged spawn is disabled, but nothing paused it: the spawn broker is launching it.
    let header_label = if v.spawn_staged {
        "starting…"
    } else if v.enabled {
        primary_status_label(v)
    } else {
        "paused"
    };
    // IS ANYTHING DRIVING THIS ROW? The daemon's own predicate plus the registry's enabled bit,
    // so a paused row cannot claim pmd owns input or show a schedule the sweep does not keep.
    let driven = v.enabled && agent_manager::daemon::pmd_drives_row(v.mode, v.tier);
    // NO CHECK-IN AND NO TIMER ON AN UNDRIVEN ROW. User: *"When i swap the mode to standard from
    // autopilot, i don't want check in or timer in the ui"* — and they are right that it was a lie
    // rather than clutter: `next: monitoring · check in 3377s · every 1m` describes a heartbeat
    // that, on Standard, nobody runs. The park is still ON the ledger (it is what pmd would resume
    // from), but a number counting down to an event that will not happen is worse than no number.
    // A human attached to the terminal temporarily owns input.
    let held_by_human = driven && v.human_attached;
    let next_owned = if v.spawn_staged {
        // Every start refuses this row (`start_refusal`) until its broker finishes, so Enter
        // resumes nothing: the broker's launch is the next thing that happens.
        Some("being created by a spawn request".to_string())
    } else if v.incomplete_fork {
        // Every start refuses this row (`incomplete_fork_refusal`), so deleting it is the only step.
        Some("no conversation captured · delete it with d".to_string())
    } else if held_by_human {
        Some("pmd resumes automatically on detach".to_string())
    } else if driven {
        None
    } else if v.enabled {
        let tail = "you drive it";
        Some(match v.next_action.split(" · ").next().unwrap_or("") {
            "" | "—" => tail.to_string(),
            current if current == tail => tail.to_string(),
            current => format!("{current} · {tail}"),
        })
    } else {
        // PAUSED: nothing runs until Enter resumes it, whatever the frozen ledger or a tier left
        // on Autopilot by a failed config write says, so that is the one next step to show.
        Some("Enter resumes".to_string())
    };
    // DOES A HUMAN HAVE TO ACT? Driven by the POSTURE (needs-you / stuck), NOT by the raw marker
    // state. When someone genuinely must act the posture already reflects it — the daemon folds a
    // `blocked` marker into the ledger (→ NeedsYou), and even with autopilot OFF `agent_loop` forces
    // NeedsYou from a blocked marker. Reading the marker's `blocked` directly instead let a WORKING
    // row (a live wake, posture Running) inherit the PREVIOUS wake's stale `blocked` and drop its
    // `next:` as if it were waiting on someone (user: *"when the session is working, i see the status
    // blocked on top of my status"*). When a human must act, the header DROPS the next fact — the
    // attention badge and current report already say "waiting on you", and the old
    // `next: blocked · confirm_done` was a third, rawer copy (user: *"It displays both blocked and
    // needs you and a bunch of not intuitive message"*).
    let needs_human = v.posture.needs_attention();
    // What to show after `next:`, or `None` to drop it. An OWNED next (undriven / held-by-chat)
    // is always meaningful and shown; otherwise it is hidden while a human is the blocker.
    let next_display: Option<String> = match &next_owned {
        Some(n) => Some(n.clone()),
        None if needs_human => None,
        None => current_decider_activity(v, SystemClock.now()).or_else(|| {
            if v.next_action.is_empty() {
                Some("—".to_string())
            } else {
                Some(v.next_action.clone())
            }
        }),
    };

    // One consistent separator between every top-level fact. Dynamic current/next text lives on
    // later rows; this row answers only "is it healthy, and who owns input?" before lower-priority
    // engine/mode/cadence identity.
    let on_style = Style::default()
        .fg(agent_manager::theme::live())
        .add_modifier(Modifier::BOLD);
    let autopilot_on = v.tier == Some(Tier::Autopilot);

    // THE SAME BADGE THE ROW WEARS — row and preview read as one system. When the badge is a FILLED
    // chip (needs you / stuck / soft), the status GLYPH lives INSIDE the fill so `◐ needs you ` reads
    // as one reverse-video block; a bare glyph in front of the chip left the icon floating outside the
    // background colour (user: *"when it displays needs you … the icon renders separately and not in
    // background color"*). Unfilled levels keep the glyph in its own status colour — there is no fill
    // for it to sit inside.
    let mut line1: Vec<Span<'static>> = if lvl.is_filled() {
        vec![Span::styled(
            format!(" {glyph} {header_label} "),
            lvl.label_style(),
        )]
    } else {
        vec![
            Span::styled(format!("{glyph} "), Style::default().fg(gcolor)),
            Span::styled(
                format!(" {header_label} "),
                Style::default().fg(gcolor).add_modifier(Modifier::BOLD),
            ),
        ]
    };
    let sep = |line: &mut Vec<Span<'static>>| line.push(Span::styled(" · ", dim));

    // Input ownership is the first fact after health; it remains visible before optional metadata
    // starts clipping on a narrow preview.
    sep(&mut line1);
    if v.human_attached {
        line1.push(Span::styled(
            "human attached",
            Style::default()
                .fg(agent_manager::theme::brand())
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        line1.push(Span::styled("control ", dim));
        let owner = if driven { "pmd" } else { "you" };
        line1.push(Span::styled(
            owner,
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }

    // Waiting age and count stay beside the health/ownership group. Only a driven row with open
    // stops has an answer queue; an undriven row must not advertise a queue nothing serves.
    if let Some(since) = v
        .oldest_stop_since
        .filter(|_| !v.stops.is_empty() && driven)
    {
        let n = v.stops.len();
        let plural = if n == 1 { "decision" } else { "decisions" };
        sep(&mut line1);
        line1.push(Span::styled(
            format!(
                "{} waiting {} · {n} {plural}",
                attention::WAITING,
                age_label(Some(since), SystemClock.now())
            ),
            match lvl.hue() {
                Some(c) => Style::default().fg(c).add_modifier(Modifier::BOLD),
                None => dim,
            },
        ));
    }

    if let Some(engine) = v.engine {
        sep(&mut line1);
        line1.push(Span::styled(engine.label(), dim));
    }

    sep(&mut line1);
    let mode = match v.tier {
        Some(Tier::Autopilot) => "autopilot",
        Some(Tier::Standard) => "standard",
        None => "mode ?",
    };
    line1.push(Span::styled(
        mode,
        if autopilot_on { on_style } else { dim },
    ));
    if loop_entry.is_some() {
        // THE CADENCE, something two parties can change (`c`, and the agent's own
        // `WakeReport::cadence_s`). From the LEDGER first — the value `job_engine` consults —
        // else `DEFAULT_CADENCE_S`. ONLY when something drives the row.
        if driven {
            let cadence = ledger
                .and_then(|l| l.cadence_s)
                .unwrap_or(job_engine::DEFAULT_CADENCE_S);
            sep(&mut line1);
            line1.push(Span::styled("every ", dim));
            line1.push(Span::raw(job_engine::human_cadence(cadence)));
        }
    }

    let mut head: Vec<Line> = vec![Line::from(line1)];

    // One lineage fact. Fork lineage wins when a row carries both links (a fork of a spawned
    // child copies its source's `spawned_by`); spawn lineage names the parent by its label.
    if let Some(parent) = &v.forked_from {
        head.push(preview_fact_line(
            "fork",
            format!("from {parent} · shared directory"),
        ));
    } else if let Some(parent) = &v.spawned_by_label {
        head.push(preview_fact_line("spawned", format!("from {parent}")));
    }

    if loop_entry.is_some() {
        head.extend(preview_marker_lines(
            v,
            inner_w,
            preview_status_rows(inner_h),
            report,
            ledger,
        ));
    }
    if let Some(next) = next_display {
        head.push(preview_fact_line("next", next));
    }
    if loop_entry.is_some() {
        head.extend(preview_checkpoint_lines(checkpoint, inner_w, inner_h));
    }
    head
}

const PREVIEW_FACT_COL: usize = 9;

fn preview_fact_line(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<PREVIEW_FACT_COL$}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value),
    ])
}

/// The agent's own current fact, under the ownership/health line and only for an
/// agent-loop row. The freshest marker status wins, with the persisted ledger status as
/// fallback; a divergent marker state is retained only when the harness escalated past it.
pub(crate) fn preview_marker_lines(
    v: &ProjectView,
    inner_w: u16,
    status_rows: usize,
    report: Option<&job::WakeReport>,
    ledger: Option<&AgentLoopState>,
) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // A real escalation keeps both truths: the head names the harness posture while `current`
    // preserves the agent's last self-declared state. A stale blocked marker never talks over a calm
    // or actively working posture.
    let state = report
        .filter(|r| v.posture.needs_attention() && !wake_state_matches_posture(r.state, v.posture))
        .map(|r| wake_state_str(r.state));

    // The freshest marker status wins; the ledger's persisted last status is the fallback. Empty
    // sessions render no placeholder, and current status never carries scheduling metadata.
    let note = report
        .and_then(|r| r.status.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ledger
                .and_then(|l| l.last_status.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });

    let current = match (state, note) {
        (Some(state), Some(note)) => Some(format!("{state} · {note}")),
        (Some(state), None) => Some(state.to_string()),
        (None, Some(note)) => Some(note.to_string()),
        (None, None) => None,
    };
    let Some(current) = current else {
        return Vec::new();
    };

    // Wrap only the current value. The label owns a stable column and continuation rows retain that
    // indent, so the hierarchy remains legible without the old filled chip or decorative divider.
    let text_w = usize::from(inner_w).saturating_sub(PREVIEW_FACT_COL).max(1);
    let rows = wrap_clamped(&current, text_w, status_rows.max(1));
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        if i == 0 {
            out.push(preview_fact_line("current", row));
        } else {
            out.push(Line::from(vec![
                Span::styled(" ".repeat(PREVIEW_FACT_COL), dim),
                Span::raw(row),
            ]));
        }
    }
    out
}

/// Does the agent's self-declared [`job::WakeState`] name the SAME thing the head badge (the
/// harness [`Posture`]) already shows? For an agent-loop row the posture is derived from this
/// wake state via the ledger — `working`→`Running`, `monitoring`→`Monitoring`,
/// `blocked`→`NeedsYou`/`Stuck` (see `view::agent_loop::agent_loop_posture`) — so when they
/// agree, the report line's state badge is a duplicate and is dropped. Only a real divergence
/// (a harness escalation over a stale marker) makes this false, and there both are shown.
fn wake_state_matches_posture(state: job::WakeState, posture: Posture) -> bool {
    match state {
        job::WakeState::Working => posture == Posture::Running,
        job::WakeState::Monitoring => posture == Posture::Monitoring,
        job::WakeState::Blocked => matches!(posture, Posture::NeedsYou | Posture::Stuck),
    }
}
