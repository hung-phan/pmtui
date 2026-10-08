//! Reading a job child's result out of what the harness left behind.
//!
//! The payloads here are the event shapes both CLIs document: claude's `result` event (with and
//! without `structured_output`) and codex's `item.completed` / `turn.completed` stream plus its `-o`
//! file. What is OURS is the reading of them — which field decides the outcome, last-valid-wins, and
//! the refusal to guess at anything malformed.

use crate::registry::Engine;
use crate::spawn::{JobOutcome, codex_thread_id, read_job_result, result_schema_json};

/// A claude `result` line carrying our schema in `structured_output`.
fn claude_structured(outcome: &str, summary: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","result":"prose","structured_output":{{"outcome":"{outcome}","summary":"{summary}"}}}}"#
    )
}

#[test]
fn claudes_structured_output_is_the_result() {
    let log = format!(
        "{}\n{}\n",
        r#"{"type":"system","subtype":"init"}"#,
        claude_structured("needs_human", "the migration needs a DBA")
    );
    let got = read_job_result(Engine::Claude, &log, None).expect("a result");
    assert_eq!(got.outcome, JobOutcome::NeedsHuman);
    assert_eq!(got.summary, "the migration needs a DBA");
    assert_eq!(got.detail, None);
}

/// LAST VALID WINS. A run that reported once and kept going must report its FINAL outcome — the rule
/// agent-deck's `ScanDoneSentinel` learned ("a worker that retried (printing fail then ok)").
#[test]
fn the_last_result_event_wins() {
    let log = format!(
        "{}\n{}\n{}\n",
        claude_structured("failed", "first attempt broke"),
        r#"{"type":"assistant","message":{"role":"assistant"}}"#,
        claude_structured("done", "second attempt fixed it")
    );
    let got = read_job_result(Engine::Claude, &log, None).expect("a result");
    assert_eq!(got.outcome, JobOutcome::Done);
    assert_eq!(got.summary, "second attempt fixed it");
}

/// With no `structured_output`, the event's own `subtype` still states how the run ended, and its
/// `result` text is the summary. That is a harness-stated fact, not a reading of the agent's prose.
#[test]
fn a_claude_run_without_structured_output_still_reports() {
    let ok = read_job_result(
        Engine::Claude,
        r#"{"type":"result","subtype":"success","result":"renamed the flag"}"#,
        None,
    )
    .expect("a result");
    assert_eq!(ok.outcome, JobOutcome::Done);
    assert_eq!(ok.summary, "renamed the flag");

    let bad = read_job_result(
        Engine::Claude,
        r#"{"type":"result","subtype":"error_max_turns","result":""}"#,
        None,
    )
    .expect("a result");
    assert_eq!(bad.outcome, JobOutcome::Failed);
    assert_eq!(
        bad.summary, "error_max_turns",
        "an empty text falls back to the subtype, which is what happened"
    );
}

/// A MALFORMED PAYLOAD IS REJECTED, NOT GUESSED AT (the other half of agent-deck's parse rule): a
/// `structured_output` that does not match the schema falls through to the subtype rather than being
/// half-read.
#[test]
fn a_malformed_structured_payload_falls_back_rather_than_guessing() {
    let log = r#"{"type":"result","subtype":"success","result":"did the thing","structured_output":{"outcome":"finished","summary":"wrong enum"}}"#;
    let got = read_job_result(Engine::Claude, log, None).expect("a result");
    assert_eq!(got.outcome, JobOutcome::Done, "from the subtype");
    assert_eq!(
        got.summary, "did the thing",
        "from the event, not the payload"
    );
}

/// No `result` event at all — a crash, a kill, a run that never reached its end — is NO result, which
/// the broker records as `ended_without_result`.
#[test]
fn a_truncated_claude_log_has_no_result() {
    assert!(
        read_job_result(
            Engine::Claude,
            "{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"assistant\"}\n",
            None
        )
        .is_none()
    );
    assert!(read_job_result(Engine::Claude, "", None).is_none());
    assert!(
        read_job_result(Engine::Claude, "not json at all\nnor this\n", None).is_none(),
        "a log of plain text is not a report"
    );
}

/// codex writes the schema straight into its `-o` file.
#[test]
fn codexs_output_file_is_the_result() {
    let got = read_job_result(
        Engine::Codex,
        r#"{"type":"turn.completed","usage":{}}"#,
        Some(
            r#"{"outcome":"failed","summary":"the fixture would not build","detail":"cargo said …"}"#,
        ),
    )
    .expect("a result");
    assert_eq!(got.outcome, JobOutcome::Failed);
    assert_eq!(got.summary, "the fixture would not build");
    assert_eq!(got.detail.as_deref(), Some("cargo said \u{2026}"));
}

/// Without a usable file, codex's TURN TERMINATOR decides the outcome and the last `agent_message`
/// supplies the summary.
#[test]
fn codex_falls_back_to_the_turn_terminator_and_last_message() {
    let log = concat!(
        r#"{"type":"thread.started"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"first pass"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","text":"ignored"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"all tests green"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":1}}"#,
        "\n"
    );
    let done = read_job_result(Engine::Codex, log, None).expect("a result");
    assert_eq!(done.outcome, JobOutcome::Done);
    assert_eq!(done.summary, "all tests green", "the LAST agent message");

    let failed_log = log.replace("turn.completed", "turn.failed");
    let failed = read_job_result(Engine::Codex, &failed_log, None).expect("a result");
    assert_eq!(failed.outcome, JobOutcome::Failed);
}

/// A FAILED TURN IS A FAILED RUN, whatever its `-o` file says. codex writes that file when the model's
/// final message lands, which is BEFORE the turn is over — a turn that fails afterwards leaves a
/// well-formed `done` file behind, and codex has shipped versions that then exit 0. Taking the file as
/// the commit signal would report that run as finished work.
#[test]
fn a_codex_turn_that_failed_after_writing_its_file_is_not_done() {
    let file = r#"{"outcome":"done","summary":"renamed the flag","detail":"and its test"}"#;
    let got = read_job_result(
        Engine::Codex,
        concat!(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"renamed the flag"}}"#,
            "\n",
            r#"{"type":"turn.failed","error":{"message":"the sandbox denied a write"}}"#
        ),
        Some(file),
    )
    .expect("a result");
    assert_eq!(got.outcome, JobOutcome::Failed, "the stream decides");
    assert_eq!(
        got.summary, "renamed the flag",
        "the file still supplies what the model said it did"
    );

    // And the same file after a turn that DID complete is taken as written: it is our schema, and the
    // outcome in it is the model's own.
    let done = read_job_result(Engine::Codex, r#"{"type":"turn.completed"}"#, Some(file))
        .expect("a result");
    assert_eq!(
        (done.outcome, done.detail.as_deref()),
        (JobOutcome::Done, Some("and its test"))
    );
}

/// AND NO TERMINATOR IS NO RESULT EVEN WITH A FILE: the run may have died mid-turn, so a row stays for
/// a human instead of claiming an outcome the stream never stated.
#[test]
fn a_codex_file_without_a_terminator_is_still_no_result() {
    assert!(
        read_job_result(
            Engine::Codex,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"working"}}"#,
            Some(r#"{"outcome":"done","summary":"renamed the flag"}"#),
        )
        .is_none(),
        "the stream never said the turn ended"
    );
}

/// THE THREAD ID CODEX GENERATED, from its own first event. codex takes no caller-chosen conversation
/// id, so this is the only record of which conversation a run created — and the only way a human can pick
/// up the conversation of a job that failed or asked for them.
#[test]
fn a_codex_stream_names_the_thread_it_started() {
    let log = concat!(
        r#"{"type":"thread.started","thread_id":"019bf0a2-7c3d-4e5f-8a9b-0c1d2e3f4a5b"}"#,
        "\n",
        r#"{"type":"thread.started","thread_id":"a-later-one-is-not-this-run"}"#,
        "\n",
        r#"{"type":"turn.completed"}"#
    );
    assert_eq!(
        codex_thread_id(log).as_deref(),
        Some("019bf0a2-7c3d-4e5f-8a9b-0c1d2e3f4a5b"),
        "the FIRST thread.started is this run's"
    );
    // The nested shape codex also emits, and every way a stream can fail to name one.
    assert_eq!(
        codex_thread_id(r#"{"type":"thread.started","thread":{"id":"t-1"}}"#).as_deref(),
        Some("t-1")
    );
    for silent in [
        "",
        "not json\n",
        r#"{"type":"turn.completed"}"#,
        r#"{"type":"thread.started"}"#,
        r#"{"type":"thread.started","thread_id":""}"#,
        r#"{"type":"thread.started","thread_id":7}"#,
    ] {
        assert!(
            codex_thread_id(silent).is_none(),
            "nothing to pin: {silent:?}"
        );
    }
}

/// A codex stream with no terminator is no result — same rule as claude's missing `result` event.
#[test]
fn an_unterminated_codex_turn_has_no_result() {
    let log = r#"{"type":"thread.started"}
{"type":"item.completed","item":{"type":"agent_message","text":"working"}}
"#;
    assert!(read_job_result(Engine::Codex, log, None).is_none());
    assert!(
        read_job_result(Engine::Codex, log, Some("   ")).is_none(),
        "an empty output file is not a report either"
    );
}

/// OVER-LONG TEXT IS TRUNCATED, NEVER DROPPED: a child that did the work must not lose its report to
/// a size rule, and a shortened field must not read as a complete one.
#[test]
fn long_fields_are_truncated_with_a_marker() {
    let long = "x".repeat(crate::spawn::SUMMARY_MAX * 2);
    let detail = "y".repeat(crate::spawn::DETAIL_MAX * 2);
    let file =
        serde_json::json!({"outcome": "done", "summary": long, "detail": detail}).to_string();
    let got = read_job_result(Engine::Codex, r#"{"type":"turn.completed"}"#, Some(&file))
        .expect("result");
    assert!(got.summary.len() <= crate::spawn::SUMMARY_MAX);
    assert!(got.summary.ends_with(crate::spawn::TRUNCATION_MARKER));
    let detail = got.detail.expect("detail kept");
    assert!(detail.len() <= crate::spawn::DETAIL_MAX);
    assert!(detail.ends_with(crate::spawn::TRUNCATION_MARKER));
}

/// Truncation lands on a CHARACTER boundary, so a multi-byte summary cannot be cut in half.
#[test]
fn truncation_never_splits_a_character() {
    let long = "\u{e9}".repeat(crate::spawn::SUMMARY_MAX);
    let file = serde_json::json!({"outcome": "done", "summary": long}).to_string();
    let got = read_job_result(Engine::Codex, r#"{"type":"turn.completed"}"#, Some(&file))
        .expect("result");
    assert!(got.summary.ends_with(crate::spawn::TRUNCATION_MARKER));
    assert!(
        got.summary.starts_with('\u{e9}'),
        "the kept prefix is whole characters: {}",
        &got.summary[..8]
    );
}

/// An empty summary is replaced rather than left blank: a receipt that says nothing is worse than one
/// that says the child reported nothing.
#[test]
fn an_empty_summary_becomes_a_stand_in() {
    let got = read_job_result(
        Engine::Codex,
        r#"{"type":"turn.completed"}"#,
        Some(r#"{"outcome":"done","summary":"   "}"#),
    )
    .expect("a result");
    assert_eq!(got.summary, "(the child reported no summary)");
}

/// The schema we hand both CLIs has to BE a schema, with our three outcome words and nothing else.
#[test]
fn the_result_schema_is_valid_and_closed() {
    let schema: serde_json::Value =
        serde_json::from_str(&result_schema_json()).expect("valid JSON");
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    let outcomes = schema["properties"]["outcome"]["enum"]
        .as_array()
        .expect("an enum of outcomes");
    assert_eq!(outcomes.len(), 3);
    for word in ["done", "needs_human", "failed"] {
        assert!(
            outcomes.iter().any(|v| v == word),
            "{word} missing from the schema"
        );
    }
    let required = schema["required"].as_array().expect("required list");
    assert!(required.iter().any(|v| v == "outcome") && required.iter().any(|v| v == "summary"));
}
