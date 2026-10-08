//! The PREVIEW's `Log` section: WHICH of a session's several possible transcripts it is
//! showing, and the tail of that transcript which fits. The source decision is the
//! interesting half — a session can be mid-wake, parked, or driven by a chat REPL, and
//! showing the wrong one reads as "it is doing nothing".

use crate::*;

/// Scrollback rows to keep captured BEYOND the current scroll position, as a LOOKAHEAD, so the
/// transcript always has more history to scroll into than the human has reached.
///
/// The depth used to be the pane's own height, which made scrolling stop about one screen back: user,
/// *"[the mouse scroll] … not scroll all the ways"*. That was never a scroll bug — `pane_window`
/// clamps against the lines it was GIVEN, and `capture-pane -S -20` gives twenty. The window can only
/// reach as far as the capture does, so the fix belongs at the read.
///
/// It is a LOOKAHEAD, not a cap: `render_preview` captures `region + detail_scroll + PREVIEW_SCROLLBACK`,
/// so the depth FOLLOWS the scroll and `detail_max` stays this many rows ahead of where the human is —
/// each page up reveals another page until tmux's own history runs out (the true top). A fixed depth
/// was a hard floor: user, *"user should be able to scroll all the way to the top"*.
///
/// 600 rows keeps that headroom generous while staying cheap: following the tail costs `region + 600`,
/// and the deep captures are paid only while actually scrolled up. The cost of a capture is dominated
/// by forking `tmux` (one exec either way), not the kilobytes it prints.
pub(crate) const PREVIEW_SCROLLBACK: usize = 600;

/// Where the PREVIEW's `Log` section got its text this frame. Resolved ONCE per
/// render so the pane title, the log body and the placeholder can never disagree
/// about what the human is looking at.
///
/// `Clone` so a capture can be cached across frames ([`App::preview_capture`]) — the autopilot
/// rainbow animates at ~8fps, and re-forking `tmux capture-pane` every frame would be the fork
/// storm the run loop already guards against.
#[derive(Clone)]
pub(crate) enum PreviewLog {
    /// The LIVE tmux pane of this session's ONE persistent interactive agent (the
    /// `pmloop-…` session `pmd` launches and nudges). For an agent-loop session
    /// this is the ONLY place the agent's output exists: the persistent model
    /// writes nothing to `steps/<seq>.log`, which is why a `steps/`-only Log
    /// section read `—  no wakes yet` forever while the agent was talking.
    Pane(String),
    /// The human's own `pmchat-…` REPL pane.
    ///
    /// The claude a human is TALKING TO is a claude, and it was rendered nowhere. User, looking at
    /// a Standard session with an open chat: *"i don't see we render the claude on the main
    /// session"* — the Log said `agent not running — m switches autopilot`, which was true about
    /// the `pmloop-` agent pmd would drive and utterly wrong about the screen in front of them.
    ///
    /// Ranked BELOW [`PreviewLog::Pane`] on purpose: when both exist, the `pmloop-` agent is the
    /// one the dashboard is about (and a live chat means pmd has parked it, so its pane is the last
    /// thing it did rather than something moving). This is the fallback that makes the common
    /// no-loop-session case honest.
    Chat(String),
    /// The legacy per-wake stream-json transcript at `steps/<seq>.log`, written by
    /// the OLD ephemeral worker (`claude -p … --output-format stream-json`) — still
    /// the truth for `Mode::Auto`/pre-persistent sessions, so it stays as the
    /// fallback and is rendered via `stream_json::render_transcript` exactly as before.
    Step(u64),
    /// A JOB's own `job.log`, rendered rather than captured.
    ///
    /// A job runs `claude -p --output-format stream-json` / `codex exec --json`, so its PANE is a wall
    /// of machine-readable events — user, looking at one: *"it use the text in raw transcript format and
    /// it is hard to view"*. The wrapper tees that stream to `job.log`, so the file is the same content
    /// the pane shows and [`agent_manager::stream_json::render_transcript`] turns it into the prose, tool
    /// lines and ending a human wants. Ranked ABOVE the live pane for a job, because the pane is the
    /// unreadable copy of this.
    Job,
    /// Nothing to show, plus the HONEST reason. Never says "no wakes yet" when the
    /// real situation is "the agent session isn't up".
    Empty(&'static str),
}

/// Pick the PREVIEW `Log` source for one agent-loop session, in priority order:
/// (a) the live pane of its persistent agent, (b) a legacy per-wake step log,
/// (c) an honest placeholder.
///
/// READ-ONLY and non-fatal by construction. The only tmux calls are `is_alive`
/// (a `has-session` probe) and `capture_tail` (`capture-pane -p`) — pmtui never
/// sends keys, never kills the session, and never writes the ledger. Every error
/// degrades to the NEXT source rather than propagating: an unknown liveness probe
/// reads as "not alive", and a failed capture falls through to the step log and
/// finally to the placeholder. So a missing/wedged tmux can neither blank the
/// dashboard nor panic it.
///
/// `want` is the capture DEPTH in rows — the Log region's height plus
/// [`PREVIEW_SCROLLBACK`] of history, since the window the pane opens can never
/// reach further back than this read does. tmux returns that much scrollback PLUS
/// the visible pane, so the caller still takes the window that actually fits
/// (see [`pane_window`]).
pub(crate) fn preview_log_source(
    driver: &dyn Driver,
    session: &str,
    chat_session: &str,
    paths: &ProjectPaths,
    want: usize,
    driven: bool,
    job: bool,
) -> PreviewLog {
    // (a0) A JOB, before any pane capture: its pane holds the raw event stream, and the tee'd `job.log`
    //      holds the same bytes in a file this can render. Checked first for exactly that reason — the
    //      pane is not a better source here, it is the unreadable one.
    if job {
        let raw = std::fs::read_to_string(paths.job_log()).unwrap_or_default();
        if !agent_manager::stream_json::render_transcript(&raw).is_empty() {
            return PreviewLog::Job;
        }
    }
    // (a) the persistent agent's live pane — the current model's only transcript. Taken
    //     with `capture_tail_styled` (`capture-pane -e`) so the agent's OWN colours
    //     survive into the Log section; the escape-free `capture_tail` stays reserved for
    //     the pane classifiers, which match on literal text.
    let alive = driver.is_alive(session).unwrap_or(false);
    if alive {
        // `capture_tail_styled` already maps a non-zero tmux exit to an empty capture; an
        // `Err` means tmux itself could not be run. Both fall through.
        let text = driver
            .capture_tail_styled(session, want.max(1))
            .unwrap_or_default();
        // Emptiness is judged on the PARSED text, never `text.trim()`: escape bytes are
        // not whitespace, so a pane holding nothing but its colour state would look
        // non-empty to `trim` and silently change which SOURCE this function picks —
        // claiming pane output where the screen is blank, and hiding the honest
        // "up but quiet" placeholder (and any legacy step log) behind it.
        if agent_manager::ansi::has_visible_text(&text) {
            return PreviewLog::Pane(text);
        }
    }
    // (a2) THE CHAT'S OWN PANE. On a Standard session there is no `pmloop-` at all, so before
    //      this the human's live REPL — the claude they are typing into — rendered nowhere and the
    //      Log claimed nothing was running. Same `capture_tail_styled` as (a), for the same reason.
    let chat_alive = driver.is_alive(chat_session).unwrap_or(false);
    if chat_alive {
        let text = driver
            .capture_tail_styled(chat_session, want.max(1))
            .unwrap_or_default();
        if agent_manager::ansi::has_visible_text(&text) {
            return PreviewLog::Chat(text);
        }
    }
    // (b) a legacy per-wake transcript, so `Mode::Auto`/pre-persistent sessions
    //     (and any agent-loop session with older wakes on disk) keep rendering.
    if let Some(seq) = latest_step_seq(&paths.steps_dir()) {
        return PreviewLog::Step(seq);
    }
    // (c) honest and specific: "up but quiet" is a different situation from
    //     "nothing is running", and only the second one is the human's to fix.
    PreviewLog::Empty(if job && alive {
        // A job has no one to nudge and no autopilot dial, so neither of the messages below fits it.
        "job is running — nothing reported yet"
    } else if job {
        "the job is over — its result is on its parent's receipt"
    } else if alive {
        "agent is running — nothing on its pane yet"
    } else if chat_alive {
        // A blank chat pane is its own case: the REPL is up, so telling the human to press `m`
        // would point them at the wrong thing entirely.
        "chat is open — nothing on its pane yet"
    } else if driven {
        // AUTOPILOT IS ALREADY ON and pmd is between launching the agent and nudging it — the window
        // after `m`, after `r`, and after Enter-on-a-paused-row. Telling this human to "press m"
        // points at the dial they just turned, and reads as "nothing is happening" when in fact the
        // launch is 500ms away. Same class as every other message that outlived its situation.
        "pmd is starting the agent"
    } else {
        // Names the key that is actually BOUND — `BINDINGS` is the current set. This one
        // line has already shipped a lie twice: "A toggles autopilot" after `A` was
        // deleted, then "m switches autopilot" after the dial moved to `m`. It renders on
        // rows whose tier chip reads `[A]`, so naming the chip's letter is the specific
        // trap; the anti-drift test over `BINDINGS` is what now catches it.
        "agent not running — m switches autopilot"
    })
}

const COLLAPSE_BLANK_RUN_AT: usize = 8;

fn flush_blank_run<'a>(output: &mut Vec<Line<'a>>, blanks: &mut Vec<Line<'a>>) {
    if blanks.len() < COLLAPSE_BLANK_RUN_AT {
        output.append(blanks);
        return;
    }
    let mut run = std::mem::take(blanks).into_iter();
    if let Some(first) = run.next() {
        output.push(first);
        output.push(Line::styled(
            "...",
            Style::default().add_modifier(Modifier::DIM),
        ));
        if let Some(last) = run.last() {
            output.push(last);
        }
    }
}

fn compact_blank_runs<'a>(lines: Vec<Line<'a>>) -> Vec<Line<'a>> {
    let mut output = Vec::with_capacity(lines.len());
    let mut blanks = Vec::new();
    for line in lines {
        if agent_manager::ansi::line_is_blank(&line) {
            blanks.push(line);
        } else {
            flush_blank_run(&mut output, &mut blanks);
            output.push(line);
        }
    }
    flush_blank_run(&mut output, &mut blanks);
    output
}

/// A WINDOW onto a tmux pane capture, as STYLED lines: the `want` rows ending `back` lines above the
/// tail, plus how many lines remain above the window (the max scroll, for clamping). `back == 0` is
/// the tail itself, which is what the pane shows until the human scrolls.
///
/// "The terminal's default colour" in an agent's output means THE THEME's, here.
///
/// `ansi::styled_lines` is a faithful parser: an agent that emits `SGR 0`/`39`/`49` gets
/// `Color::Reset`, which is the terminal's own pair, and a span that never set a colour gets `None`,
/// which inherits whatever cell it lands on. Both were right when the dashboard drew on the terminal's
/// background. Now that a theme paints the canvas, `Reset` ink is the ONE colour guaranteed not to
/// suit it — white transcript text on a light theme's white pane. So a default foreground becomes the
/// theme's ink and a default background is dropped, which lets the painted canvas show through.
/// Colours the agent actually chose are untouched.
fn theme_default_ink(lines: &mut [Line<'_>]) {
    for line in lines {
        for span in &mut line.spans {
            if span.style.fg.is_none_or(|fg| fg == Color::Reset) {
                span.style.fg = Some(attention::text());
            }
            if span.style.bg == Some(Color::Reset) {
                span.style.bg = None;
            }
        }
    }
}

/// Trailing blank rows are dropped. Oversized interior blank runs are compacted to their boundary
/// rows plus a dim ellipsis: Claude's alternate screen anchors content at the top and chrome at the
/// bottom, so resizing it tall can otherwise put hundreds of empty rows between the answer and the
/// prompt and make the preview look blank. Normal paragraph spacing remains byte-faithful.
///
/// The rows come from [`agent_manager::ansi::styled_lines`], so the agent's own colours and
/// attributes are carried through verbatim. Claude's welcome box, prompt, and status rows remain;
/// only excess empty space between meaningful regions is elided.
///
/// The scroll is what makes the right pane navigable — user: *"that session i want it to be able to
/// navigate or scroll"*. How far it can scroll is set by the compacted capture handed to the window,
/// while capture depth still follows [`PREVIEW_SCROLLBACK`].
pub(crate) fn pane_window(capture: &str, want: usize, back: usize) -> (Vec<Line<'_>>, usize) {
    let mut lines = agent_manager::ansi::styled_lines(capture);
    theme_default_ink(&mut lines);
    while lines.last().is_some_and(agent_manager::ansi::line_is_blank) {
        lines.pop();
    }
    let lines = compact_blank_runs(lines);
    let max = lines.len().saturating_sub(want);
    let start = max.saturating_sub(back.min(max));
    let end = (start + want).min(lines.len());
    (lines[start..end].to_vec(), max)
}

/// The `Log` section's rows: its rule, then the last `tail_h` rows of whichever source
/// [`preview_log_source`] picked. `tail_h` is measured from the region the layout actually
/// granted, so the caller passes it in rather than this deciding how much room it has.
pub(crate) fn preview_log_lines<'a>(
    log_src: &'a PreviewLog,
    loop_entry: Option<&(ProjectPaths, String, String)>,
    inner_w: u16,
    tail_h: usize,
    back: usize,
) -> (Vec<Line<'a>>, usize) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // How many transcript lines sit ABOVE the window — 0 for every source that is not a live pane,
    // because those are short and there is nothing to scroll past.
    let mut max = 0usize;
    let mut log: Vec<Line> = vec![section_line("preview", inner_w)];
    match log_src {
        // ONE arm for both live panes: the rendering is identical (tail-aligned, the pane's own
        // colours preserved) and only the TITLE distinguishes whose output it is. Sharing the arm
        // is deliberate — a second copy of this loop would be the place a future colour or gutter
        // fix lands in one pane and not the other.
        PreviewLog::Pane(text) | PreviewLog::Chat(text) => {
            // A live pane, tail-aligned to the region and carrying its OWN colours (the capture
            // came from `capture-pane -e`) — see `pane_window` for why the pane's own chrome is kept
            // rather than guessed at.
            let (tail, m) = pane_window(text, tail_h, back);
            max = m;
            if tail.is_empty() {
                log.push(Line::styled("  —", dim));
            } else {
                for mut l in tail {
                    // The two-space gutter the other Log sources use, prepended as its
                    // OWN unstyled span so the agent's spans are never restyled or
                    // re-allocated (they still borrow the capture).
                    l.spans.insert(0, Span::raw("  "));
                    log.push(l);
                }
            }
        }
        // ONE arm for both event streams: a legacy wake's `steps/<seq>.log` and a job's `job.log` are
        // the same format read for the same reason, and only the FILE differs. A missing or unreadable
        // log degrades to a dim dash either way.
        PreviewLog::Step(_) | PreviewLog::Job => {
            let raw = loop_entry
                .map(|(p, ..)| {
                    let file = match log_src {
                        PreviewLog::Step(seq) => p.step_log(*seq),
                        _ => p.job_log(),
                    };
                    std::fs::read_to_string(file).unwrap_or_default()
                })
                .unwrap_or_default();
            let rendered = agent_manager::stream_json::render_transcript(&raw);
            if rendered.is_empty() {
                log.push(Line::styled("  —", dim));
            } else {
                let start = rendered.len().saturating_sub(tail_h);
                for l in &rendered[start..] {
                    log.push(Line::raw(format!("  {l}")));
                }
            }
        }
        PreviewLog::Empty(why) => log.push(Line::styled(format!("  —  {why}"), dim)),
    }
    (log, max)
}

/// The highest wake `<seq>` on record under `steps_dir`, scanning for the per-wake
/// artifact names `<n>.log` and `<n>.result.json`. `None` if the directory is
/// missing/unreadable or holds no such file. PURE (filesystem-only, no panics) so
/// the render path can call it cheaply on each draw.
pub(crate) fn latest_step_seq(steps_dir: &Path) -> Option<u64> {
    let mut max: Option<u64> = None;
    for entry in std::fs::read_dir(steps_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let seq = name
            .strip_suffix(".log")
            .or_else(|| name.strip_suffix(".result.json"))
            .and_then(|stem| stem.parse::<u64>().ok());
        if let Some(n) = seq {
            max = Some(max.map_or(n, |m| m.max(n)));
        }
    }
    max
}
