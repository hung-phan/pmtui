//! The detail pane for the selected row, and the LOG it tails. Which source that log
//! comes from is a decision of its own (`preview_log_source`) because a session can be
//! mid-wake, parked, or driven by a chat REPL, and showing the wrong one reads as "it
//! is doing nothing".
//!
//! `render_preview` is the composer, and it owns the disk reads; the three regions it lays
//! out are a file each — `head` (the status/metadata line plus the agent's marker report),
//! `stop_block` (the decisions RESERVED out of the log's height), and `log` (where the text
//! comes from and how much of its tail fits). The ORDER is the layout: the two fixed
//! regions are built first, and the transcript gets what they leave.

mod autopilot;
mod checkpoint;
mod head;
mod log;
mod stop_block;

// The one decision-tone table, shared with the fleet decision lane (`render::decisions`) — the only
// thing left in the `autopilot` module now that the inline preview feed has been removed (pmd's
// decisions live in the `v` lane; the preview shows only the agent's own output).
pub(crate) use autopilot::autopilot_tone;
pub(crate) use checkpoint::*;
pub(crate) use head::*;
pub(crate) use log::*;
pub(crate) use stop_block::*;

use crate::*;

/// Minimum height for the detached agent pane. The actual fit grows above the preview viewport and
/// follows deep scrolling in chunks; this floor matches the launch default.
pub(crate) const AGENT_PANE_FIT_ROWS: u16 = 50;
const AGENT_PANE_FIT_LOOKAHEAD_ROWS: usize = 100;
const AGENT_PANE_FIT_CHUNK_ROWS: usize = 50;
const AGENT_PANE_FIT_MAX_ROWS: usize = 2_000;

pub(crate) fn preview_pane_fit_rows(viewport_rows: u16, back: usize) -> u16 {
    let wanted = usize::from(viewport_rows)
        .saturating_add(back)
        .saturating_add(AGENT_PANE_FIT_LOOKAHEAD_ROWS)
        .max(usize::from(AGENT_PANE_FIT_ROWS));
    let rounded = wanted.saturating_add(AGENT_PANE_FIT_CHUNK_ROWS - 1) / AGENT_PANE_FIT_CHUNK_ROWS
        * AGENT_PANE_FIT_CHUNK_ROWS;
    u16::try_from(rounded.min(AGENT_PANE_FIT_MAX_ROWS)).unwrap_or(u16::MAX)
}

// Compile-time guard: the fit height must stay comfortably TALLER than the ~12-row visible transcript
// window, or a full-screen TUI leaves nothing above the tail to scroll into (the "cannot scroll to the
// top" bug). Lowering this below the window is a compile error, not a runtime surprise.
const _: () = assert!(AGENT_PANE_FIT_ROWS >= 40);

/// How long a PREVIEW capture may be reused before the next draw re-forks `tmux capture-pane` (see
/// the cache in `render_preview` + [`App::preview_capture`]). Under the idle 500ms poll every draw
/// re-captures (the cache is always older than this), so freshness is unchanged there; it only
/// spares the extra ~8fps animation frames while a row is on autopilot. Kept below the refresh
/// cadence so the transcript never lags it.
const PREVIEW_CAPTURE_TTL: std::time::Duration = std::time::Duration::from_millis(400);

/// A cached PREVIEW capture (held in [`App::preview_capture`]): the [`PreviewLog`] and the inputs it
/// was computed from, so `render_preview` can reuse it across the ~8fps rainbow-animation frames
/// instead of re-forking `tmux capture-pane` each one. Reused only while `id`/`depth`/`driven` match
/// the current draw and `at` is younger than [`PREVIEW_CAPTURE_TTL`].
pub(crate) struct PreviewCapture {
    pub(crate) id: String,
    pub(crate) depth: usize,
    pub(crate) driven: bool,
    pub(crate) at: std::time::Instant,
    pub(crate) log: PreviewLog,
}

/// The PREVIEW pane (right in the wide tier, below the list in the stacked one),
/// dominated by the agent's OWN OUTPUT rather than by metadata. Top to bottom:
///
/// 1. one compact header line — colored status glyph + posture + `autopilot X ·
///    next: … · conv <8-char>` (`step #N` only when it is a real, non-zero counter — never for an
///    agent-loop row, which is not phase-stepped);
/// 2. for an agent-loop row, the agent's latest machine report state (only on a genuine harness
///    escalation) read from the MARKER file, and its one-line human-facing `status` (the marker's,
///    else the ledger's persisted `last_status`) — nothing at all when it has never reported;
/// 3. a `Log` section holding the TAIL of whatever the agent is actually saying,
///    sized to the pane's ACTUAL height (what was a hard-coded 6 lines in a fixed
///    8-row pane). Source per [`preview_log_source`]: the live tmux pane of the
///    session's persistent agent, else a legacy per-wake `steps/<seq>.log`
///    transcript, else an honest placeholder;
/// 4. when the session has open stops, a short `Stops` tail section whose rows are
///    RESERVED before the log, so the thing a human must act on is never pushed
///    off-screen.
///
/// READ-ONLY (pmtui never writes the ledger — only `pmd` does; the tmux side is a
/// `has-session` probe plus a `capture-pane`, never a keystroke) and panic-safe:
/// every height/index computation is `saturating_*`/clamped, and any
/// missing/unreadable file or failed capture degrades to a dim `—` placeholder
/// instead of crashing.
///
/// COST: the pane capture shells out to tmux, so it runs at the render tick (~2Hz)
/// for the SELECTED row ONLY — `render` draws exactly one preview per frame, and
/// the list rows never capture.
pub(crate) fn render_preview(f: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let Some(v) = app.selected_view() else {
        f.render_widget(
            Paragraph::new(Line::styled("no projects registered", dim)).block(
                Block::bordered()
                    .border_style(attention::rule())
                    .title(" PREVIEW ")
                    .title_style(attention::pane_title()),
            ),
            area,
        );
        return;
    };

    // ---- read-only facts on disk --------------------------------------------
    // Only an agent-loop row has an agent of its own; resolve its per-session state
    // dir AND its persistent agent's tmux session name from the registry the same
    // way `cycle_tier`/`request_attach` do (the session name comes from
    // `tmux::session_name`, never re-derived here, so pmtui and pmd can never
    // disagree about which pane belongs to this session). Any other mode (or a row
    // with no registry entry) leaves this None and shows a dim placeholder.
    let loop_entry: Option<(ProjectPaths, String, String)> = if v.mode == Mode::AgentLoop {
        Registry::load(&app.registry_path)
            .unwrap_or_default()
            .projects
            .iter()
            .find(|p| p.id == v.id)
            .map(|p| {
                (
                    entry_state_paths(p),
                    session_name(&p.id, &p.root),
                    // The CHAT session too: the Log falls back to it, so the human's own REPL is
                    // rendered rather than reported as "not running". Same deterministic naming as
                    // everywhere else — never re-derived locally.
                    session_name(&p.id, &p.root),
                )
            })
    } else {
        None
    };
    // The agent's machine report is the MARKER file (`needs-you.json`) — ONE file
    // per session that the agent overwrites (atomically, monotonic `seq`) at every
    // decision point. The old `steps/<seq>.result.json` path is a leftover from the
    // ephemeral per-wake worker model and is never written by the persistent loop,
    // which is why this line used to read "no wakes yet" forever.
    let report = loop_entry
        .as_ref()
        .and_then(|(p, ..)| job_engine::parse_report(&p.needs_you()).ok());
    // The ledger carries the authoritative conversation id and the last disposed
    // human-facing status; tolerate a missing/malformed ledger.
    let ledger = loop_entry
        .as_ref()
        .and_then(|(p, ..)| job::load(p).ok().flatten());
    // Supplemental agent-owned continuity state. It is preview-only: malformed, oversized, or
    // unsupported content disappears without affecting the ledger, marker, or scheduler.
    let checkpoint = loop_entry
        .as_ref()
        .and_then(|(p, ..)| state::read_checkpoint(&p.checkpoint()).ok().flatten());

    // Fit BEFORE capturing. Modern Claude uses tmux's alternate screen, which has no tmux
    // scrollback; capture-pane can return only the pane's current rows. A fixed 50-row pane gives a
    // tall preview a max scroll of zero. Keep stable lookahead above the viewport and grow in chunks
    // as the human scrolls, avoiding a resize on every wheel notch.
    if let Some((_, session, chat)) = loop_entry.as_ref() {
        app.fit_agent_pane(
            &v.id,
            session,
            chat,
            area.width.saturating_sub(2),
            preview_pane_fit_rows(area.height.saturating_sub(2), app.detail_scroll),
        );
    }

    // Where the Log section's text comes from. Resolved BEFORE the block because
    // the pane title names the source; `area.height - 2` is the inner height (the
    // two border rows), an upper bound on the Log region — and the body still takes the
    // tail that actually fits.
    //
    // The capture depth FOLLOWS THE SCROLL: `region + detail_scroll + PREVIEW_SCROLLBACK`. The window
    // `pane_window` opens can never reach further back than the capture handed to it, so a fixed depth
    // was a hard scroll floor — user: *"user should be able to scroll all the way to the top"*. Adding
    // `detail_scroll` guarantees the current window is always captured, and the `+ PREVIEW_SCROLLBACK`
    // lookahead keeps `detail_max` ahead of where the human is, so each page up reveals another page
    // until tmux's own history runs out (the true top). It stays cheap while FOLLOWING (scroll 0 ⇒
    // just region + lookahead) and deepens only as far as the human has actually scrolled.
    let capture_depth =
        usize::from(area.height.saturating_sub(2)) + app.detail_scroll + PREVIEW_SCROLLBACK;
    // REUSE a recent capture rather than re-forking `tmux capture-pane` on every draw: the autopilot
    // rainbow animates at ~8fps (the run loop polls fast while a row is on autopilot), and a capture
    // per frame would be a fork storm. Keyed by the SELECTED row + scroll depth — a change to either
    // (j/k, wheel) re-captures at once — and by age: past PREVIEW_CAPTURE_TTL the next draw refreshes
    // it, so the transcript still updates ~2×/sec and the idle 500ms path is unchanged (its cache is
    // always older than the TTL). Only the extra animation frames in between reuse it, forking nothing.
    // The daemon's own predicate, so the placeholder cannot claim autopilot is off on a row the
    // sweep is driving — AND part of the cache key, since it decides the Empty message.
    let driven = agent_manager::daemon::pmd_drives_row(v.mode, v.tier);
    let fresh = {
        let c = app.preview_capture.borrow();
        c.as_ref()
            .filter(|pc| {
                pc.id == v.id
                    && pc.depth == capture_depth
                    && pc.driven == driven
                    && pc.at.elapsed() < PREVIEW_CAPTURE_TTL
            })
            .map(|pc| pc.log.clone())
    };
    let log_src = match fresh {
        Some(log) => log,
        None => {
            let log = match loop_entry.as_ref() {
                Some((paths, session, chat)) => preview_log_source(
                    app.agent_tmux.as_ref(),
                    session,
                    chat,
                    paths,
                    capture_depth,
                    driven,
                    v.job,
                ),
                None => PreviewLog::Empty("agent-loop sessions only"),
            };
            *app.preview_capture.borrow_mut() = Some(PreviewCapture {
                id: v.id.clone(),
                depth: capture_depth,
                driven,
                at: std::time::Instant::now(),
                log: log.clone(),
            });
            log
        }
    };
    // Every start refuses a staged spawn or a failed fork's leftover, so neither empty placeholder
    // may name `m`: each names why the row cannot start instead.
    let log_src = match log_src {
        PreviewLog::Empty(_) if v.spawn_staged => {
            PreviewLog::Empty("nothing to show yet — being created by a spawn request")
        }
        PreviewLog::Empty(_) if v.incomplete_fork => {
            PreviewLog::Empty("no conversation to show — delete this fork with d")
        }
        other => other,
    };

    // ---- the frame ----------------------------------------------------------
    // The title names the log's SOURCE, so "live pane" vs "Wake #N" tells the human
    // whether they are watching the running agent or an archived wake transcript.
    let title = match &log_src {
        PreviewLog::Pane(_) => format!(" {} · live ", v.label()),
        // Named, because "whose output am I reading" is the whole point of titling the source: the
        // chat is the human's own conversation, not the agent pmd drives.
        PreviewLog::Chat(_) => format!(" {} · chat ", v.label()),
        PreviewLog::Step(seq) => format!(" {} · Wake #{seq} ", v.label()),
        PreviewLog::Job => format!(" {} · job ", v.label()),
        PreviewLog::Empty(_) => format!(" {} ", v.label()),
    };
    // HOW FAR BACK the transcript is held, when it is held at all — the same indicator the STATUS
    // pane wears, and for the same reason: a scrolled pane and a quiet agent look identical
    // otherwise, so a human who has scrolled up has no way to tell that the live tail is elsewhere.
    let title = format!("{title}· open ");
    let title = match app.detail_scroll {
        0 => title,
        back => format!("{title}\u{2191}{back} "),
    };
    app.preview_attach_hit.set(Rect::new(
        area.x.saturating_add(1),
        area.y,
        u16::try_from(text_cols(&title))
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(2)),
        1,
    ));
    // No focus highlight — the pane is not "activated". The border is the quiet rule; the title
    // still carries its `↑N` scroll indicator (built into `title` above) so a scrolled transcript
    // is distinguishable from a quiet one.
    // Deliberately NO block-level padding here, unlike the text-dense SESSIONS/STATUS panes: the
    // preview is a CONTENT pane (the agent's transcript + a dense head meta line + the full-width
    // `preview` section rule). Block padding shrank that rule and truncated the head's tail
    // ("… you drive it" → "you drive"), so the pane keeps its full inner width; the regions inside
    // carry their own leading indent where they want breathing room.
    let block = Block::bordered()
        .border_style(attention::rule())
        .title(title)
        .title_style(attention::pane_title());
    let inner = block.inner(area);
    let inner_w = inner.width;
    f.render_widget(block, area);

    // ---- the three regions -------------------------------------------------
    // THE ATTENTION LEVEL, resolved once: the head badge and the `Stops` rule both wear
    // it, and two reads of it could disagree about the same row inside one frame.
    let lvl = attention::level(v);
    let head = preview_head(
        v,
        inner_w,
        inner.height,
        lvl,
        LoopPreviewState {
            entry: loop_entry.as_ref(),
            report: report.as_ref(),
            ledger: ledger.as_ref(),
            checkpoint: checkpoint.as_ref(),
        },
    );
    let stops = preview_stop_block(v, inner, lvl);

    // ---- split: head and stops are fixed; the transcript takes the rest --
    // `Min(0)` on the transcript row means the fixed head/stops rows win when the pane is tiny (and
    // ratatui shrinks them rather than panicking when even they don't fit). ORDER (top→bottom):
    // head (posture + the `report` section) · the transcript (`Min(0)`, "the point of the pane") ·
    // stops (the action bar, when present, last of all). pmd's decisions used to sit as a feed
    // between head and transcript; that moved to the `v` decision lane, so the preview is now just
    // the agent's own output.
    let [head_a, log_a, stops_a] = Layout::vertical([
        Constraint::Length(u16::try_from(head.len()).unwrap_or(u16::MAX)),
        Constraint::Min(0),
        Constraint::Length(u16::try_from(stops.len()).unwrap_or(u16::MAX)),
    ])
    .areas(inner);

    // Head is line-based (no wrap): one logical line == one row, so a long metadata
    // tail clips at the right edge instead of shoving the transcript down.
    f.render_widget(Paragraph::new(Text::from(head)), head_a);

    // ---- log: the point of the pane ----------------------------------------
    let tail_h = usize::from(log_a.height.saturating_sub(1)); // minus the section rule
    let (log, detail_max) = preview_log_lines(
        &log_src,
        loop_entry.as_ref(),
        inner_w,
        tail_h,
        app.detail_scroll,
    );
    // Recorded for `handle_key` to clamp against, the same arrangement `render_wake_view` uses.
    app.detail_max.set(detail_max);
    // Deliberately NO wrap: one transcript line == one screen row keeps the tail
    // budget above exact (same reason as the full-screen wake view).
    f.render_widget(Paragraph::new(Text::from(log)), log_a);

    if !stops.is_empty() {
        // NO WRAP, and this is the fix for how bad this block LOOKED.
        //
        // It used to carry `Wrap { trim: true }`, which did two things to a body whose rows
        // were ALREADY fitted by `stop_preview_lines`:
        //
        //  * `trim: true` strips leading whitespace, so the two-space indent under each
        //    header was silently deleted — every row landed flush against the border and the
        //    block read as an undifferentiated wall of text with no hierarchy at all. That is
        //    the thing a human sees and calls ugly, and no amount of restructuring the rows
        //    upstream could have fixed it, because the indent never reached the screen;
        //  * wrapping re-broke rows that were already truncated to the pane, so the block's
        //    RESERVED height (`stops.len()`, taken out of the log's) stopped being exact.
        //
        // Same rule as the log immediately above and the wake view: exactly ONE thing may
        // decide where a line breaks. Here that is `stop_preview_lines`.
        f.render_widget(Paragraph::new(Text::from(stops)), stops_a);
    }
}
