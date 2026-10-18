//! Render an agent's event stream — claude's `--output-format stream-json`, codex's `exec --json` —
//! into readable lines.
//!
//! The wake worker runs `claude -p … --output-format stream-json --verbose`, so
//! each `steps/<seq>.log` is a stream of newline-delimited JSON events rather than
//! plain prose. Showing that raw in the pmtui wake pane is unreadable, so this
//! module turns the events into terse, human-friendly lines:
//!   - assistant prose (`text` blocks) verbatim,
//!   - `→ Tool: <input summary>` for a `tool_use`,
//!   - `  ⎿ <first line>` for a `tool_result`,
//!   - `✔ <result>` / `✖ <error>` for the final `result` event.
//!
//! It is intentionally DEFENSIVE and PURE (no I/O): a line that is not a JSON
//! object passes through verbatim (so a plain error like "No conversation found…"
//! still shows), unknown event types are skipped, and nothing here ever panics —
//! this is presentation-adjacent code that must never crash the TUI. A later
//! chunk reuses this parser for a full-screen transcript view.

use serde_json::Value;

/// Max width for an assistant prose line or a final result before truncation, so
/// one monster line can't blow up the pane layout.
const MAX_LINE: usize = 200;
/// Max width for a tool-input summary or a tool-result snippet.
const MAX_SUMMARY: usize = 100;

/// Render a Claude `--output-format stream-json` log (newline-delimited JSON
/// events) into readable lines. See the module docs for the guarantees.
pub fn render_transcript(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            // A JSON object is a stream-json event; anything else (bare string,
            // number, array) or invalid JSON passes through verbatim so plain
            // errors are never silently dropped.
            Ok(v) if v.is_object() => render_event(&v, &mut out),
            _ => out.push(truncate(trimmed, MAX_LINE)),
        }
    }
    out
}

/// Dispatch a single parsed event object on its `type`.
///
/// Both engines' streams land here, because both are read for the same reason — a human looking at what
/// a run is doing. claude's `assistant`/`user`/`result` and codex's `item.completed`/`turn.*` describe
/// the same three things (prose, a tool, an ending), so they render to the same three shapes rather than
/// to two transcripts that look nothing alike.
fn render_event(v: &Value, out: &mut Vec<String>) {
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => render_message(v, out, true),
        Some("user") => render_message(v, out, false),
        Some("result") => render_result(v, out),
        Some("item.completed") => render_codex_item(v, out),
        Some("turn.completed") => out.push("\u{2714} turn complete".into()),
        Some("turn.failed") => {
            let why = v
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the turn failed");
            out.push(format!("\u{2716} {}", truncate(why, MAX_LINE)));
        }
        // "system"/"thread.started" (session bookkeeping) and any unknown type -> skip, stay terse.
        _ => {}
    }
}

/// One codex `item.completed`: its agent prose, or the command it ran.
fn render_codex_item(v: &Value, out: &mut Vec<String>) {
    let Some(item) = v.get("item") else {
        return;
    };
    match item.get("type").and_then(Value::as_str) {
        Some("agent_message") => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                for line in text.lines() {
                    if !line.trim().is_empty() {
                        out.push(truncate(line.trim_end(), MAX_LINE));
                    }
                }
            }
        }
        // A command codex ran reads as the same tool line claude's `tool_use` produces.
        Some("command_execution") => {
            if let Some(command) = item
                .get("command")
                .and_then(Value::as_str)
                .or_else(|| item.get("text").and_then(Value::as_str))
            {
                out.push(format!(
                    "\u{2192} Tool: {}",
                    truncate(command.trim(), MAX_SUMMARY)
                ));
            }
        }
        _ => {}
    }
}

/// Render an `assistant`/`user` event's `message.content[]` blocks. Assistant
/// events contribute `text` prose and `→ Tool: …` lines; user events contribute
/// `  ⎿ …` tool-result snippets.
fn render_message(v: &Value, out: &mut Vec<String>, assistant: bool) {
    let Some(content) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") if assistant => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    for line in text.lines() {
                        if !line.trim().is_empty() {
                            out.push(truncate(line.trim_end(), MAX_LINE));
                        }
                    }
                }
            }
            Some("tool_use") if assistant => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                let summary = tool_input_summary(block.get("input"));
                if summary.is_empty() {
                    out.push(format!("→ {name}"));
                } else {
                    out.push(format!("→ {name}: {summary}"));
                }
            }
            Some("tool_result") if !assistant => {
                if let Some(short) = tool_result_short(block) {
                    out.push(format!("  ⎿ {short}"));
                }
            }
            _ => {}
        }
    }
}

/// A short one-line view of a `tool_use` input: the single most-relevant field
/// for common tools (Bash command, Read/Write/Edit path, Grep/Glob pattern),
/// else the compact JSON. Collapsed to one line and truncated.
fn tool_input_summary(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let picked = input
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| input.get("file_path").and_then(Value::as_str))
        .or_else(|| input.get("path").and_then(Value::as_str))
        .or_else(|| input.get("pattern").and_then(Value::as_str))
        .or_else(|| input.get("url").and_then(Value::as_str))
        .or_else(|| input.get("description").and_then(Value::as_str));
    let s = match picked {
        Some(s) => first_nonempty_line(s).to_string(),
        None => match input {
            Value::Object(m) if m.is_empty() => String::new(),
            _ => serde_json::to_string(input).unwrap_or_default(),
        },
    };
    truncate(&s, MAX_SUMMARY)
}

/// The first non-empty line of a `tool_result` block's content, truncated. The
/// content may be a bare string or an array of `{type,text}` blocks. `None` when
/// there is nothing to show (empty result).
fn tool_result_short(block: &Value) -> Option<String> {
    let content = block.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let mut buf = String::new();
            for item in items {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    buf.push_str(t);
                }
            }
            buf
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let first = first_nonempty_line(&text);
    if first.is_empty() {
        return None;
    }
    Some(truncate(first, MAX_SUMMARY))
}

/// Render the terminal `result` event as `✔ …` (success) or `✖ …` (error).
fn render_result(v: &Value, out: &mut Vec<String>) {
    let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
        || v.get("subtype").and_then(Value::as_str) == Some("error");
    if is_error {
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| v.get("message").and_then(Value::as_str))
            .or_else(|| v.get("result").and_then(Value::as_str))
            .unwrap_or("error");
        let first = first_nonempty_line(msg);
        let shown = if first.is_empty() { "error" } else { first };
        out.push(format!("✖ {}", truncate(shown, MAX_LINE)));
    } else {
        let r = v.get("result").and_then(Value::as_str).unwrap_or("");
        let first = first_nonempty_line(r);
        let shown = if first.is_empty() { "done" } else { first };
        out.push(format!("✔ {}", truncate(shown, MAX_LINE)));
    }
}

/// First non-empty (trimmed) line of `s`, or `""` if all lines are blank.
fn first_nonempty_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Truncate `s` to at most `max` characters, appending `…` when it was cut. Uses
/// char boundaries so it can never panic on multi-byte UTF-8.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests;
