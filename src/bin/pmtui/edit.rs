//! The two settings a human edits after a session exists — its GOAL (`brief.md`) and
//! its CADENCE — and the file writes that apply them. One module because both are
//! reached from two surfaces each (an inline field and, for the goal, `$EDITOR`), and
//! routing every write through one function per setting is what keeps the atomic write
//! and the clamp from being duplicated or forgotten.

use crate::*;

/// Whether a session has NO goal recorded at `brief` — the gate on turning autopilot
/// on, since a hands-off loop with no direction is a nudge with nothing to steer by.
///
/// Missing, unreadable and whitespace-only all count as empty ON PURPOSE: that is
/// exactly what the engine sees, because `JobScheduler::nudge` reads the brief with
/// `read_to_string(..).unwrap_or_default()` and cannot tell the three apart either.
/// Judging it any more finely here would let a flip through that the loop then runs
/// goal-less.
pub(crate) fn goal_is_empty(brief: &Path) -> bool {
    std::fs::read_to_string(brief)
        .unwrap_or_default()
        .trim()
        .is_empty()
}

/// Turn the raw contents of the brief editor buffer into the goal text — a PLAIN text
/// editor, so everything the human typed is kept verbatim, `#` lines included. Only
/// surrounding whitespace and blank lines are trimmed (the editor's trailing newline, an
/// accidental blank line or two); the interior body — paragraph breaks, `#` headings,
/// anything — is preserved exactly.
///
/// User: *"when i press Ctrl+e to edit goal, you always mention # comment will be ignored but i
/// don't want that. it should be a simple text editor. the goal should have all the string user
/// wants to put in"*. So this no longer strips `#`-comment lines (it used to, `git commit`-style);
/// an empty/whitespace buffer still means "keep the current goal" via the `is_empty` check in
/// [`crate::edit_brief`]. Pure, so the parse is unit-tested directly without spawning an editor.
pub(crate) fn brief_from_editor_buffer(raw: &str) -> String {
    raw.trim().to_string()
}

/// Seed text for the brief editor buffer: JUST the current goal (with a trailing newline when
/// non-empty), and nothing else. No `#`-comment guidance — this is a plain text editor now (see
/// [`brief_from_editor_buffer`]), so anything seeded here would come back as part of the goal.
/// An empty goal seeds an empty buffer; the field/overlay is where the "empty save keeps it" rule
/// is explained, not the buffer.
pub(crate) fn brief_editor_seed(goal: &str) -> String {
    let goal = goal.trim_end();
    if goal.is_empty() {
        String::new()
    } else {
        format!("{goal}\n")
    }
}

/// The outcome of applying an edited goal to a live session's `brief.md`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GoalEdit {
    /// The brief was replaced. `lines` is the new goal's line count (status detail).
    Written { lines: usize },
    /// The saved text matched what was already on disk (ignoring surrounding
    /// whitespace) — nothing was written, so the file's bytes and mtime are untouched.
    Unchanged,
}

/// Apply an edited goal to a live session's `brief.md`: compare against what is on
/// disk and, only if it actually differs, replace the file ATOMICALLY.
///
/// The atomic write is the load-bearing part. `JobScheduler::nudge` re-reads this
/// exact path on every heartbeat with `read_to_string(..).unwrap_or_default()`, so a
/// truncate-then-fill write races the harness: a nudge landing mid-write reads an
/// empty/partial brief and, because of the `unwrap_or_default()`, silently substitutes
/// the "(No goal is recorded on disk …)" fallback instead of the real mandate.
/// Editing interactively makes that window far more likely, so the write goes through
/// `state::write_text_atomic` (temp in the same directory + rename) — a concurrent
/// reader sees either the whole old brief or the whole new one.
///
/// Comparison ignores surrounding whitespace, so re-saving an untouched buffer (whose
/// trailing newline the editor may have normalized) is correctly reported as
/// `Unchanged` and leaves the file alone. Writes NOTHING but `brief.md`.
///
/// This is the pure, file-only half of the `g` affordance, split out from the
/// editor-driving `edit_brief` (which owns the tty and so cannot be unit-tested) so
/// read → compare → atomic-write is covered directly.
pub(crate) fn apply_goal_edit(brief: &Path, new_goal: &str) -> Result<GoalEdit> {
    let current = std::fs::read_to_string(brief).unwrap_or_default();
    if current.trim() == new_goal.trim() {
        return Ok(GoalEdit::Unchanged);
    }
    state::write_text_atomic(brief, new_goal)
        .with_context(|| format!("write {}", brief.display()))?;
    Ok(GoalEdit::Written {
        lines: new_goal.lines().count(),
    })
}

/// Turn the raw contents of the DIRECTIVE editor buffer into the directive text — the same
/// PLAIN, keep-everything parse [`brief_from_editor_buffer`] applies to a goal (a `#` line is
/// content now, not guidance). A distinct NAME so the two editor surfaces can diverge later
/// without a shared-parser surprise, but ONE implementation today. An empty/whitespace buffer
/// still KEEPS the current directive (via the `is_empty` check in [`crate::edit_directive`]);
/// rescinding is the inline field's deliberate `^X`, never an emptied editor.
pub(crate) fn directive_from_editor_buffer(raw: &str) -> String {
    brief_from_editor_buffer(raw)
}

/// Seed text for the directive editor buffer: JUST the current directive (with a trailing
/// newline when non-empty), and nothing else — a plain text editor like the goal's (see
/// [`directive_from_editor_buffer`]). No `#`-comment guidance: it would round-trip back into the
/// directive. What a directive IS (a RESTRICTIVE-only rule), that an empty save KEEPS it, and
/// that `^X` rescinds are all explained on the inline directive overlay, not in this buffer.
pub(crate) fn directive_editor_seed(directive: &str) -> String {
    brief_editor_seed(directive)
}

/// The outcome of applying an edited directive to a live session's `directive.md` — the
/// directive twin of [`GoalEdit`]. There is deliberately no `Rescinded` arm: rescind is a
/// SEPARATE, explicit action ([`rescind_directive`]), never an outcome of an ordinary save.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirectiveEdit {
    /// `directive.md` was replaced. `lines` is the new directive's line count (status detail).
    Written { lines: usize },
    /// The saved text matched what was already on disk (ignoring surrounding whitespace) —
    /// nothing was written, so the file's bytes and mtime are untouched.
    Unchanged,
}

/// Apply an edited directive to a live session's `directive.md`, mirroring [`apply_goal_edit`]
/// exactly: compare against disk and, only if it actually differs, replace the file ATOMICALLY
/// (temp in the same dir + rename), so the decider's fresh `read_to_string(..).unwrap_or_default()`
/// at consult time can never observe a half-written directive. Writes NOTHING but `directive.md`
/// — never the ledger (pmtui is never a ledger writer).
///
/// An empty `new_directive` is a legitimate write ("" replaces the old text) — but the inline
/// field never calls this with an empty buffer (an empty save KEEPS the current directive), so
/// this is only reached with real content; clearing is [`rescind_directive`]'s job.
pub(crate) fn apply_directive_edit(directive: &Path, new_directive: &str) -> Result<DirectiveEdit> {
    let current = std::fs::read_to_string(directive).unwrap_or_default();
    if current.trim() == new_directive.trim() {
        return Ok(DirectiveEdit::Unchanged);
    }
    state::write_text_atomic(directive, new_directive)
        .with_context(|| format!("write {}", directive.display()))?;
    Ok(DirectiveEdit::Written {
        lines: new_directive.lines().count(),
    })
}

/// Rescind the standing directive: REMOVE `directive.md` so the decider's fresh read yields
/// empty and the DIRECTIVE fence is omitted downstream (`ProjectPaths::directive` — "empty/absent
/// ⇒ no directive"). The deliberate, explicit clear the inline field's `^X` invokes — NOT what an
/// empty save does (that keeps the current directive, the anti-wipe guard the goal shares).
///
/// Returns whether a non-empty directive was actually present, so the caller can say "rescinded"
/// versus "nothing to rescind". An already-absent file is a clean no-op (rescinding twice is not
/// an error). Removes NOTHING but `directive.md`.
pub(crate) fn rescind_directive(directive: &Path) -> Result<bool> {
    let had = std::fs::read_to_string(directive)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    match std::fs::remove_file(directive) {
        Ok(()) => Ok(had),
        // Already gone (or never written) — rescinding is idempotent, so this is success.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(had),
        Err(e) => Err(e).with_context(|| format!("remove {}", directive.display())),
    }
}

/// Parse a human-written cadence into seconds: `600`, `600s`, `10m`, `1h`, `1h30m`,
/// `2h15m30s`. Case- and space-insensitive. `Err` carries a message for the status line.
///
/// A bare number is SECONDS, matching the on-disk field and the create form's own label, so
/// the same digits mean the same thing wherever a human types them.
///
/// Returns the raw total; CLAMPING is the caller's job and happens in exactly one place
/// ([`apply_cadence_edit`]) so the human and agent routes share one bound. Rejecting rather
/// than clamping here would also be wrong for the message: "10x too fast, using the minimum"
/// is more useful than "invalid".
pub(crate) fn parse_cadence(input: &str) -> Result<u64, String> {
    let s: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.to_ascii_lowercase();
    if s.is_empty() {
        return Err("nothing typed".into());
    }
    // A bare number is the common case and the one the create form's label implies.
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let mult = match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return Err(format!("{input:?} — use seconds, or 10m / 1h30m")),
        };
        // A unit with no number in front of it ("mh", "1hm") is a typo, not a zero.
        let n: u64 = digits
            .parse()
            .map_err(|_| format!("{input:?} — {c} needs a number before it"))?;
        digits.clear();
        saw_unit = true;
        total = total.saturating_add(n.saturating_mul(mult));
    }
    if !digits.is_empty() {
        return Err(format!("{input:?} — trailing {digits:?} has no unit"));
    }
    if !saw_unit {
        return Err(format!("{input:?} — use seconds, or 10m / 1h30m"));
    }
    Ok(total)
}

/// What [`apply_cadence_edit`] did, so the caller can report it precisely.
pub(crate) enum CadenceEdit {
    /// The effective cadence already was this — nothing written.
    Unchanged(u64),
    /// Written. `clamped` is true when the request was outside the bounds, so the status can
    /// say the adopted value came from a clamp rather than silently substituting it.
    Written { secs: u64, clamped: bool },
}

/// Clear BOTH things that make pmd wait, so the next sweep drives this session.
///
/// THE BUG THIS CLOSES, reported three times across three milestones: *"I start with a standard, and
/// later i swap to autopilot. The pmd doesn't send any request to my claude/codex"*, *"changing the
/// cadence needs to cancel current check in and start again … i update it to 1m and it doesn't take
/// effect"*, and — after m41 released only the first brake — *"when i switch to autopilot mode, pmd
/// doesn't drive it immediately"*.
///
/// There are TWO independent brakes:
///
/// 1. **The park.** `JobRun::Monitoring { until }` makes `JobScheduler::tick` return early until that
///    instant, and an agent may have asked to nap an hour (`next_check_s`). Re-arming `until` to `at`
///    is what makes pmd LOOK at the row. `at` is `now` for "start driving NOW".
///
/// 2. **The awaiting-report watermark.** pmd's `nudged_at_report_generation` says "I nudged at
///    this accepted report generation and the agent has not reported since", which `job_engine`
///    reads as *the agent is
///    still working* — correctly, since it exists to honour *"i don't want autopilot to kick in and
///    queue a prompt"*. But it re-parks through `busy_recheck`, which only gives up after
///    `DEFAULT_STALL_BUSY_S` (1800s). So a session nudged during an EARLIER autopilot stint, switched
///    to Standard before it answered, then switched back, is held for half an hour by a nudge that
///    cannot be answered any more. Releasing brake 1 alone just made pmd look sooner and re-park.
///
/// So turning autopilot on CLEARS the watermark: pmd was not driving this row, so an outstanding
/// nudge is stale by construction. The protection it provided degrades to what guarded this before it
/// existed — `tmux::classify_pane` plus two consecutive Idle observations — and that is the right
/// trade here, because the human has just explicitly asked for a row nobody was driving to be driven.
/// A CADENCE change deliberately does NOT do this (see [`apply_cadence_edit`], which re-times its own
/// park): a new interval is not permission to interrupt work already in flight.
///
/// Only a `Monitoring` RUN is re-armed — `Blocked` is a session waiting on a human and must keep
/// waiting, `Running` has a wake in flight, `Idle` is already due — but the watermark is cleared
/// whatever the run state, because none of those states make a stale nudge answerable.
///
/// pmtui requests the wake through `control.json`; pmd remains the sole ledger writer.
pub(crate) fn start_driving_now(paths: &ProjectPaths) -> Result<()> {
    let mut control = state::read_control(paths)
        .with_context(|| format!("read {}", paths.control().display()))?;
    control.wake_generation = control.wake_generation.saturating_add(1);
    state::write_control(paths, &control)
        .with_context(|| format!("write {}", paths.control().display()))
}

/// Write a new cadence request for one agent-loop session: `control.json` first,
/// then the registry's creation/display seed.
///
/// pmd applies the control request to `state.json`; pmtui never writes that ledger.
/// A failure after the control write leaves only the registry seed stale, which is
/// the harmless direction.
///
/// Clamped in ONE place, to the same bounds `JobScheduler::adopt_cadence` uses, so a human
/// and an agent cannot end up with different ideas of a legal cadence.
pub(crate) fn apply_cadence_edit(
    paths: &ProjectPaths,
    reg_path: &Path,
    id: &str,
    want: u64,
) -> Result<CadenceEdit> {
    let secs = want.clamp(job_engine::CADENCE_MIN_S, job_engine::CADENCE_MAX_S);
    let clamped = secs != want;
    let mut control = state::read_control(paths)
        .with_context(|| format!("read {}", paths.control().display()))?;
    if control.human_cadence_s == Some(secs) {
        return Ok(CadenceEdit::Unchanged(secs));
    }
    control.human_cadence_s = Some(secs);
    state::write_control(paths, &control)
        .with_context(|| format!("write {}", paths.control().display()))?;
    // Best-effort, and deliberately NOT fatal: the dial has already moved where it counts.
    // A registry that briefly disagrees about the seed is a cosmetic drift; refusing the
    // whole edit because of it would strand the human at the old cadence.
    if let Ok(mut reg) = Registry::load(reg_path)
        && let Some(e) = reg.projects.iter_mut().find(|p| p.id == id)
    {
        e.cadence_s = Some(secs);
        let _ = reg.save(reg_path);
    }
    Ok(CadenceEdit::Written { secs, clamped })
}
