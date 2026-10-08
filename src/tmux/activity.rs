//! Is the agent mid-response, or waiting at its prompt? The pure, panic-free
//! heuristics that read a `capture_tail` snapshot: [`classify_pane`], the spinner
//! and prompt predicates it is built from, and [`has_composer`]. They are one unit
//! because a false Idle is the dangerous direction — it types a nudge into a
//! working agent — so no predicate here can be changed without the others in view.

use super::dialog::dialog_option_text;

/// Whether an interactive claude pane is mid-response or idle at its prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneActivity {
    Busy,
    Idle,
}

/// How many trailing non-empty lines are scanned for activity signals.
///
/// Sized from the MEASURED real layout (Claude Code v2.1.x): the input box is
/// *not* the bottom of the pane. Counting up from the last non-empty line there
/// are 4 statusline footer lines, a separator, the prompt line, a separator, and
/// the spinner/summary line — 8 — and the transcript lines above those still
/// carry the `esc to interrupt` hint older builds print. 16 spans all of that
/// with headroom for a wrapped footer without reaching so far back that a stale
/// spinner from a previous turn can veto Idle.
pub(super) const PANE_TAIL_LINES: usize = 16;

/// Classify a `capture_tail` snapshot of an interactive claude pane. CONSERVATIVE: only `Idle` when we are
/// confident the agent is waiting at an empty prompt; anything ambiguous is `Busy` (so we never type a nudge
/// mid-response). Pure + panic-free.
///
/// The real UI renders a MULTI-LINE FOOTER *below* the input box:
/// ```text
/// ✻ Cogitated for 20s                    <- spinner-led, but the turn is DONE (1)
/// ───────────────────────────────────
/// ❯                                      <- the actual prompt (padded U+00A0)
/// ───────────────────────────────────
///   ⚠️ bedrock-claude-opus-5: …          <- 4-line statusline footer
///   https://status.example.com/…
///   Opus 5 (1M context) | Context: 8% …
///   ⏵⏵ auto mode on (shift+tab to cycle) <- LAST non-empty line
/// ```
/// So Idle detection is **position-independent** (the prompt is never last), and
/// Busy detection **takes precedence** — that same bare `❯` is drawn while the
/// agent is generating, so only a busy signal can prevent a false Idle. (1) is
/// the only spinner-led form that means DONE; the two in-progress forms are
/// `✻ Mustering…` (warm-up, no counter yet) and `✻ Quantumizing… (3s · ↓ 5
/// tokens)` (streaming) — see [`is_busy_spinner_line`], where the in-progress
/// ellipsis, not the counter, is the load-bearing discriminator.
///
/// Heuristics applied to the last [`PANE_TAIL_LINES`] non-empty lines, in order:
///   1. Busy if a line contains `esc to interrupt`/`ctrl+c to interrupt` (case-insensitive), OR a Braille
///      spinner glyph (`U+2800..=U+28FF`) — both seen in other/older builds — OR is an in-progress
///      spinner-led line (see [`is_busy_spinner_line`]).
///   2. Else Idle if ANY line in the window is an input prompt awaiting text: a bare claude prompt
///      (`>`/`❯` then whitespace only, [`is_bare_prompt`]) or codex's composer ([`is_codex_composer`],
///      which is NEVER bare — it carries a rotating placeholder).
///   3. Else Busy (conservative default — unknown ⇒ don't nudge).
pub fn classify_pane(capture: &str) -> PaneActivity {
    let non_empty: Vec<&str> = capture.lines().filter(|l| !l.trim().is_empty()).collect();
    // No content at all ⇒ we cannot confirm an idle prompt ⇒ stay conservative.
    if non_empty.is_empty() {
        return PaneActivity::Busy;
    }
    let start = non_empty.len().saturating_sub(PANE_TAIL_LINES);
    let window = &non_empty[start..];
    // (1) Busy FIRST: the prompt is on screen mid-response too, so these are the
    // only signals that separate "generating" from "waiting". Never reorder.
    for &line in window {
        let lower = line.to_ascii_lowercase();
        if lower.contains("esc to interrupt") || lower.contains("ctrl+c to interrupt") {
            return PaneActivity::Busy;
        }
        if line.chars().any(|c| matches!(c, '\u{2800}'..='\u{28FF}')) {
            return PaneActivity::Busy;
        }
        if is_busy_spinner_line(line) {
            return PaneActivity::Busy;
        }
    }
    // (2) Confident-Idle: an input prompt anywhere in the window awaits input —
    // claude's BARE prompt, or codex's composer, which is never bare (see
    // `is_codex_composer`) and so needs its own predicate rather than a widened
    // character class in `is_bare_prompt`.
    // Shares its predicate with [`has_composer`] (which pmtui's `s` key gates on) so the
    // two can never disagree about what counts as an input field.
    if window
        .iter()
        .any(|line| is_bare_prompt(line) || is_codex_composer(line))
    {
        PaneActivity::Idle
    } else {
        PaneActivity::Busy
    }
}

/// The claude spinner's leading glyph — and it is ANIMATED, which is why this is a
/// RANGE and not a list.
///
/// The original list was `*`/`✻`/`·`, inferred from nine live captures that happened to
/// catch only `✻` among the multi-byte forms. Six frames captured later, byte-identical
/// apart from the lead, show the glyph CYCLING: `*` U+002A, `·` U+00B7, `✢` U+2722,
/// `✶` U+2736, `✻` U+273B, `✽` U+273D. Three of those six were NOT in the list, so
/// half of all streaming frames fell through to the bare-prompt test and classified
/// **Idle** — making the harness type a nudge into an agent that was mid-response. That
/// is the dangerous direction, and it is exactly what the user reported wanting stopped:
/// "when the worker is working, we don't need to nudge them."
///
/// Enumerating the observed glyphs would re-create the same bug on the next frame we have
/// not seen, so this accepts the Dingbats decorative-asterisk/sparkle block the animation
/// draws from (U+2722..=U+2743) plus the two single-byte leads. The block deliberately
/// EXCLUDES the glyphs that lead other claude rows — `●` U+25CF (tool header), `⎿` U+23BF
/// (tool result), box drawing U+2500.., arrows U+2190.. — so a widened lead cannot pin an
/// ordinary transcript row at Busy. The busy MARKER test below is unchanged and still
/// does the real work.
pub(super) fn is_spinner_glyph(first: char) -> bool {
    matches!(first, '*' | '·') || ('\u{2722}'..='\u{2743}').contains(&first)
}

/// A spinner-LED status line (see [`is_spinner_glyph`]) that reports work IN PROGRESS.
///
/// The real UI has THREE spinner-led forms — measured across nine live captures,
/// six of them taken at 2s intervals through a single turn — and only the last
/// one means the agent is done:
///   1. `✻ Mustering…`                      warm-up: ellipsis, no counter ⇒ BUSY
///   2. `✻ Quantumizing… (3s · ↓ 5 tokens)` streaming: + counter          ⇒ BUSY
///   3. `✻ Cogitated for 20s`               FINISHED summary              ⇒ not busy
///
/// The load-bearing marker is therefore the IN-PROGRESS ELLIPSIS (`…`/`...`), not
/// the counter: the counter only appears once tokens start streaming, so keying
/// busy on `↓`/`↑`/`tokens` alone reads the first seconds of EVERY turn as Idle —
/// a false Idle, which is the dangerous direction (it types a nudge into an agent
/// that is mid-response). Form 3 has neither an ellipsis nor a counter, so a
/// genuinely idle pane still classifies Idle.
///
/// The spinner lead is what keeps this narrow: ordinary ellipsis-bearing footer
/// and transcript text (`TT: https://t.cor…`, `… +1 line (ctrl+o to expand)`,
/// `● Reading 1 file…`) is not spinner-led, so it cannot pin the pane at Busy.
pub(super) fn is_busy_spinner_line(line: &str) -> bool {
    let line = line.trim_start();
    let spinner_led = line.chars().next().is_some_and(is_spinner_glyph);
    spinner_led
        && (line.contains('…')
            || line.contains("...")
            || line.contains('↓')
            || line.contains('↑')
            || line.to_ascii_lowercase().contains("tokens"))
}

/// Is a COMPOSER on screen — i.e. is there anything in this pane that would accept a
/// typed message?
///
/// Exactly clause (2) of [`classify_pane`], factored out so a caller that needs to know
/// "would `send_keys` land in a text field?" cannot drift from the classifier. Used by
/// pmtui's `s` key, whose trailing `Enter` must never be delivered to a select widget:
/// with a numbered dialog on screen there is no composer, the characters go to the
/// widget's keyboard accelerators, and the Enter CONFIRMS whatever option was
/// pre-highlighted (see [`classify_dialog`](super::classify_dialog), and `job_engine`'s `park_dialog`, which
/// exists because "typing a choice into a live agent is the riskiest action in the
/// system").
///
/// Deliberately independent of Busy/Idle: claude and codex both keep their composer on
/// screen while generating, so a mid-response pane still has one.
pub fn has_composer(capture: &str) -> bool {
    let non_empty: Vec<&str> = capture.lines().filter(|l| !l.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return false;
    }
    let start = non_empty.len().saturating_sub(PANE_TAIL_LINES);
    non_empty[start..]
        .iter()
        .any(|l| is_bare_prompt(l) || is_codex_composer(l))
}

/// A content fingerprint of the TRANSCRIPT REGION of an Idle-classified capture — the
/// lines ABOVE the input prompt — for the heartbeat's confirmation gate to tell a pane
/// that is genuinely WAITING from one that only LOOKS idle because its busy marker is
/// off-screen.
///
/// WHY IT EXISTS. [`classify_pane`] is a single-snapshot heuristic, and Claude Code
/// v2.1.x has no per-line busy marker while it STREAMS an answer: the spinner line
/// disappears once tokens render, `esc to interrupt` is absent from the build entirely
/// (measured across ~40 captures), and the response is a growing `●`-led block (`●`
/// U+25CF is deliberately NOT a spinner glyph — see [`is_spinner_glyph`]) above the same
/// bare `❯` a waiting pane shows. So `classify_pane` returns Idle for an actively-working
/// agent, and (measured) does so for 13+ consecutive frames — enough to defeat the
/// two-observation gate on its own. But a working pane's transcript GROWS between two
/// captures while a waiting pane's is byte-stable, so the gate compares this fingerprint
/// across its two observations and only nudges when they match: a changing transcript
/// re-arms instead of typing into a mid-response agent, independent of the spinner/glyph
/// vocabulary (the least stable thing in the UI).
///
/// EXCLUDES THE FOOTER, and that exclusion is load-bearing. The real UI draws a
/// statusline BELOW the prompt (`Opus 4.8 (1M context) | Context: 0% | Session: 11h 05m`)
/// whose counters tick every second even while the agent waits; hashing them would mean
/// two captures a recheck apart are NEVER byte-stable and a genuinely idle session would
/// never be nudged. So the fingerprint stops at the LAST input-prompt line
/// ([`is_bare_prompt`] / [`is_codex_composer`]): everything above is transcript, and the
/// prompt line and everything below it (the volatile footer, and codex's own rotating
/// composer placeholder) are excluded. With no prompt line at all (not an Idle capture —
/// the gate only calls this on one) it hashes the whole capture, which still changes when
/// the content does. Trailing whitespace is trimmed per line because tmux pads/strips row
/// tails inconsistently; leading indentation is kept (it is stable on a waiting pane).
pub fn idle_fingerprint(capture: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let lines: Vec<&str> = capture.lines().collect();
    let boundary = lines
        .iter()
        .rposition(|l| is_bare_prompt(l) || is_codex_composer(l))
        .unwrap_or(lines.len());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in &lines[..boundary] {
        line.trim_end().hash(&mut hasher);
    }
    hasher.finish()
}

/// Stable fingerprint of meaningful pane transcript progress while a worker is Busy.
///
/// The composer and footer are excluded exactly as in [`idle_fingerprint`]. Recognized live
/// spinner/interrupt rows are also excluded because their elapsed seconds, token counters, and
/// animated glyphs change while the underlying work may be completely stuck. New command output,
/// tool results, or response text remains in the hash and therefore restarts the inactivity window.
pub fn progress_fingerprint(capture: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let lines: Vec<&str> = capture.lines().collect();
    let boundary = lines
        .iter()
        .rposition(|line| is_bare_prompt(line) || is_codex_composer(line))
        .unwrap_or(lines.len());
    let mut hash = FNV_OFFSET;
    for line in &lines[..boundary] {
        let line = line.trim_end();
        if line.trim().is_empty() || is_volatile_busy_line(line) {
            continue;
        }
        for byte in line
            .as_bytes()
            .iter()
            .copied()
            .chain(std::iter::once(b'\n'))
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

fn is_volatile_busy_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("esc to interrupt")
        || lower.contains("ctrl+c to interrupt")
        || line.chars().any(|c| matches!(c, '\u{2800}'..='\u{28FF}'))
        || is_busy_spinner_line(line)
}

/// A bare claude prompt: `>` or `❯` followed only by whitespace. The real UI pads
/// it with U+00A0, which `char::is_whitespace` already accepts (as does `trim`).
pub(super) fn is_bare_prompt(line: &str) -> bool {
    let mut chars = line.trim().chars();
    matches!(chars.next(), Some('>') | Some('❯')) && chars.all(char::is_whitespace)
}

/// codex's COMPOSER line — the analogue of claude's bare prompt, and the reason
/// [`is_bare_prompt`] can never detect an idle codex on its own.
///
/// codex leads its composer with `›` U+203A (not claude's `❯` U+276F) and, unlike
/// claude, NEVER draws it bare: it always carries a rotating PLACEHOLDER hint. That
/// was measured, not assumed — two live runs painted `› Summarize recent commits`
/// and `› Write tests for @filename` while the prompts actually submitted were
/// something else entirely, and the same placeholder is drawn on a fresh pane,
/// mid-work, and after the turn finishes. So the text is a hint, never input, and
/// no bare-prompt test can ever fire on it.
///
/// That is the whole of the second bug: with only `>`/`❯` accepted, EVERY codex
/// state classified `Busy` (measured: idle-at-composer, mid-work, approval dialog,
/// free-text question and trust dialog all came back `Busy`), so the twice-Idle
/// nudge gate could never fire, no codex session was ever nudged, and every one
/// ended in a misleading 30-minute `Stuck`. codex autopilot had never worked.
///
/// Deliberately NARROW, because a false Idle is the dangerous direction:
///   - A NUMBERED line is excluded. codex marks the selected dialog choice with the
///     very same `›` (`› 1. Yes, proceed (y)`), so without this exclusion both the
///     approval dialog and the startup trust dialog would read Idle and the harness
///     would type free text into a pane waiting on a numbered choice. Excluding them
///     keeps both at `Busy`, which is what routes them to [`classify_dialog`].
///   - Busy detection still runs FIRST and still wins. The composer is drawn while
///     codex is working too (measured), exactly like claude's prompt mid-response, so
///     only a busy marker separates "generating" from "waiting" — and codex paints
///     `• Working (0s • esc to interrupt)` from the first frame of a turn, which
///     rule (1) of [`classify_pane`] already catches.
///   - It cannot loosen the claude path: `›` is not a lead claude draws.
///
/// NOT a "the composer is the only `›` line" test, because it is not: codex echoes
/// the submitted user message with a `›` lead too (its wrapped continuation lines are
/// indented and unmarked). Multiple echoes accumulate over a conversation, so counting
/// `›` lines would break on the second turn. Presence is the signal; ordering is not.
pub(super) fn is_codex_composer(line: &str) -> bool {
    let trimmed = line.trim();
    match trimmed.strip_prefix('›') {
        // `› 1. Yes, proceed (y)` is a dialog CHOICE, not a prompt awaiting text.
        Some(_) => dialog_option_text(trimmed).is_none(),
        None => false,
    }
}
