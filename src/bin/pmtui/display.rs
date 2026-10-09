//! How a value becomes the text, glyph or colour on screen: the tier chips,
//! the status category a row sorts and colours by, the
//! age label, the width-aware string helpers (`text_cols`, `truncate`) every pane fits
//! its content with, and `input_line` (the shared one-row text field, caret and all).
//! The string helpers are pure, so what the screen says is asserted without drawing.

use crate::*;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// First meaningful line of human intent, normalized for one-line Task titles.
pub(crate) fn intent_title(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(advise::sanitize_control_bytes)
        .filter(|line| !line.is_empty())
}

/// The dashboard status shown when Enter finds no live wake to watch: explain
/// that the loop wakes on its cadence and point at the actions that apply.
/// Includes the cadence seconds when the registry entry records one.
pub(crate) fn no_wake_status(id: &str, cadence_s: Option<u64>) -> String {
    match cadence_s {
        Some(s) => format!(
            "{id}: no wake running right now — it runs every {s}s; Enter to watch when one is live · s answer · d close"
        ),
        None => format!(
            "{id}: no wake running right now — Enter to watch when one is live · s answer · d close"
        ),
    }
}

/// The SESSIONS row's one-letter tier chip — the scannable form, kept short because it
/// sits inside a fixed-width row. The preview spells it out (see [`autopilot_state`]).
pub(crate) fn tier_tag(t: Option<Tier>) -> &'static str {
    match t {
        Some(Tier::Autopilot) => "A",
        Some(Tier::Standard) => "S",
        None => "?",
    }
}

pub(crate) fn risk_str(rc: RiskClass) -> &'static str {
    match rc {
        RiskClass::Low => "low",
        RiskClass::Medium => "medium",
        RiskClass::Hard => "hard",
    }
}

/// The primary activity label shown in both the session row and selected preview.
/// A detached decider consult is active Autopilot work even while the worker itself is
/// parked in `Monitoring`, so name that work instead of exposing the implementation park.
pub(crate) fn primary_status_label(v: &ProjectView) -> &'static str {
    if v.enabled
        && v.tier == Some(Tier::Autopilot)
        && !v.posture.needs_attention()
        && v.decider_live
    {
        "reviewing"
    } else if v.enabled
        && v.tier == Some(Tier::Autopilot)
        && !v.posture.needs_attention()
        && v.advice_inflight.is_some()
    {
        "review pending"
    } else {
        v.posture.label()
    }
}

/// Compact, observable decider activity for the primary view. This contains only durable
/// harness facts: consult id, elapsed wall time, and sibling decisions still queued.
pub(crate) fn current_decider_activity(v: &ProjectView, now: Epoch) -> Option<String> {
    let Some(inflight) = v.advice_inflight.as_ref() else {
        let queued = v.advice_queue.len();
        return (queued > 0).then(|| {
            let noun = if queued == 1 { "decision" } else { "decisions" };
            format!("{queued} {noun} queued")
        });
    };
    let queued = v
        .advice_queue
        .iter()
        .filter(|queued| !inflight.stop_ids.contains(&queued.stop_id))
        .count();
    let elapsed = v
        .decider_runs
        .iter()
        .rev()
        .find(|run| run.seq == inflight.seq)
        .map(|run| format_duration_hms(now.saturating_sub(run.started_at).max(0)));
    let timing = elapsed
        .map(|elapsed| format!("{elapsed} elapsed"))
        .unwrap_or_else(|| "in progress".to_string());
    let activity = if v.decider_live {
        "reviewing"
    } else {
        "pending"
    };
    Some(format!(
        "decision #{} · {activity} {timing} · {queued} queued",
        inflight.seq
    ))
}

fn format_duration_hms(total_seconds: i64) -> String {
    let hours = total_seconds / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Which of the FOUR glanceable buckets a project falls into — the SAME partition
/// [`counts`] uses, so the list glyphs and the header counts can never disagree.
/// `0` = needs-you/waiting, `1` = running/live, `2` = idle/done, `3` = stuck.
///
/// `Stuck` is its OWN bucket rather than a red flavour of needs-you, and that is the
/// whole point: it used to be counted as needs-you in the header while the row drew it
/// in a different colour, so the header's `◐ N need-you` silently included rows that
/// were not asking a question at all. One partition, one glyph per bucket, both surfaces.
pub(crate) fn status_category(v: &ProjectView) -> u8 {
    if !v.enabled {
        // A PAUSED row is doing nothing, so it counts as idle and draws the dim `○`. Found by
        // rendering the sectioned list: a session paused mid-`working` kept the green `●` and was
        // still tallied under `● N running` in the header — the row said "paused" beside a glyph
        // that said "live", and the header claimed a process that had been killed.
        //
        // The consequence for the OTHER counter is deliberate too: a paused row holding an open
        // stop no longer adds to `◐ N need you`. That number is a call to action about what is
        // live, and the human is the one who stopped this session. The ROW still says
        // `paused !!` (`attention::level_active` keeps the badge), so the unanswered question is
        // visible where it belongs — on the row, not in a count that would ask them to act.
        return 2;
    }
    if v.posture == Posture::Stuck {
        // BEFORE `needs_attention()`, which is true for Stuck as well as NeedsYou.
        3
    } else if v.posture.needs_attention() {
        0
    } else if v.tier == Some(Tier::Standard) {
        // A STANDARD agent-loop session is HUMAN-driven: pmd never nudges it
        // (`daemon::pmd_drives_row(AgentLoop, Standard) == false`), so it never advances
        // this session's ledger. Its posture is therefore a FROZEN snapshot, stale in BOTH
        // directions — `Fresh`/idle for a never-driven session a human is actively working
        // in, or a leftover `Monitoring`/`Running` after an Autopilot→Standard flip that pmd
        // will never touch again. Bucketing on that posture read a live session as idle and a
        // dead one as running (user: *"when i use standard, the status on the top bar doesn't
        // really reflect correctly"*).
        //
        // The bucket is a claim about ACTIVITY, not mere liveness (user: *"when my claude is idle,
        // the top bar still says running"*): `●` running means the agent is WORKING right now, `○`
        // idle means it is alive but sitting at its prompt (or nothing is up). `refresh` classifies
        // the live conversation pane's CONTENT (the same `classify_pane` + `idle_fingerprint`
        // stability gate pmd uses for a driven row) into `agent_working`:
        //   - dead pane (`!session_live`)                 ⇒ idle `○`;
        //   - working, or a first-tick/unclassified live pane (`agent_working != Some(false)`) ⇒ `●`;
        //   - confirmed idle-at-prompt (`Some(false)`)    ⇒ idle `○`.
        // This supersedes m74/m75's `if session_live { 1 } else { 2 }`, which conflated "a pane
        // exists" with "the agent is working" and so painted an idle claude green. A blocked ask
        // still wins ABOVE via `needs_attention()` (the autopilot-off honesty gate forces
        // `NeedsYou`), so this only decides the working-vs-idle split for a row that isn't asking.
        if !v.session_live || v.agent_working == Some(false) {
            2
        } else {
            1
        }
    } else if v.posture == Posture::Monitoring && v.agent_working == Some(false) {
        // A DRIVEN session that has finished its nudged turn and is idle at its prompt,
        // WAITING for the next nudge (per the live turn-end signal — see
        // `ProjectView::agent_working`). It is genuinely idle RIGHT NOW (`○`), which is what
        // the user asked to see — *"after the claude finish and wait for … update the
        // status"*. It flips back to running (`●`) the moment the next turn starts.
        2
    } else if matches!(
        v.posture,
        Posture::Running | Posture::Working | Posture::Monitoring
    ) {
        // `Monitoring` = an autopilot session pmd is actively DRIVING — the persistent
        // `claude`/`codex` alive and the daemon due to nudge it again. While its agent is
        // mid-turn (`agent_working == Some(true)`), or the turn-end hook isn't wired so we
        // can't tell (`None`), it is LIVE (green `●`), not idle/done (`○`). It used to fall
        // to the `else` idle bucket, and because a persistent agent-loop session is NEVER
        // `Running` (that variant is vestigial ephemeral back-compat), a driven session could
        // never land in the running bucket at all — every autopilot session read as
        // `0 running, 1 idle` while its agent was really working (user-reported).
        1
    } else {
        2
    }
}

pub(crate) fn status_is_working(v: &ProjectView) -> bool {
    status_category(v) == 1
}

/// The agent-deck status vocabulary: a colored glyph per bucket. `●` green =
/// running/live, `◐` yellow = needs-you/waiting, `○` dim gray = idle/done, `✕` red =
/// stuck. Used by both the top counts and the list rows so they read as one system.
///
/// Every bucket has its own SHAPE, not just its own colour. `Stuck` and needs-you both
/// drew `◐` and were distinguished by hue alone, which is no distinction at all for a
/// colour-blind reader or on a terminal theme that flattens red and yellow — and "the
/// agent is wedged" is not the same call to action as "the agent asked you something".
/// The glyph carries the meaning; the colour only reinforces it.
/// The table itself lives in `attention` — one module a reviewer can read to see the
/// whole palette — and this stays as the name both surfaces already call.
pub(crate) fn category_glyph(cat: u8) -> (&'static str, Color) {
    attention::bucket_glyph(cat)
}

/// The glyph+color for one project: purely its [`status_category`] bucket. The `Stuck`
/// recolour that used to live here is a bucket of its own now, so there is no longer a
/// way for a row to render something the header did not count.
pub(crate) fn status_glyph(v: &ProjectView) -> (&'static str, Color) {
    category_glyph(status_category(v))
}

/// `s` cut to at most `n` terminal columns, ending in `…` when anything was dropped. A wide
/// glyph that would straddle the ellipsis is dropped whole, so the result never overflows.
///
/// The cut walks grapheme clusters, each measured as ratatui's buffer draws it: an emoji
/// presentation sequence (`⚠` + U+FE0F) is two cells as a cluster but one as separate chars.
pub(crate) fn truncate(s: &str, n: usize) -> String {
    if text_cols(s) <= n {
        return s.to_string();
    }
    let budget = n.saturating_sub(1);
    let mut used = 0;
    let mut t = String::new();
    for grapheme in s.graphemes(true) {
        let w = text_cols(grapheme);
        if used + w > budget {
            break;
        }
        used += w;
        t.push_str(grapheme);
    }
    format!("{t}…")
}

/// [`truncate`] from the OTHER end: `s` cut to at most `n` terminal columns, beginning with `…`
/// when anything was dropped.
///
/// For a PATH. Two candidate directories share their parent and differ in their last segment, so
/// cutting the tail hides the only part worth reading; cutting the head is what every file dialog
/// and shell prompt does. Walks grapheme clusters from the right, measured as ratatui's buffer
/// draws them, so a wide glyph is never split.
pub(crate) fn truncate_left(s: &str, n: usize) -> String {
    if text_cols(s) <= n {
        return s.to_string();
    }
    let budget = n.saturating_sub(1);
    let mut used = 0;
    let mut kept: Vec<&str> = Vec::new();
    for grapheme in s.graphemes(true).rev() {
        let w = text_cols(grapheme);
        if used + w > budget {
            break;
        }
        used += w;
        kept.push(grapheme);
    }
    let mut out = String::from("\u{2026}");
    out.extend(kept.into_iter().rev());
    out
}

/// [`truncate`] padded with spaces to exactly `n` terminal columns — the fixed-width field a
/// user-chosen label fills. `format!("{:<n}")` pads by chars, which overflows on wide glyphs.
pub(crate) fn pad_cols(s: &str, n: usize) -> String {
    let mut out = truncate(s, n);
    let pad = n.saturating_sub(text_cols(&out));
    out.extend(std::iter::repeat_n(' ', pad));
    out
}

/// The one-row text-input line every overlay draws: a `> ` prompt, the field's text, and a
/// visible caret at the cursor — a reversed cell over a character, or a trailing `_` at the
/// end. Windowed by [`Field::caret_view`] so the caret stays on screen however long the text
/// runs; `cols` is the column budget for the text-and-caret AFTER the two-column prompt.
///
/// Shared by all five text overlays (answer, goal, directive, cadence, send) for the reason
/// the old `caret_tail` was: ONE scroll-and-caret rule for five identical-looking inputs, so a
/// fix or a regression can only ever happen in one place. It replaces `caret_tail`, which only
/// ever pinned the caret to the END — the reason `Left`/`Right`/`Home`/`End` did nothing.
pub(crate) fn input_line(field: &Field, cols: usize) -> Line<'static> {
    let view = field.caret_view(cols);
    let base = Style::default().fg(agent_manager::theme::soft());
    let mut spans = vec![Span::raw("> "), Span::styled(view.left, base)];
    // The caret cell: the character under it, drawn REVERSED so its position is unmistakable,
    // or a trailing `_` when the caret is at the end (the same glyph the field always showed).
    if view.at.is_empty() {
        spans.push(Span::styled("_", base));
    } else {
        spans.push(Span::styled(view.at, base.add_modifier(Modifier::REVERSED)));
    }
    spans.push(Span::styled(view.right, base));
    Line::from(spans)
}

/// The create-form Goal row's single-line display value. Empty → a hint that
/// advertises inline typing and the editor; a one-line goal → the text with an
/// inline cursor (unchanged quick one-liner behavior); a multi-line goal (pasted
/// or editor-composed) → the first non-empty line, truncated, plus a
/// `(+N more lines)` count. Never returns an embedded newline, so a long/multi-line
/// brief can't break the one-line field row.
pub(crate) fn goal_field_display(goal: &str) -> String {
    if goal.is_empty() {
        return "(required → type inline, or Ctrl+E to edit in $EDITOR)".to_string();
    }
    if !goal.contains('\n') {
        // Single line: show it verbatim with the inline text cursor.
        return format!("{goal}_");
    }
    let first = goal
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let extra = goal.lines().count().saturating_sub(1);
    format!("{} (+{extra} more lines)", truncate(first, 48))
}

/// A compact relative age, never wider than 3 columns: `12s`, `5m`, `3h`, `6d`, `9w`,
/// `2y`. The unit ladder (not a `99d+` clamp) is what keeps it both narrow and exact —
/// the row has 3 columns to spend and an under-reported age would be a quiet lie about
/// how stale a session is.
///
/// `None` (nothing has ever run) reads `—`. A FUTURE timestamp — clock skew, or a ledger
/// written by another host — floors at `0s` rather than rendering a negative age.
pub(crate) fn age_label(last: Option<Epoch>, now: Epoch) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const YEAR: i64 = 365 * DAY;
    let Some(t) = last else {
        return "—".to_string();
    };
    let d = (now - t).max(0);
    if d < MIN {
        format!("{d}s")
    } else if d < HOUR {
        format!("{}m", d / MIN)
    } else if d < DAY {
        format!("{}h", d / HOUR)
    } else if d < WEEK {
        format!("{}d", d / DAY)
    } else if d < YEAR {
        format!("{}w", d / WEEK)
    } else {
        // A century-old timestamp is a corrupt one; 99y is as much as the column can say.
        format!("{}y", (d / YEAR).min(99))
    }
}

/// The machine's local UTC offset in SECONDS, determined ONCE per process and cached.
///
/// Dependency-free on purpose (this crate carries no time/tz crate — see `Cargo.toml`):
/// `date +%z` is the one thing that already knows the local zone, and shelling out is the
/// codebase's idiom over taking a dependency. Read once (the `OnceLock`), so it costs one
/// `date` spawn on the first render that needs it and nothing thereafter; a DST change
/// mid-session is therefore not tracked, which is acceptable for a glanceable timestamp.
/// Falls back to UTC (`0`) if `date` is unavailable or its output is unparseable.
fn local_utc_offset_secs() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| parse_utc_offset(s.trim()))
            .unwrap_or(0)
    })
}

/// Parse a `date +%z` offset (`"+HHMM"` / `"-HHMM"`, e.g. `"-0700"`) into seconds. `None` on
/// any shape it does not recognise, so the caller can fall back to UTC.
fn parse_utc_offset(z: &str) -> Option<i64> {
    let sign = match z.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hh: i64 = z.get(1..3)?.parse().ok()?;
    let mm: i64 = z.get(3..5)?.parse().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

/// Format an [`Epoch`] as a FIXED local `HH:MM:SS` wall-clock time. Unlike [`age_label`],
/// this does not change between renders — it is WHEN the event happened, not how long ago,
/// so a feed of past decisions reads as a timestamped log rather than a set of counters
/// ticking upward. Pure split ([`fmt_clock`]) so the formatting is unit-tested without the
/// machine's zone.
pub(crate) fn clock_label(at: Epoch) -> String {
    fmt_clock(at, local_utc_offset_secs())
}

/// [`clock_label`] with the offset passed in — the testable core.
pub(crate) fn fmt_clock(at: Epoch, offset_secs: i64) -> String {
    let sod = at.saturating_add(offset_secs).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", sod / 3600, (sod % 3600) / 60, sod % 60)
}

pub(crate) fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Autopilot => "autopilot",
        Tier::Standard => "standard",
    }
}

/// The plain-language descriptor for each Autonomy level — the create form's one dial,
/// and the only place with room to say what each level actually DOES.
///
/// Standard's old wording ("collaborative (asks about important decisions)") described
/// only `policy::decide_kind`'s half of the tier and was misleading about the rest: on a
/// `Mode::AgentLoop` row Standard means pmd does not drive the session at all
/// (`daemon::pmd_drives_row`), so there is nothing running to ask you anything. Say who
/// holds the wheel instead — that is the difference the dial actually makes.
pub(crate) fn autonomy_descriptor(t: Tier) -> &'static str {
    match t {
        Tier::Standard => "you drive it (pmd never types into it)",
        Tier::Autopilot => "hands-off (pmd drives it on its cadence)",
    }
}

/// Display width of `s` in terminal COLUMNS, measured the way ratatui draws it.
///
/// The dashboard's own chrome (ASCII plus `↑ ↓ ← → · /` and the status glyphs) is one
/// column per char, but session display names are user text: a CJK or emoji label takes
/// two cells per glyph, and a char count let it overflow its row. [`keybar_line`] still
/// keeps one column of slack: erring toward dropping a chip early beats overflowing the bar.
pub(crate) fn text_cols(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}
