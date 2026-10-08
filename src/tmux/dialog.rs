//! Is the agent blocked on a terminal decision? [`classify_dialog`] recognizes a
//! numbered dialog block and extracts its question, choices, selection mode and
//! current state. This module never answers one; the driver must actively prove a
//! delegable menu reacts to navigation before any submit key is sent.

/// Whether a recognised dialog is a positively identified goal-choice UI or must
/// stay with the human.
///
/// `HumanOnly` is the default. A dialog becomes `DelegableChoice` only when its
/// footer explicitly says Enter selects an option and the current selection marker
/// was captured. Permission, command-approval, trust, and unknown dialog chrome
/// therefore fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDialogClass {
    DelegableChoice,
    HumanOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDialogMode {
    Single,
    Multiple,
}

/// A BLOCKING interactive dialog detected on a pane: the decision the agent is
/// waiting on, the choices it offered, and the minimum metadata required to move
/// its selection safely. Extracted (never answered) by [`classify_dialog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDialog {
    /// The question line verbatim, trimmed (e.g. `Do you want to create hello.txt?`).
    pub question: String,
    /// The numbered choices in the order drawn, stripped of the `❯` selection
    /// marker and the `<n>. ` prefix (e.g. `["Yes", "Yes, allow all edits during
    /// this session (shift+tab)", "No"]`).
    pub options: Vec<String>,
    /// Zero-based option carrying the live `❯`/`›` marker. `None` when the
    /// capture did not contain a marker; such a dialog is always human-only.
    pub selected_index: Option<usize>,
    /// Radio-style single choice or checkbox-style multi selection.
    pub mode: PaneDialogMode,
    /// Checked pane-option indices for [`PaneDialogMode::Multiple`].
    pub checked_indices: Vec<usize>,
    /// Whether the dialog's own chrome positively identifies it as a delegable
    /// choice rather than a permission, trust, or unknown prompt.
    pub class: PaneDialogClass,
    /// Stable hash of the nearby heading and lines between the question and options. This
    /// captures account, command, and similar authority-bearing context without persisting it
    /// in human-facing stops.
    pub context_hash: u64,
}

impl PaneDialog {
    pub fn identity_fingerprint(&self) -> u64 {
        let mut hash = self.context_hash;
        hash = hash_text(hash, &self.question);
        for option in &self.options {
            hash = hash_text(hash, option);
        }
        hash = hash_text(
            hash,
            match self.mode {
                PaneDialogMode::Single => "single",
                PaneDialogMode::Multiple => "multiple",
            },
        );
        hash = hash_text(
            hash,
            match self.class {
                PaneDialogClass::DelegableChoice => "delegable",
                PaneDialogClass::HumanOnly => "human_only",
            },
        );
        for checked in &self.checked_indices {
            hash = hash_text(hash, &checked.to_string());
        }
        hash
    }

    pub fn dashboard_answerable(&self) -> bool {
        self.class == PaneDialogClass::DelegableChoice
            && self.selected_index.is_some()
            && (self.mode == PaneDialogMode::Single || self.checked_indices.is_empty())
            && !self
                .options
                .iter()
                .enumerate()
                .any(|(index, option)| self.options[..index].contains(option))
    }
}

/// Whether a terminal option represents a concrete selection rather than opening a
/// free-text or chat sub-flow. Every automated or dashboard-driven choice path shares
/// this predicate so meta options remain attach-only.
pub fn dialog_option_is_concrete(option: &str) -> bool {
    let lower = option.trim().to_ascii_lowercase();
    ![
        "type something",
        "something else",
        "chat about this",
        "provide another",
        "provide your own",
        "enter another",
        "another response",
        "different answer",
        "enter a response",
        "write a response",
        "free text",
        "custom answer",
        "write your own",
        "specify another",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        && !lower.starts_with("other")
}

/// How many trailing non-empty lines [`classify_dialog`] scans for a dialog block.
///
/// The block itself is small — question + options + footer — but it is the LAST
/// thing drawn (a dialog replaces the input box and its statusline footer), so a
/// short window suffices. Sized at 24 to leave headroom for a long option list
/// and a wrapped footer while staying well inside the 40-line `capture_tail`
/// the caller takes, and deliberately larger than nothing-but-the-block so the
/// question is never cut off from its options.
const DIALOG_TAIL_LINES: usize = 24;

/// A dialog must offer at least this many numbered options before it is reported.
/// Ordinary transcript prose that happens to end in `?` cannot clear this bar, and
/// a real permission dialog always offers at least yes/no.
const DIALOG_MIN_OPTIONS: usize = 2;

/// How many lines of interleaved CONTEXT may sit between a dialog's question and the
/// first of its numbered options.
///
/// claude puts them adjacent, and requiring adjacency is most of what keeps ordinary
/// prose out. codex does not — its approval dialog prints the command it wants to run
/// in between (measured, verbatim shape):
/// ```text
///   Would you like to run the following command?     <- question, ends in `?`
///   Environment: local                               <- 2 lines of context…
///   $ awk "{n++} END {print n}" notes.txt
/// › 1. Yes, proceed (y)                              <- …then the options
/// ```
/// so a strict-adjacency scan found zero options, bailed, and the pane sat `Busy`
/// until the 30-minute stall backstop fired a misleading `Stuck` at the human.
///
/// 2 is what the capture needs; 4 leaves headroom for a long command line that wraps
/// without letting a question reach an unrelated numbered list further down the pane.
/// The `Esc to cancel` footer requirement below the options is still the guard doing
/// the heavy lifting — it is dialog-only chrome, so prose cannot fake the whole shape.
pub(super) const DIALOG_MAX_QUESTION_GAP: usize = 4;

/// Recognise a BLOCKING interactive dialog on a `capture_tail` snapshot and extract
/// what the human has to decide. `None` for every other pane. Pure + panic-free,
/// same shape and placement as [`classify_pane`](super::classify_pane), and deliberately independent of
/// it: a dialog pane classifies `Busy` (there is no bare prompt on screen), so
/// without this the harness re-parks the busy recheck until the 30-minute stall
/// backstop fires a MISLEADING "wedged / no progress" `Stuck`.
///
/// The rules come from the ONE real capture we have — a live
/// `claude --permission-mode default` session that stopped on a `Write` tool call:
/// ```text
///  Do you want to create hello.txt?                            <- question, ends in `?`
///  ❯ 1. Yes                                                    <- selected option
///    2. Yes, allow all edits during this session (shift+tab)
///    3. No
///
///  Esc to cancel · Tab to amend                                <- footer
/// ```
/// so ALL THREE parts are required, in this order, inside the tail window:
///   1. a line ending in `?` that is not itself a numbered option,
///   2. below it — after at most [`DIALOG_MAX_QUESTION_GAP`] lines of interleaved
///      context — [`DIALOG_MIN_OPTIONS`]+ numbered option lines (`<n>. <text>`, the
///      selected one prefixed `❯` by claude or `›` by codex), with deeper-indented
///      continuation lines folded into the option they wrap,
///   3. below the options, a footer offering `Esc to cancel`.
///
/// Keeping the options CLOSE to the question AND requiring the footer below them is
/// what keeps prose out: a transcript paragraph ending in `?` has no numbered block
/// under it, and `Esc to cancel` is a dialog-only hint (the busy transcript hint is
/// `esc to interrupt`, which [`classify_pane`](super::classify_pane) reads instead). No dialog texts are
/// guessed at — every widening above traces to a real codex capture.
///
/// Scanning BACKWARDS returns the LOWEST matching block, i.e. the live dialog
/// rather than an older one still visible further up the transcript.
///
/// ONE PHRASE-ANCHORED EXCEPTION runs afterwards, only on a window these rules have
/// already declined: codex's **startup trust dialog**
/// ([`classify_codex_trust_dialog`]). Two clauses above reject it — its `?` sits
/// mid-line in a wrapped paragraph, and its footer is `Press enter to continue`, not
/// `Esc to cancel` — and neither clause is widened to let it in, because "a mid-line
/// `?` plus a bare press-enter footer" describes a large amount of ordinary prose and
/// a false dialog escalates a session that is working fine. The exception matches that
/// dialog's own literal wording instead, so the rules above stay exactly as narrow as
/// they were.
pub fn classify_dialog(capture: &str) -> Option<PaneDialog> {
    let non_empty: Vec<&str> = capture.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = non_empty.len().saturating_sub(DIALOG_TAIL_LINES);
    let window = non_empty.get(start..)?;
    for q in (0..window.len()).rev() {
        let question = window[q].trim();
        // (1) A question line — and not a numbered option that happens to end in `?`.
        if !question.ends_with('?') || dialog_option_text(window[q]).is_some() {
            continue;
        }
        // (2) The numbered options beneath it, after at most DIALOG_MAX_QUESTION_GAP
        // lines of interleaved context (claude has none; codex prints the command).
        let collected = collect_dialog_options(window, q);
        if collected.options.len() < DIALOG_MIN_OPTIONS {
            continue;
        }
        // (3) The `Esc to cancel` footer, BELOW the option block.
        if let Some((footer_offset, footer)) = window[collected.end..]
            .iter()
            .enumerate()
            .find(|(_, l)| l.to_ascii_lowercase().contains("esc to cancel"))
        {
            let footer = footer.to_ascii_lowercase();
            let footer_is_live_tail = collected.end + footer_offset + 1 == window.len();
            let mode = if collected.checkbox_count == collected.options.len()
                && collected.submit_control
            {
                PaneDialogMode::Multiple
            } else {
                PaneDialogMode::Single
            };
            let structurally_complete = match mode {
                PaneDialogMode::Single => collected.checkbox_count == 0,
                PaneDialogMode::Multiple => true,
            };
            let class = if collected.selected_index.is_some()
                && footer.contains("enter to select")
                && footer_is_live_tail
                && structurally_complete
                && !permission_like(question, &collected.options)
            {
                PaneDialogClass::DelegableChoice
            } else {
                PaneDialogClass::HumanOnly
            };
            return Some(PaneDialog {
                question: question.to_string(),
                options: collected.options,
                selected_index: collected.selected_index,
                mode,
                checked_indices: collected.checked_indices,
                class,
                context_hash: dialog_context_hash(window, q, collected.start),
            });
        }
    }
    classify_codex_trust_dialog(window)
}

/// Content anchors that identify authority-changing prompts even if a future CLI
/// renders them with the same navigation footer as an ordinary question. These are
/// intentionally narrow, measured permission phrases; unknown wording remains
/// human-only unless it also clears the positive choice-chrome test.
fn permission_like(question: &str, options: &[String]) -> bool {
    let question = question.to_ascii_lowercase();
    if question.contains("would you like to run the following command?")
        || question.contains("do you trust the contents of this directory?")
        || question.contains("do you trust the files in this folder?")
        || question.contains("trust this folder?")
    {
        return true;
    }
    options.iter().any(|option| {
        let option = option.to_ascii_lowercase();
        [
            "allow all edits during this session",
            "allow always",
            "allow once",
            "don't ask again",
            "do not ask again",
            "tell claude what to do differently",
            "tell codex what to do differently",
        ]
        .iter()
        .any(|marker| option.contains(marker))
    })
}

/// The numbered option block below the question at `window[q]` → the options in draw
/// order, and the index of the first line AFTER the block (where a caller looks for a
/// footer). Empty options mean there is no block within
/// [`DIALOG_MAX_QUESTION_GAP`] lines.
///
/// Shared by both recognisers in [`classify_dialog`] rather than copied, so the one
/// phrase-anchored exception cannot drift from the general rule it is an exception to.
struct CollectedOptions {
    options: Vec<String>,
    start: usize,
    end: usize,
    selected_index: Option<usize>,
    checked_indices: Vec<usize>,
    checkbox_count: usize,
    submit_control: bool,
}

fn collect_dialog_options(window: &[&str], q: usize) -> CollectedOptions {
    let mut i = q + 1;
    while i < window.len()
        && i < q + 1 + DIALOG_MAX_QUESTION_GAP
        && dialog_option_text(window[i]).is_none()
    {
        i += 1;
    }
    let start = i;
    let mut options: Vec<String> = Vec::new();
    let mut selected_index = None;
    let mut checked_indices = Vec::new();
    let mut checkbox_count = 0usize;
    let mut submit_control = false;
    while i < window.len() {
        if let Some(option) = dialog_option(window[i]) {
            if option.selected {
                selected_index = Some(options.len());
            }
            if let Some(checked) = option.checked {
                checkbox_count += 1;
                if checked {
                    checked_indices.push(options.len());
                }
            }
            options.push(option.text.to_string());
        } else if !options.is_empty()
            && checkbox_count > 0
            && window[i].trim().eq_ignore_ascii_case("submit")
        {
            submit_control = true;
            break;
        } else if !options.is_empty() && !dialog_block_boundary(window[i]) {
            // A WRAPPED option: codex breaks a long choice onto deeper-indented
            // continuation lines. Fold it back into the choice it belongs to —
            // without this the run STOPS at the wrap, so the human is shown a
            // truncated option and every choice below it is silently dropped.
            if let Some(last) = options.last_mut() {
                last.push(' ');
                last.push_str(window[i].trim());
            }
        } else {
            break;
        }
        i += 1;
    }
    CollectedOptions {
        options,
        start,
        end: i,
        selected_index,
        checked_indices,
        checkbox_count,
        submit_control,
    }
}

fn dialog_block_boundary(line: &str) -> bool {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    trimmed.is_empty()
        || lower.contains("esc to cancel")
        || lower.starts_with("press enter to")
        || trimmed
            .chars()
            .all(|c| matches!(c, '─' | '━' | '═' | '-' | ' '))
}

/// codex's startup trust question, VERBATIM (lowercased for a case-insensitive match) —
/// the anchor the one exception in [`classify_dialog`] turns on.
///
/// Live-captured from `codex 0.146.1.355` launched on a directory it had never seen, on
/// a private tmux socket in a scratch tempdir at 80 columns (what a detached
/// `new-session` gets, and this question is 44 characters, so it does not wrap there):
/// ```text
///   Do you trust the contents of this directory? Working with untrusted contents
///   comes with higher risk of prompt injection. Trusting the directory allows
///   project-local config, hooks, and exec policies to load.
///
/// › 1. Yes, continue
///   2. No, quit
///
///   Press enter to continue
/// ```
/// A whole sentence naming directory trust is what makes matching it MID-LINE safe: no
/// structural rule is relaxed to accept it, so no prose can back into a dialog verdict
/// by having a `?` in the wrong place. The cost is that this is the only string in this
/// module matched as CONTENT rather than chrome — codex rewording it silently returns us
/// to the bug, so [`FIXTURE_CODEX_DIALOG_TRUST`] is the tripwire that says so.
pub(super) const CODEX_TRUST_QUESTION: &str = "do you trust the contents of this directory?";
const CODEX_TRUST_FOLDER_QUESTION: &str = "trust this folder?";

/// Recognise codex's startup trust dialog on a window whose general rules
/// ([`classify_dialog`]) already found nothing.
///
/// WHY it needs its own recogniser: the dialog blocks the FIRST launch in any directory
/// codex has not seen, before it will accept any prompt, and none of
/// `--ask-for-approval never`, `--sandbox workspace-write` or
/// `--dangerously-bypass-approvals-and-sandbox` skips it (all three measured). It draws
/// no bare prompt and no busy marker, so [`classify_pane`] reads it `Busy`, the
/// twice-Idle nudge gate never fires, and the session sits until the stall backstop
/// reports a misleading "wedged" `Stuck` half an hour later. That is why a user's first
/// codex session in a new directory looked like autopilot doing nothing at all.
///
/// Two guards, and the ANCHOR is the one doing the work:
///   1. a line CONTAINING [`CODEX_TRUST_QUESTION`] (mid-line is the point — that `?` is
///      mid-paragraph, which is precisely why general clause 1 cannot see it),
///   2. [`DIALOG_MIN_OPTIONS`]+ numbered options below it within
///      [`DIALOG_MAX_QUESTION_GAP`] lines — the same block, collected by the same
///      [`collect_dialog_options`], as the general rule.
///
/// Deliberately NO footer requirement. The general rule needs `Esc to cancel` because
/// its question test ("a line ending in `?`") matches prose on its own; this anchor is
/// dialog-only text already, so a `Press enter to continue` check would buy nothing
/// against prose while adding a second way to MISS the dialog when codex reflows its
/// footer — and missing it is the whole bug.
///
/// Recognising is ALL this does. Nothing here answers the dialog (no code in this module
/// sends keys), and the trust decision is escalated to the human like any other dialog:
/// deciding whether a directory's contents may load project-local config, hooks and exec
/// policies is the definition of a call only a person may make.
fn classify_codex_trust_dialog(window: &[&str]) -> Option<PaneDialog> {
    // Backwards, like the general scan and for the same reason: the LOWEST match is the
    // live dialog, not an older one still visible further up the transcript.
    for q in (0..window.len()).rev() {
        let trimmed = window[q].trim();
        let lower = trimmed.to_ascii_lowercase();
        let Some((at, anchor)) = [CODEX_TRUST_QUESTION, CODEX_TRUST_FOLDER_QUESTION]
            .into_iter()
            .filter_map(|anchor| lower.find(anchor).map(|at| (at, anchor)))
            .min_by_key(|(at, _)| *at)
        else {
            continue;
        };
        let collected = collect_dialog_options(window, q);
        if collected.options.len() < DIALOG_MIN_OPTIONS {
            continue;
        }
        // Report the QUESTION alone, not the wrapped paragraph it leads: the human reads
        // it as a one-line stop. `to_ascii_lowercase` is byte-for-byte length-preserving,
        // so the match offsets are valid char boundaries in the original too; `get`
        // keeps that panic-free regardless of what codex ever paints.
        let end = at + anchor.len();
        let question = trimmed.get(at..end).unwrap_or(trimmed);
        return Some(PaneDialog {
            question: question.to_string(),
            options: collected.options,
            selected_index: collected.selected_index,
            mode: PaneDialogMode::Single,
            checked_indices: Vec::new(),
            class: PaneDialogClass::HumanOnly,
            context_hash: dialog_context_hash(window, q, collected.start),
        });
    }
    None
}

fn dialog_context_hash(window: &[&str], question: usize, options_start: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    if let Some(heading) = question.checked_sub(1).and_then(|index| window.get(index))
        && dialog_option_text(heading).is_none()
        && !dialog_block_boundary(heading)
        && !heading.trim().ends_with('?')
    {
        hash = hash_text(hash, heading.trim());
    }
    window[question + 1..options_start]
        .iter()
        .fold(hash, |hash, line| hash_text(hash, line.trim()))
}

fn hash_text(mut hash: u64, text: &str) -> u64 {
    for byte in text.bytes().chain(std::iter::once(0xff)) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// A numbered dialog option line (`❯ 1. Yes` / `   3. No` / codex's `› 1. Yes,
/// proceed (y)`) → its bare text, with the selection marker, the leading whitespace
/// and the `<n>. ` prefix gone. `None` for anything else — including a plain numbered
/// content line such as the diff row `  1 hi` (no `.` after the number).
pub(super) fn dialog_option_text(line: &str) -> Option<&str> {
    dialog_option(line).map(|option| option.text)
}

struct ParsedDialogOption<'a> {
    text: &'a str,
    selected: bool,
    checked: Option<bool>,
}

/// As [`dialog_option_text`], plus whether this row carries the live selection
/// marker. Kept private so callers cannot accidentally parse a choice without
/// also applying [`classify_dialog`]'s full shape and footer checks.
fn dialog_option(line: &str) -> Option<ParsedDialogOption<'_>> {
    let mut rest = line.trim_start();
    // The currently-selected option carries a marker; the rest are padded. claude
    // draws `❯` U+276F, codex draws `›` U+203A — and without the codex marker its
    // selected choice fell through to the digit test (the marker is not a digit),
    // returned `None`, and took the whole option block with it, so `classify_dialog`
    // never fired on a codex approval dialog at all.
    let selected = rest.starts_with(['❯', '›']);
    if let Some(after_marker) = rest.strip_prefix(['❯', '›']) {
        rest = after_marker.trim_start();
    }
    // `<n>` — at least one ASCII digit, then a literal `.`, then the text.
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let text = rest[digits..].strip_prefix('.')?.trim();
    let (text, checked) = if let Some(text) = text.strip_prefix("[✔]") {
        (text.trim(), Some(true))
    } else if let Some(text) = text.strip_prefix("[ ]") {
        (text.trim(), Some(false))
    } else if let Some(text) = text
        .strip_prefix("[x]")
        .or_else(|| text.strip_prefix("[X]"))
    {
        (text.trim(), Some(true))
    } else {
        (text, None)
    };
    if text.is_empty() {
        None
    } else {
        Some(ParsedDialogOption {
            text,
            selected,
            checked,
        })
    }
}
