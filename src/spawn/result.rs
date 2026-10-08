//! A job child's RESULT: what the harness reports when the run ends.
//!
//! The child is a one-shot headless run, so nothing here asks the agent to leave a file behind as
//! its final act. That design was tried elsewhere and is a known trap: agent-deck's
//! `done_sentinel.go` exists because "Claude's Stop hook fires at the end of every turn and is
//! mapped to the generic 'waiting' status", which left their conductor with no trustworthy
//! "finished" signal for an INTERACTIVE child. A process that exits has no such ambiguity, and both
//! CLIs will emit a final payload conforming to a schema we hand them:
//!
//! - claude: the last line of `--output-format stream-json` is a `result` event, and
//!   `--json-schema` puts our fields in its `structured_output`.
//! - codex: `--output-schema` shapes the final message and `-o <path>` writes it to a file, with
//!   `turn.completed` / `turn.failed` in the `--json` stream saying which way the turn went.
//!
//! Two parse rules are borrowed from that same prior art: the LAST valid payload wins (a run that
//! reported once and kept going must report its final outcome), and anything malformed is REJECTED
//! rather than guessed at. Every branch below is driven by a field the harness emitted — never by
//! reading the agent's prose.

use serde::{Deserialize, Serialize};

use crate::registry::Engine;

/// What a job says became of its task. Deliberately three words: a job reports on its own TASK, and
/// no machine declares the project done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobOutcome {
    /// The task is finished.
    Done,
    /// The task cannot proceed without a person.
    NeedsHuman,
    /// The task was attempted and failed.
    Failed,
}

/// The longest `summary` a receipt carries. Over-long text is truncated, never rejected: a child
/// that did the work must not lose its report to a size rule.
pub const SUMMARY_MAX: usize = 4096;
/// The longest `detail` a receipt carries.
pub const DETAIL_MAX: usize = 16384;
/// Appended to anything this module shortens, so a truncated field never reads as a complete one.
pub const TRUNCATION_MARKER: &str = "\u{2026} (truncated)";

/// A job's report, as it lands in the parent's receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub outcome: JobOutcome,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The shape a job's final payload is asked for, handed to claude as `--json-schema` and to codex as
/// `--output-schema`. Written to the child's state directory at launch.
///
/// `additionalProperties: false` and the `enum` keep the model from inventing an outcome word; the
/// parse below re-derives the outcome from the harness's own terminator regardless, so a schema a CLI
/// ignores costs nothing.
pub fn result_schema_json() -> String {
    r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["outcome", "summary"],
  "properties": {
    "outcome": { "type": "string", "enum": ["done", "needs_human", "failed"] },
    "summary": { "type": "string", "description": "One or two sentences: what became of the task." },
    "detail": { "type": "string", "description": "Optional longer prose for a human reading afterwards." }
  }
}
"#
    .to_string()
}

/// `text` shortened to `max` bytes on a character boundary, with [`TRUNCATION_MARKER`] appended when
/// anything was dropped.
fn clamp(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // Leave room for the marker, then back off to a character boundary.
    let budget = max.saturating_sub(TRUNCATION_MARKER.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &text[..end])
}

impl JobResult {
    /// A result with both text fields clamped, and an empty summary replaced by a stand-in — a
    /// receipt that says nothing is worse than one that says the child reported nothing.
    fn clamped(outcome: JobOutcome, summary: &str, detail: Option<&str>) -> Self {
        let summary = summary.trim();
        Self {
            outcome,
            summary: if summary.is_empty() {
                "(the child reported no summary)".to_string()
            } else {
                clamp(summary, SUMMARY_MAX)
            },
            detail: detail
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(|d| clamp(d, DETAIL_MAX)),
        }
    }
}

/// Our schema's shape, for deserializing a payload that followed it.
#[derive(Deserialize)]
struct Payload {
    outcome: JobOutcome,
    summary: String,
    detail: Option<String>,
}

/// A payload conforming to [`result_schema_json`], or `None` when it does not.
fn payload(value: &serde_json::Value) -> Option<JobResult> {
    let p: Payload = serde_json::from_value(value.clone()).ok()?;
    Some(JobResult::clamped(
        p.outcome,
        &p.summary,
        p.detail.as_deref(),
    ))
}

/// Every line of `log` that parses as a JSON object, in order. A tee'd stream log can also hold the
/// CLI's own human-readable noise, so a line that is not JSON is skipped rather than failing the scan.
fn json_lines(log: &str) -> impl Iterator<Item = serde_json::Value> + '_ {
    log.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
}

/// The job's result, read from what the harness left behind: `log` is the tee'd combined output and
/// `result_file` the contents of codex's `-o` file when it exists.
///
/// `None` means the run left no usable report — it crashed, was killed, or never reached its end —
/// which the broker records as `ended_without_result`. It never means "the agent forgot a step".
pub fn read_job_result(engine: Engine, log: &str, result_file: Option<&str>) -> Option<JobResult> {
    match engine {
        Engine::Claude => claude_result(log),
        Engine::Codex => codex_result(log, result_file),
    }
}

/// claude: the LAST `result` event in the stream decides.
///
/// Its `structured_output` is preferred, because that is our schema. Failing that, the event's own
/// `subtype` still proves how the run ended and `result` carries the final text — a harness-stated
/// fact, so using it is not guesswork. Anything else is no result at all.
fn claude_result(log: &str) -> Option<JobResult> {
    let last = json_lines(log)
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"))
        .last()?;
    if let Some(structured) = last.get("structured_output")
        && let Some(result) = payload(structured)
    {
        return Some(result);
    }
    let text = last.get("result").and_then(|r| r.as_str()).unwrap_or("");
    match last.get("subtype").and_then(|s| s.as_str()) {
        Some("success") => Some(JobResult::clamped(JobOutcome::Done, text, None)),
        // Every other subtype claude emits on a `result` event is a failure of the run
        // (`error_during_execution`, `error_max_turns`, …), so the outcome is stated, not inferred.
        Some(other) => Some(JobResult::clamped(
            JobOutcome::Failed,
            if text.is_empty() { other } else { text },
            None,
        )),
        None => None,
    }
}

/// codex: THE STREAM SAYS HOW THE RUN ENDED, and the `-o` file says what the model's last message was.
///
/// The file is never the commit signal. codex writes it when the model's final message lands, which is
/// before the turn is over: a turn that fails afterwards still leaves a well-formed file behind, and
/// codex has shipped versions that then exit 0. So the terminator is read first — a turn that failed is
/// a failed run whatever the file claims, and a stream with no terminator reported nothing at all. On a
/// completed turn the file is authoritative, because that is our schema and it carries the outcome the
/// model itself chose. Without a usable file the last `agent_message` supplies the summary.
fn codex_result(log: &str, result_file: Option<&str>) -> Option<JobResult> {
    let file_text = result_file.map(str::trim).filter(|text| !text.is_empty());
    let from_file = file_text
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| payload(&value));

    let terminator = json_lines(log)
        .filter_map(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .filter(|t| matches!(*t, "turn.completed" | "turn.failed"))
                .map(str::to_string)
        })
        .last()?;
    if terminator == "turn.completed"
        && let Some(result) = from_file
    {
        return Some(result);
    }

    let text = from_file
        .map(|result| result.summary)
        .or_else(|| file_text.map(str::to_string))
        .or_else(|| last_agent_message(log))
        .unwrap_or_default();
    let outcome = if terminator == "turn.completed" {
        JobOutcome::Done
    } else {
        JobOutcome::Failed
    };
    Some(JobResult::clamped(outcome, &text, None))
}

/// The thread id codex announced for this run, from the FIRST `thread.started` event in its stream.
///
/// codex has no caller-chosen conversation id — the one flag claude does have (`--session-id`) — so the
/// only way to learn which conversation a run created is to read the id codex generated. A job exits, so
/// it never resumes itself; this is what lets a HUMAN pick up the conversation of a job that failed or
/// asked for them, instead of losing it with the process.
pub fn codex_thread_id(log: &str) -> Option<String> {
    json_lines(log)
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("thread.started"))
        .find_map(|v| {
            Some(
                v.get("thread_id")
                    .or_else(|| v.get("thread").and_then(|t| t.get("id")))?
                    .as_str()?
                    .to_string(),
            )
        })
        .filter(|id| !id.is_empty())
}

/// The text of the last completed `agent_message` item in a codex stream.
fn last_agent_message(log: &str) -> Option<String> {
    json_lines(log)
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("item.completed"))
        .filter_map(|v| {
            let item = v.get("item")?;
            if item.get("type").and_then(|t| t.as_str()) != Some("agent_message") {
                return None;
            }
            Some(item.get("text")?.as_str()?.to_string())
        })
        .last()
}
