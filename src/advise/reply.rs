//! Reading the supervisor's reply: what a usable verdict and every refusal are, how the
//! JSON object is recovered from the process's combined output, and the checks that
//! decide between the two. The refusal vocabulary lives with the code that raises it so
//! a new check cannot be added without naming what it refuses.

use std::fmt;

use serde_json::{Map, Value};

use super::consult::{Consult, Grant};
use super::pane_text::has_forbidden_control_bytes;

/// Characters of SUPERVISOR-authored text (`text` / `reason`) allowed to reach the
/// worker's pane. A supervisor that wants to write an essay is not answering a
/// low-stakes question, and an unbounded paste into a live REPL is its own hazard.
pub(super) const MAX_SUPERVISOR_TEXT: usize = 400;

/// A validated verdict, expressed only in terms the harness already owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Pick option `index` (0-based, in range). `option` is
    /// `Consult::options[index]` — the HARNESS's text, copied here so callers cannot
    /// accidentally reach for a supervisor-supplied string.
    Select {
        index: usize,
        option: String,
        reason: String,
    },
    /// A short free-text instruction (control-byte-free, length-capped).
    Answer { text: String, reason: String },
}

/// Why a consult produced nothing usable. EVERY variant escalates to the human — none
/// of them falls back to a canned approval, and none of them retries silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No JSON object could be recovered from the supervisor's output at all
    /// (empty log, prose only, truncated write, a crash message).
    NoJson,
    /// A JSON object, but with no `nonce` field — so it is not provably a reply to
    /// THIS consult.
    MissingNonce,
    /// A `nonce` that is not ours: a replayed old reply, or a reply steered by
    /// injected text that guessed wrong.
    WrongNonce { got: String },
    /// No `action`, or one outside the three we understand.
    UnknownAction { got: String },
    /// A legal action, but not one this consult granted (free text where options were
    /// enumerated, or an index where none were).
    OutsideGrant { action: String, grant: Grant },
    /// `select_option` with no usable integer `option_index` (absent, a float, a
    /// string, or not representable).
    MissingIndex,
    /// `option_index` outside `0..options.len()` — including negatives. This is the
    /// ONLY way a "hallucinated option" can present itself, by construction.
    IndexOutOfRange { got: i64, options: usize },
    /// `answer` with an absent or blank `text`.
    EmptyText,
    /// Supervisor-authored text longer than the cap.
    TooLong { field: String, chars: usize },
    /// A C0/C1 control byte (ESC, CR, DEL, …) in text destined for the pane. Named so
    /// the log says which field, because this is the escape-sequence injection path.
    ControlBytes { field: String },
    /// The supervisor did its job and declined. Carries its reason for the human.
    SupervisorRefused { reason: String },
    /// The supervisor never produced a usable result: it timed out, exited non-zero,
    /// or its session vanished. Raised by the caller, not by [`validate`].
    NoResult { detail: String },
    /// Byte-identical advice on a byte-identical pane (Rule 7): the worker is not
    /// responding to what we typed, so typing it again is a ping-pong loop.
    NoProgress,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoJson => write!(f, "the supervisor produced no parseable JSON reply"),
            Self::MissingNonce => write!(f, "the supervisor's reply carried no nonce"),
            Self::WrongNonce { got } => {
                write!(f, "the supervisor echoed the wrong nonce ({got:?})")
            }
            Self::UnknownAction { got } => {
                write!(f, "the supervisor asked for an unknown action ({got:?})")
            }
            Self::OutsideGrant { action, grant } => write!(
                f,
                "the supervisor asked for {action:?}, which is outside this decision's \
                 grant ({grant:?})"
            ),
            Self::MissingIndex => write!(
                f,
                "the supervisor chose an option but gave no usable option_index"
            ),
            Self::IndexOutOfRange { got, options } => write!(
                f,
                "the supervisor chose option_index {got}, but only {options} options exist"
            ),
            Self::EmptyText => write!(f, "the supervisor's answer text was empty"),
            Self::TooLong { field, chars } => {
                write!(
                    f,
                    "the supervisor's {field} was {chars} characters — too long"
                )
            }
            Self::ControlBytes { field } => write!(
                f,
                "the supervisor's {field} contained control/escape bytes, which must \
                 never be typed into a live pane"
            ),
            Self::SupervisorRefused { reason } => {
                write!(f, "the supervisor declined to decide: {reason}")
            }
            Self::NoResult { detail } => {
                write!(f, "the supervisor produced no result: {detail}")
            }
            Self::NoProgress => write!(
                f,
                "the same advice was already delivered to an unchanged pane — the \
                 worker is not acting on it"
            ),
        }
    }
}

/// Validate a supervisor's raw output against the consult it was asked. **Pure**: it
/// reads nothing, writes nothing, spawns nothing, and never panics — `raw` is
/// untrusted input, so there is no `unwrap`/`expect` on anything parsed out of it and
/// no slicing that can be out of range.
///
/// `raw` is the whole tee'd log of the consult process, so it may contain stderr noise
/// around the one JSON line; see [`extract_object`].
pub fn validate(consult: &Consult, raw: &str) -> Result<Verdict, Refusal> {
    let obj = extract_object(raw).ok_or(Refusal::NoJson)?;

    // Rule 5: prove this reply is about THIS consult before reading any of its
    // content. A reply steered by injected text cannot echo a nonce it never saw, and a
    // replayed older reply carries the wrong one. (The received value is trimmed — the
    // nonce itself has no whitespace, so trimming cannot make a wrong nonce match.)
    let nonce = obj
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or(Refusal::MissingNonce)?
        .trim();
    if nonce != consult.nonce {
        return Err(Refusal::WrongNonce {
            got: nonce.chars().take(64).collect(),
        });
    }

    // Supervisor-authored, so REJECTED (not cleaned) on any control byte or overrun.
    let reason = checked_supervisor_text(obj.get("reason").and_then(Value::as_str), "reason")?;

    let action = obj
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let grant = consult.grant();
    match action.as_str() {
        // The supervisor doing its job. Its reason is what the human reads.
        "refuse" => Err(Refusal::SupervisorRefused {
            reason: if reason.is_empty() {
                "no reason given".to_string()
            } else {
                reason
            },
        }),
        "select_option" => {
            if grant != Grant::PickOption {
                return Err(Refusal::OutsideGrant { action, grant });
            }
            // `as_i64` deliberately: a float, a string, or a bare-word index is NOT an
            // index, and a NEGATIVE one must be caught as out-of-range rather than
            // wrapping into a huge `usize`.
            let raw_index = obj
                .get("option_index")
                .and_then(Value::as_i64)
                .ok_or(Refusal::MissingIndex)?;
            let len = consult.options.len();
            let index = usize::try_from(raw_index).ok().filter(|i| *i < len).ok_or(
                Refusal::IndexOutOfRange {
                    got: raw_index,
                    options: len,
                },
            )?;
            // Rule 1: the option text is the HARNESS's, read out of the consult by
            // index. The supervisor's transcription (if it sent one) is discarded, so a
            // hallucinated option is unrepresentable rather than merely unlikely.
            let option = consult.options.get(index).cloned().ok_or(
                // Unreachable: `index < len` was just checked. Kept as an `Err` rather
                // than an `unwrap` because this function must not panic on any input.
                Refusal::IndexOutOfRange {
                    got: raw_index,
                    options: len,
                },
            )?;
            Ok(Verdict::Select {
                index,
                option,
                reason,
            })
        }
        "answer" => {
            if grant != Grant::FreeAnswer {
                return Err(Refusal::OutsideGrant { action, grant });
            }
            let text = checked_supervisor_text(obj.get("text").and_then(Value::as_str), "answer")?;
            if text.is_empty() {
                return Err(Refusal::EmptyText);
            }
            Ok(Verdict::Answer { text, reason })
        }
        other => Err(Refusal::UnknownAction {
            got: other.chars().take(64).collect(),
        }),
    }
}

/// A supervisor-authored string, checked and trimmed: absent ⇒ empty (the caller
/// decides whether empty is fatal), any control byte ⇒ [`Refusal::ControlBytes`], over
/// the cap ⇒ [`Refusal::TooLong`]. This is the REJECT half of the trust asymmetry.
fn checked_supervisor_text(raw: Option<&str>, field: &str) -> Result<String, Refusal> {
    let s = raw.unwrap_or("");
    if has_forbidden_control_bytes(s) {
        return Err(Refusal::ControlBytes {
            field: field.to_string(),
        });
    }
    let chars = s.chars().count();
    if chars > MAX_SUPERVISOR_TEXT {
        return Err(Refusal::TooLong {
            field: field.to_string(),
            chars,
        });
    }
    Ok(s.trim().to_string())
}

/// Recover our JSON object from the consult's tee'd log.
///
/// The log holds the child's COMBINED output, so a `claude --output-format json`
/// envelope (one line) can be surrounded by stderr noise, and the object we want may be
/// nested: `.structured_output` (from `--json-schema`) or `.result` (a STRING holding
/// the model's reply, possibly inside a ```` ``` ```` fence). Lines are scanned from the
/// END because the result line is the last thing written; the whole-capture brace span
/// is only a fallback for a pretty-printed envelope. Returns `None` rather than erroring
/// on anything unrecognisable.
fn extract_object(raw: &str) -> Option<Map<String, Value>> {
    for line in raw.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(m) = object_from_str(line) {
            return Some(m);
        }
    }
    // Fallback: the widest brace span in the capture (a pretty-printed envelope).
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end < start {
        return None;
    }
    object_from_str(raw.get(start..=end)?)
}

/// Parse one candidate string into our object, unwrapping a `claude` envelope if that
/// is what it is. Non-recursive by construction (it only ever calls [`inner_object`],
/// which does not call back), so untrusted input cannot drive it into deep recursion.
fn object_from_str(s: &str) -> Option<Map<String, Value>> {
    let value: Value = serde_json::from_str(s).ok()?;
    let obj = value.as_object()?;
    // `--json-schema` puts the validated object here directly.
    if let Some(inner) = obj.get("structured_output").and_then(Value::as_object) {
        return Some(inner.clone());
    }
    // `--output-format json` puts the model's reply in `result`, as a string (or, for
    // some shapes, already as an object).
    match obj.get("result") {
        Some(Value::String(text)) => {
            if let Some(inner) = inner_object(text) {
                return Some(inner);
            }
        }
        Some(Value::Object(inner)) => return Some(inner.clone()),
        _ => {}
    }
    // Not an envelope: it may be the bare object itself (a stub supervisor, or `-p`
    // with no `--output-format`).
    if obj.contains_key("action") || obj.contains_key("nonce") {
        return Some(obj.clone());
    }
    None
}

/// Parse the model's reply text (which may be fenced with ```` ```json ````) into an
/// object, falling back to its widest brace span.
fn inner_object(text: &str) -> Option<Map<String, Value>> {
    let t = strip_code_fences(text);
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(t) {
        return Some(m);
    }
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end < start {
        return None;
    }
    match serde_json::from_str::<Value>(t.get(start..=end)?) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// Strip a surrounding markdown code fence, if present. Purely cosmetic tolerance —
/// [`inner_object`]'s brace-span fallback would find the object anyway; this keeps the
/// common case on the exact-parse path.
fn strip_code_fences(text: &str) -> &str {
    let t = text.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // Drop the info string (`json`) on the opening fence, then the closing fence.
    let rest = rest.split_once('\n').map_or("", |(_, r)| r);
    rest.trim().strip_suffix("```").unwrap_or(rest).trim()
}
