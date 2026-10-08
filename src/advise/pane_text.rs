//! Text that ends up in (or is read out of) a live worker pane: the control-byte
//! predicate both halves of the trust asymmetry are built on, the sanitizer for quoted
//! worker text, the one line the harness types for a verdict, and the pane fingerprint
//! the anti-ping-pong check compares. One unit because they all exist to keep
//! `tmux send-keys` from being handed a keystroke instead of a sentence.

use super::consult::Consult;
use super::reply::{Refusal, Verdict};

/// Characters of harness-quoted worker text (the question, one option) echoed back to
/// the worker. It already knows what it asked; this is only there so the answer is
/// self-describing when it arrives a cadence later.
const MAX_QUOTED_TEXT: usize = 300;

/// Does `s` contain any byte that must never reach `tmux send-keys`?
///
/// Rejects the full C0 range (`0x00..=0x1F` — so ESC `\x1b`, CR `\r`, LF `\n` and TAB
/// are all out), DEL (`0x7F`), and the C1 range (`0x80..=0x9F`). Ordinary space
/// (`0x20`) is allowed, as is any printable character above C1.
///
/// Why the whole range and not just newlines: `send-keys -l` writes bytes verbatim
/// (measured against a real tmux server), so `\x1b[Z` arrives as **shift+tab**, which
/// cycles claude's permission mode, and `\r` submits a second message. A "no newline"
/// check stops neither.
pub fn has_forbidden_control_bytes(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        u < 0x20 || u == 0x7f || (0x80..=0x9f).contains(&u)
    })
}

/// Replace every byte [`has_forbidden_control_bytes`] rejects with a single space, then
/// collapse whitespace runs and trim. Used ONLY on harness-quoted worker text (see the
/// trust-asymmetry note in the module docs); supervisor-authored text is rejected, not
/// cleaned.
pub fn sanitize_control_bytes(s: &str) -> String {
    let swapped: String = s
        .chars()
        .map(|c| {
            let u = c as u32;
            if u < 0x20 || u == 0x7f || (0x80..=0x9f).contains(&u) {
                ' '
            } else {
                c
            }
        })
        .collect();
    swapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FNV-1a over the whole string — the pane fingerprint behind the anti-ping-pong
/// check. Only needs to detect "byte-identical", so a non-cryptographic hash is the
/// right tool; it mirrors `tmux`'s own `hash8` construction.
pub fn text_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Harness-quoted worker text, made safe to echo into a live pane: control bytes
/// sanitized to spaces, whitespace collapsed, double quotes downgraded (the text is
/// wrapped in quotes), and truncated on a CHARACTER boundary. The SANITIZE half of the
/// trust asymmetry — a worker must not be able to veto the whole feature by emitting a
/// tab.
fn quote_for_pane(s: &str) -> String {
    let cleaned = sanitize_control_bytes(s).replace('"', "'");
    if cleaned.chars().count() <= MAX_QUOTED_TEXT {
        return cleaned;
    }
    let mut out: String = cleaned.chars().take(MAX_QUOTED_TEXT).collect();
    out.push('…');
    out
}

/// Render the ONE line of text the harness will type into the worker for a validated
/// verdict — the goal-aware replacement for the canned
/// `"Auto-approved (low-stakes, no human needed)…"` string.
///
/// Returns `Err` rather than a sanitized string if the assembled bytes still contain
/// anything [`has_forbidden_control_bytes`] rejects. That cannot happen — supervisor
/// text was rejected upstream and quoted worker text was sanitized — which is precisely
/// why it is worth checking: it turns "no control byte reaches `send_keys`" from an
/// argument into a verified invariant, without ever panicking on untrusted input.
pub fn apply_text(consult: &Consult, verdict: &Verdict) -> Result<String, Refusal> {
    let question = quote_for_pane(&consult.question);
    let body = match verdict {
        Verdict::Select {
            index,
            option,
            reason,
        } => format!(
            "on \"{question}\", take option {n} — {opt}. Why: {reason}",
            n = index + 1,
            opt = quote_for_pane(option),
        ),
        Verdict::Answer { text, reason } => {
            format!("on \"{question}\": {text} Why: {reason}")
        }
    };
    let s = format!(
        "Decision from your session supervisor (this was low-stakes, so it was resolved \
         against your goal without a human): {body} Proceed on that basis and do not \
         re-ask."
    );
    if has_forbidden_control_bytes(&s) {
        return Err(Refusal::ControlBytes {
            field: "assembled decision".to_string(),
        });
    }
    Ok(s)
}
