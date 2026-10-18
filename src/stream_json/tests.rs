use super::*;

#[test]
fn empty_input_yields_no_lines() {
    assert!(render_transcript("").is_empty());
    assert!(render_transcript("   \n\n  ").is_empty());
}

#[test]
fn assistant_text_event_yields_its_lines() {
    let raw = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello there\n\nsecond line"}]}}"#;
    assert_eq!(
        render_transcript(raw),
        vec!["Hello there".to_string(), "second line".to_string()]
    );
}

#[test]
fn assistant_tool_use_bash_summarizes_command() {
    let raw = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la /tmp","description":"list"}}]}}"#;
    assert_eq!(
        render_transcript(raw),
        vec!["→ Bash: ls -la /tmp".to_string()]
    );
}

#[test]
fn assistant_tool_use_read_summarizes_file_path() {
    let raw = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/etc/hosts"}}]}}"#;
    assert_eq!(
        render_transcript(raw),
        vec!["→ Read: /etc/hosts".to_string()]
    );
}

#[test]
fn assistant_tool_use_unknown_falls_back_to_compact_json() {
    let raw = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Weird","input":{"a":1}}]}}"#;
    assert_eq!(
        render_transcript(raw),
        vec!["→ Weird: {\"a\":1}".to_string()]
    );
}

#[test]
fn user_tool_result_string_content() {
    let raw = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"first line\nsecond"}]}}"#;
    assert_eq!(render_transcript(raw), vec!["  ⎿ first line".to_string()]);
}

#[test]
fn user_tool_result_array_content() {
    let raw = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"text","text":"array result"}]}]}}"#;
    assert_eq!(render_transcript(raw), vec!["  ⎿ array result".to_string()]);
}

#[test]
fn empty_tool_result_is_skipped() {
    let raw = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":""}]}}"#;
    assert!(render_transcript(raw).is_empty());
}

#[test]
fn success_result_event() {
    let raw = r#"{"type":"result","subtype":"success","is_error":false,"result":"All done here"}"#;
    assert_eq!(render_transcript(raw), vec!["✔ All done here".to_string()]);
}

#[test]
fn error_result_via_is_error() {
    let raw = r#"{"type":"result","subtype":"success","is_error":true,"result":"boom happened"}"#;
    assert_eq!(render_transcript(raw), vec!["✖ boom happened".to_string()]);
}

#[test]
fn error_result_via_subtype() {
    let raw = r#"{"type":"result","subtype":"error","error":"the failure message"}"#;
    assert_eq!(
        render_transcript(raw),
        vec!["✖ the failure message".to_string()]
    );
}

#[test]
fn non_json_line_passes_through_verbatim() {
    let raw = "No conversation found for this session";
    assert_eq!(
        render_transcript(raw),
        vec!["No conversation found for this session".to_string()]
    );
}

#[test]
fn system_and_unknown_types_are_skipped() {
    let raw = concat!(
        r#"{"type":"system","subtype":"init","session_id":"abc"}"#,
        "\n",
        r#"{"type":"totally_unknown","foo":"bar"}"#
    );
    assert!(render_transcript(raw).is_empty());
}

#[test]
fn long_line_is_truncated_with_ellipsis() {
    let long = "x".repeat(400);
    let raw = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
    );
    let out = render_transcript(&raw);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].chars().count(), MAX_LINE);
    assert!(out[0].ends_with('…'));
}

#[test]
fn multi_event_stream_renders_in_order() {
    let raw = concat!(
        r#"{"type":"system","subtype":"init"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"thinking"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"echo hi"}}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"hi"}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","result":"finished"}"#
    );
    assert_eq!(
        render_transcript(raw),
        vec![
            "thinking".to_string(),
            "→ Bash: echo hi".to_string(),
            "  ⎿ hi".to_string(),
            "✔ finished".to_string(),
        ]
    );
}

/// CODEX'S STREAM READS LIKE CLAUDE'S. A job can run either engine, and a pane showing nothing
/// because the renderer only knew one of them is the same unreadable panel in a different costume.
#[test]
fn codex_items_render_as_prose_tools_and_an_ending() {
    let raw = concat!(
        r#"{"type":"thread.started","thread_id":"t1"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Renamed the flag.\nChecked its test."}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"cargo test --lib"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#,
        "\n"
    );
    assert_eq!(
        render_transcript(raw),
        vec![
            "Renamed the flag.".to_string(),
            "Checked its test.".to_string(),
            "\u{2192} Tool: cargo test --lib".to_string(),
            "\u{2714} turn complete".to_string(),
        ],
        "thread.started is bookkeeping and stays out"
    );
}

/// A failed turn names WHY, and an item with nothing to show adds no line.
#[test]
fn a_failed_codex_turn_is_named_and_empty_items_are_skipped() {
    let failed = r#"{"type":"turn.failed","error":{"message":"the sandbox denied a write"}}"#;
    assert_eq!(
        render_transcript(failed),
        vec!["\u{2716} the sandbox denied a write".to_string()]
    );
    assert_eq!(
        render_transcript(r#"{"type":"turn.failed"}"#),
        vec!["\u{2716} the turn failed".to_string()],
        "a failure with no message still reports the failure"
    );
    assert!(
        render_transcript(concat!(
            r#"{"type":"item.completed","item":{"type":"reasoning"}}"#,
            "\n",
            r#"{"type":"item.completed"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"   "}}"#
        ))
        .is_empty(),
        "nothing to show adds no line"
    );
}

/// THE BRANCHES THE SUITE NEVER REACHED, each driven through the one public entry.
///
/// All of these were uncovered production lines passing the 95% per-file gate, because this test
/// module used to live inside `stream_json.rs` and its own lines counted toward that file's score.
/// Moving it out made the gap measurable; this closes it. A transcript renderer is exactly the kind
/// of code where an unexercised branch is invisible until a real stream hits it.
#[test]
fn a_tool_use_with_no_usable_input_renders_the_name_alone() {
    // `input` absent entirely, and `input: {}` — both yield an empty summary, so the line is the
    // bare `→ name` rather than `→ name: `.
    let absent =
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task"}]}}"#;
    assert_eq!(render_transcript(absent), vec!["→ Task".to_string()]);
    let empty = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task","input":{}}]}}"#;
    assert_eq!(render_transcript(empty), vec!["→ Task".to_string()]);
    // A tool with no name at all still reads as something.
    let nameless = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","input":{"pattern":"fn main"}}]}}"#;
    assert_eq!(
        render_transcript(nameless),
        vec!["→ tool: fn main".to_string()]
    );
}

/// A `tool_result` whose content is neither a string nor a `{text}` array falls back to compact JSON,
/// and one that renders to nothing is dropped rather than printed as an empty `⎿`.
#[test]
fn a_tool_result_falls_back_to_json_and_drops_an_empty_one() {
    let numeric = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":42}]}}"#;
    assert_eq!(render_transcript(numeric), vec!["  ⎿ 42".to_string()]);

    // An array whose items carry no `text` builds an empty buffer, which is nothing to show.
    let textless = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"type":"image"}]}]}}"#;
    assert!(render_transcript(textless).is_empty());

    // Several `{text}` items join with a space.
    let joined = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":[{"text":"one"},{"text":"two"}]}]}}"#;
    assert_eq!(render_transcript(joined), vec!["  ⎿ one two".to_string()]);

    // Whitespace-only content is also nothing to show.
    let blank =
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"   \n  "}]}}"#;
    assert!(render_transcript(blank).is_empty());
}

/// A `result` event with no text still says which way the run went.
///
/// `✔`/`✖` is the only thing a reader has when the payload is empty, so the fallback words matter:
/// a blank success must not render as a bare tick with nothing after it.
#[test]
fn a_result_event_with_no_text_still_names_its_outcome() {
    let blank_success = r#"{"type":"result","subtype":"success","result":""}"#;
    assert_eq!(render_transcript(blank_success), vec!["✔ done".to_string()]);
    let no_result_field = r#"{"type":"result","subtype":"success"}"#;
    assert_eq!(
        render_transcript(no_result_field),
        vec!["✔ done".to_string()]
    );

    let blank_error = r#"{"type":"result","is_error":true,"error":"  \n "}"#;
    assert_eq!(render_transcript(blank_error), vec!["✖ error".to_string()]);
    let no_message = r#"{"type":"result","is_error":true}"#;
    assert_eq!(render_transcript(no_message), vec!["✖ error".to_string()]);
    // An error whose text lives in `message` rather than `error`.
    let via_message = r#"{"type":"result","is_error":true,"message":"rate limited"}"#;
    assert_eq!(
        render_transcript(via_message),
        vec!["✖ rate limited".to_string()]
    );
}

/// A codex `command_execution` names the command from `text` when it has no `command` field, and an
/// item this renderer does not understand contributes nothing.
#[test]
fn a_codex_command_item_reads_either_field_and_unknown_items_are_skipped() {
    let via_text =
        r#"{"type":"item.completed","item":{"type":"command_execution","text":"cargo test"}}"#;
    assert_eq!(
        render_transcript(via_text),
        vec!["→ Tool: cargo test".to_string()]
    );
    // Neither field: nothing to say about it.
    let neither = r#"{"type":"item.completed","item":{"type":"command_execution"}}"#;
    assert!(render_transcript(neither).is_empty());
    // An item type this renderer has no rule for, and an event with no `item` at all.
    let unknown = r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#;
    assert!(render_transcript(unknown).is_empty());
    let itemless = r#"{"type":"item.completed"}"#;
    assert!(render_transcript(itemless).is_empty());
    // A codex agent_message whose lines are all blank.
    let blank = r#"{"type":"item.completed","item":{"type":"agent_message","text":"  \n\n "}}"#;
    assert!(render_transcript(blank).is_empty());
}

/// An event whose `message.content` is missing or not an array renders nothing rather than panicking,
/// and a `text` block in a USER event is not prose (only assistant text is).
#[test]
fn a_malformed_message_renders_nothing_and_user_text_is_not_prose() {
    for raw in [
        r#"{"type":"assistant"}"#,
        r#"{"type":"assistant","message":{}}"#,
        r#"{"type":"assistant","message":{"content":"not an array"}}"#,
    ] {
        assert!(render_transcript(raw).is_empty(), "{raw}");
    }
    // `text` from the user side, and `tool_use` from the user side, are both ignored.
    let user_text = r#"{"type":"user","message":{"content":[{"type":"text","text":"a prompt"}]}}"#;
    assert!(render_transcript(user_text).is_empty());
    let user_tool = r#"{"type":"user","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
    assert!(render_transcript(user_tool).is_empty());
    // …and an assistant `tool_result` likewise.
    let assistant_result =
        r#"{"type":"assistant","message":{"content":[{"type":"tool_result","content":"out"}]}}"#;
    assert!(render_transcript(assistant_result).is_empty());
}
