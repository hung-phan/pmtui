//! Unit tests for the tmux driver, one file per code module. The pane FIXTURES
//! and the `StepHandle` helper live HERE rather than in a sibling because the
//! activity and the dialog tests read the same captures — a fixture only one of
//! them could see would stop being evidence about the real UI.

use std::path::Path;

use super::StepHandle;

mod activity;
mod codex;
mod dialog;
mod dialog_keys;
mod driver;
mod fake;
mod fork_identity;
mod launch;
mod real;
mod send_text;
mod session_names;

fn handle(dir: &Path, session: &str) -> StepHandle {
    StepHandle {
        session: session.to_string(),
        done_signal: dir.join(format!("{session}.done")),
        log: dir.join(format!("{session}.log")),
    }
}

// ---- Real-UI pane fixtures, trimmed to the load-bearing structure ---------
//
// Provenance: `tmux capture-pane` output from a live pmd-driven `pmloop-`
// session on Claude Code v2.1.232, then TRIMMED to only what `classify_pane`
// actually reads:
//   1. a bare prompt line that is NOT the last non-empty line (the whole
//      point of the fix), padded with the real U+00A0,
//   2. the NUMBER of footer lines below that prompt (its text is irrelevant;
//      the count is what `PANE_TAIL_LINES` must span),
//   3. which spinner form sits above the input box, and
//   4. the separators that bracket the input box (they add to the count).
//
// Machine- and account-specific chrome is deliberately GONE: the vendor
// data-handling banner, the bedrock model warning and status link, the model
// name/version, the per-user session timer and the welcome/tips box.
// None of it reproduces on another machine or account, `classify_pane` must
// not depend on any of it, and leaving it in buried a ~4-line signal in ~40
// lines of noise that rotted whenever an unrelated banner changed. The
// statusline keeps its real ROW COUNT (separator + 4 rows) with neutral
// placeholder text.
//
// The prompt's trailing U+00A0 padding IS load-bearing (it proves `trim` /
// `char::is_whitespace` accept it), so it stays an explicit `\u{a0}` escape
// spliced in with `concat!` — never an invisible byte inside a raw string.

/// Live `pmloop-` capture (claude v2.1.232), TRIMMED as described above: a
/// FRESH session, nothing typed yet, so there is no spinner line at all —
/// just the input box and the footer drawn below it.
const FIXTURE_REAL_IDLE: &str = concat!(
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  <statusline row 1>\n",
    "  <statusline row 2>\n",
    "  <statusline row 3>\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
);

/// Live `pmloop-` capture (claude v2.1.232), TRIMMED as described above: a
/// COMPLETED turn — the finished-summary spinner form `✻ Cogitated for 20s`
/// stays on screen above the input box forever.
const FIXTURE_REAL_IDLE_AFTER_TURN: &str = concat!(
    "❯ Tell me a joke.\n",
    "\n",
    "● Why do programmers prefer dark mode?\n",
    "\n",
    "✻ Cogitated for 20s\n",
    "\n",
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  <statusline row 1>\n",
    "  <statusline row 2>\n",
    "  <statusline row 3>\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
);

/// Live `pmloop-` capture (claude v2.1.232), TRIMMED as described above:
/// captured MID-generation — the streaming spinner form (in-progress ellipsis
/// plus a live token counter) above the input box, while the bare prompt is
/// drawn below it just the same.
const FIXTURE_REAL_BUSY: &str = concat!(
    "❯ Tell me a joke.\n",
    "\n",
    "● Reading 1 file… (ctrl+o to expand)\n",
    "\n",
    "✻ Quantumizing… (3s · ↓ 5 tokens)\n",
    "\n",
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  <statusline row 1>\n",
    "  <statusline row 2>\n",
    "  <statusline row 3>\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
);

/// Live `pmloop-` capture (claude v2.1.232), TRIMMED as described above:
/// captured ~2s into a turn — the warm-up spinner form, ellipsis only, before
/// the first token (and therefore the counter) arrived.
const FIXTURE_REAL_BUSY_WARMUP: &str = concat!(
    "❯ Tell me a joke.\n",
    "\n",
    "✻ Mustering…\n",
    "\n",
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  <statusline row 1>\n",
    "  <statusline row 2>\n",
    "  <statusline row 3>\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
);

/// Live capture of a BLOCKING PERMISSION DIALOG — `claude --permission-mode
/// default` v2.1.232 stopping on a `Write` tool call — TRIMMED the same way as
/// the fixtures above (the welcome/tips box, the vendor data-handling banner and
/// the absolute cwd are machine/account-specific and gone; the dialog block, the
/// diff preview above it and the separators are exactly as captured).
///
/// Two facts make it the fixture this feature exists for: there is NO bare
/// prompt and NO busy marker anywhere on it, so `classify_pane` falls to its
/// conservative default and calls it `Busy` forever (asserted below), and the
/// `1 hi` diff row is a numbered content line that is NOT an option (so it pins
/// the `<n>. ` requirement in `dialog_option_text`).
const FIXTURE_REAL_DIALOG_PERMISSION: &str = concat!(
    "❯ Create a file named hello.txt containing the word hi\n",
    "\n",
    "● Write(hello.txt)\n",
    "\n",
    "────────────────────────────────\n",
    " Create file\n",
    " hello.txt\n",
    "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n",
    "  1 hi\n",
    "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n",
    " Do you want to create hello.txt?\n",
    " ❯ 1. Yes\n",
    "   2. Yes, allow all edits during this session (shift+tab)\n",
    "   3. No\n",
    "\n",
    " Esc to cancel · Tab to amend\n",
);

/// The one deliberately FAT fixture, and the only reason to keep one: the
/// same real shape, but with the deepest footer we have observed — 4
/// statusline rows that have each WRAPPED to two lines, so 8 lines sit below
/// the input box — plus enough transcript above the spinner that the pane is
/// LONGER than the [`PANE_TAIL_LINES`] window.
///
/// Its whole job is to pin that window. The spinner ends up 12 non-empty
/// lines above the bottom, still inside the 16-line window, so the pane reads
/// Busy. If the real footer ever grew past the window (or `PANE_TAIL_LINES`
/// shrank), the spinner would fall out of it, this pane would read Idle off
/// its bare prompt — a false Idle that types a nudge into a streaming agent —
/// and THIS is the test that would catch it.
const FIXTURE_DEEP_FOOTER_BUSY: &str = concat!(
    "❯ Tell me a joke.\n",
    "\n",
    "● Why do programmers prefer dark mode?\n",
    "  Because light attracts bugs.\n",
    "\n",
    "● Want another one?\n",
    "  I have a whole changelog of them.\n",
    "\n",
    "  <transcript line the window must drop>\n",
    "  <transcript line the window must drop>\n",
    "\n",
    "✻ Quantumizing… (11s · ↓ 412 tokens)\n",
    "\n",
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  <statusline row 1>\n",
    "  <statusline row 1, wrapped>\n",
    "  <statusline row 2>\n",
    "  <statusline row 2, wrapped>\n",
    "  <statusline row 3>\n",
    "  <statusline row 3, wrapped>\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
    "  <auto-mode row, wrapped>\n",
);

/// The tail of a real pane below the spinner line: separator, the bare prompt
/// with its U+00A0 padding, separator, and the generic last footer line.
const SPINNER_PANE_TAIL: &str = concat!(
    "────────────────────────────────\n",
    "❯\u{a0}\n",
    "────────────────────────────────\n",
    "  ⏵⏵ auto mode on (shift+tab to cycle)\n",
);

/// A minimal pane of the REAL shape carrying exactly one spinner-led line
/// above the input box. Each spinner form then gets its own small,
/// single-purpose test where the asserted distinction is the only thing on
/// screen.
fn pane_with_spinner(spinner: &str) -> String {
    format!("{spinner}\n{SPINNER_PANE_TAIL}")
}

// ---- CODEX pane fixtures --------------------------------------------------
//
// Provenance: `tmux capture-pane` from live `codex 0.146.1` panes on a private
// socket in a scratch tempdir, TRIMMED the same way as the claude fixtures above
// to only what the classifiers read. The model name, the absolute scratch path and
// the context/window counters that made up codex's one-row statusline footer are
// machine- and account-specific, so they are replaced by a neutral placeholder that
// keeps the real ROW COUNT (one) — the footer's only load-bearing property is that
// it sits BELOW the composer, so the composer is never the last non-empty line.
//
// The composer's placeholder text is deliberately DIFFERENT between fixtures
// (`Summarize recent commits` vs `Write tests for @filename`) because both were
// observed on live panes whose submitted prompts were something else entirely.
// That is the proof it is a rotating hint and not input, and pinning two values
// stops anyone keying detection on one of them.

// `  <codex statusline row>` below stands in for codex's one real statusline row
// (model · cwd · context counters — all machine-specific). Its only load-bearing
// property is that it sits BELOW the composer, so idle detection has to be
// position-independent. Spelled out per fixture because `concat!` takes literals only.

/// Live codex capture: a FRESH pane, nothing submitted yet — just the composer
/// with its rotating placeholder, and the footer below it.
const FIXTURE_CODEX_IDLE: &str = concat!(
    "› Summarize recent commits\n",
    "\n",
    "  <codex statusline row>\n"
);

/// Live codex capture: a FINISHED turn. The transcript and the final answer (which
/// codex brackets in separators) stay on screen, and the composer sits below them.
/// The user's echoed message has scrolled off, so here the composer is the ONLY
/// `›`-led line — the state `is_codex_composer` has to read as Idle.
const FIXTURE_CODEX_IDLE_AFTER_TURN: &str = concat!(
    "• Explored\n",
    "  └ Read notes.txt\n",
    "\n",
    "• Edited notes.txt (+1 -0)\n",
    "\n",
    "────────────────────────────────\n",
    "• notes.txt initially had 3 lines. Appended delta; it now has 4 lines.\n",
    "────────────────────────────────\n",
    "\n",
    "› Write tests for @filename\n",
    "\n",
    "  <codex statusline row>\n"
);

/// Live codex capture taken MID-TURN. Two things make it the fixture that keeps the
/// codex widening honest: the composer is drawn here TOO (so only the busy marker
/// prevents a false Idle, exactly as with claude), and the user's submitted message
/// is echoed with its OWN `›` lead — so a `›`-led line is present twice and neither
/// one may be allowed to mean "idle" on its own.
const FIXTURE_CODEX_BUSY: &str = concat!(
    "› Count the lines in notes.txt with awk.\n",
    "\n",
    "• Explored\n",
    "  └ Read notes.txt\n",
    "\n",
    "• Working (3s • esc to interrupt)\n",
    "\n",
    "› Summarize recent commits\n",
    "\n",
    "  <codex statusline row>\n"
);

/// Live codex capture of the MEASURED WARM-UP GAP: sampled every 100ms across a
/// submit, the first two frames (t+0ms, t+100ms) show the echoed user message and the
/// composer but NO `• Working (… esc to interrupt)` line, which appears by t+200ms.
///
/// So codex has a ~100–200ms window where it is working but paints no busy marker,
/// and this pane therefore reads **Idle** — asserted below rather than hidden,
/// because it is the one imperfection in the codex widening and a future reader must
/// be able to see it.
///
/// It is not a live hazard: the nudge gate needs `IDLE_CONFIRMATIONS_REQUIRED` = 2
/// CONSECUTIVE Idle observations `BUSY_RECHECK_S` = 5s apart, and a 200ms window
/// cannot contain two samples 5 seconds apart. claude carries the identical risk for
/// the same reason (its own warm-up spinner takes a moment to paint), which is why
/// that gate exists at all.
const FIXTURE_CODEX_WARMUP_GAP: &str = concat!(
    "› Read notes.txt and tell me its last line.\n",
    "\n",
    "› Write tests for @filename\n",
    "\n",
    "  <codex statusline row>\n"
);

/// Live codex capture: codex asked a FREE-TEXT question and is waiting for prose (not
/// a numbered choice). There is no busy marker, so the composer is what makes this
/// Idle — and it must be, or the human's answer is never typed in and the session
/// waits out the full stall backstop.
const FIXTURE_CODEX_QUESTION: &str = concat!(
    "• What specific change should I make to notes.txt?\n",
    "\n",
    "› Summarize recent commits\n",
    "\n",
    "  <codex statusline row>\n"
);

/// Live codex capture of a BLOCKING APPROVAL DIALOG, verbatim in shape. Three things
/// here each broke a separate `classify_dialog` clause before the fix:
///   1. the selected option is marked with codex's `›`, not claude's `❯`,
///   2. two CONTEXT lines sit between the question and the first option, and
///   3. option 2 WRAPS onto a deeper-indented continuation line.
///
/// Its footer (`Press enter to confirm or esc to cancel`) does contain `esc to
/// cancel`, so that clause of the original rule already passed.
const FIXTURE_CODEX_DIALOG_APPROVAL: &str = concat!(
    "  Would you like to run the following command?\n",
    "\n",
    "  Environment: local\n",
    "\n",
    "  $ awk \"{n++} END {print n}\" notes.txt\n",
    "\n",
    "› 1. Yes, proceed (y)\n",
    "  2. Yes, and don't ask again for commands that start with `awk '{n++} END\n",
    "     {print n}' notes.txt` (p)\n",
    "  3. No, and tell Codex what to do differently (esc)\n",
    "\n",
    "  Press enter to confirm or esc to cancel\n",
);

/// Live codex capture of the STARTUP TRUST DIALOG — which fires even under
/// `-a never -s workspace-write` (and under
/// `--dangerously-bypass-approvals-and-sandbox`), so a fresh codex pane can sit on a
/// dialog before any work begins. Re-captured verbatim on `codex 0.146.1.355` at 80
/// columns; the `> You are in <dir>` line above it and the welcome banner are dropped
/// as machine-specific and classifier-irrelevant.
///
/// The general `classify_dialog` rules still reject it — its `?` is mid-line in a
/// wrapped paragraph and its footer is `Press enter to continue`, not `Esc to cancel`
/// — so it is recognised by the phrase-anchored exception
/// (`classify_codex_trust_dialog`) instead, and this fixture is the tripwire for
/// codex ever rewording that anchor.
const FIXTURE_CODEX_DIALOG_TRUST: &str = concat!(
    "  Do you trust the contents of this directory? Working with untrusted contents\n",
    "  comes with higher risk of prompt injection. Trusting the directory allows\n",
    "  project-local config, hooks, and exec policies to load.\n",
    "\n",
    "› 1. Yes, continue\n",
    "  2. No, quit\n",
    "\n",
    "  Press enter to continue\n",
);
