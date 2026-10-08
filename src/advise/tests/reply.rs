use super::super::reply::MAX_SUPERVISOR_TEXT;
use super::super::{Grant, Refusal, Verdict, validate};
use super::{NONCE, consult_free, consult_with_options, envelope};

#[test]
fn selection_uses_the_harness_option_instead_of_supervisor_text() {
    let consult = consult_with_options();
    let raw = envelope(&format!(
        "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\"option_index\":1,\
         \"option\":\"rm -rf /\",\"reason\":\"dprint is already in the repo\"}}"
    ));

    assert_eq!(
        validate(&consult, &raw),
        Ok(Verdict::Select {
            index: 1,
            option: "dprint".to_string(),
            reason: "dprint is already in the repo".to_string(),
        })
    );
}

#[test]
fn free_answer_is_trimmed_and_keeps_its_reason() {
    let raw = envelope(&format!(
        "{{\"nonce\":\" {NONCE} \",\"action\":\" ANSWER \",\
         \"text\":\"  Use 'Migration notes' as the header.  \",\
         \"reason\":\"  matches the goal's docs wording  \"}}"
    ));

    assert_eq!(
        validate(&consult_free(), &raw),
        Ok(Verdict::Answer {
            text: "Use 'Migration notes' as the header.".to_string(),
            reason: "matches the goal's docs wording".to_string(),
        })
    );
}

#[test]
fn supported_bare_envelope_fenced_pretty_and_embedded_shapes_parse() {
    let consult = consult_with_options();
    let good = format!(
        "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\"option_index\":0,\
         \"reason\":\"r\"}}"
    );
    let good_value: serde_json::Value = serde_json::from_str(&good).expect("valid fixture");
    let cases = [
        (
            "structured output",
            format!("{{\"type\":\"result\",\"result\":\"ignored\",\"structured_output\":{good}}}"),
        ),
        ("bare object", good.clone()),
        ("fenced result", envelope(&format!("```json\n{good}\n```"))),
        (
            "noise around line",
            format!("warning: something\n{}\n\n", envelope(&good)),
        ),
        (
            "pretty envelope",
            format!("{{\n  \"type\": \"result\",\n  \"structured_output\": {good}\n}}\n"),
        ),
        (
            "result object",
            serde_json::json!({"result": good_value}).to_string(),
        ),
        (
            "prose around result object",
            envelope(&format!("analysis before\n{good}\nanalysis after")),
        ),
    ];

    for (name, raw) in cases {
        assert!(
            matches!(
                validate(&consult, &raw),
                Ok(Verdict::Select { index: 0, .. })
            ),
            "{name} should parse, got {:?}",
            validate(&consult, &raw)
        );
    }
}

#[test]
fn malformed_or_ungranted_reply_shapes_refuse_without_panicking() {
    let consult = consult_with_options();
    let cases = [
        ("empty", String::new(), Refusal::NoJson),
        (
            "prose",
            "I think option 1 is best.".to_string(),
            Refusal::NoJson,
        ),
        (
            "truncated json",
            format!("{{\"nonce\":\"{NONCE}\",\"action\":\"sel"),
            Refusal::NoJson,
        ),
        ("array", "[1,2,3]".to_string(), Refusal::NoJson),
        (
            "unrelated object",
            "{\"hello\":\"world\"}".to_string(),
            Refusal::NoJson,
        ),
        (
            "missing nonce",
            "{\"action\":\"select_option\",\"option_index\":0,\"reason\":\"r\"}".to_string(),
            Refusal::MissingNonce,
        ),
        (
            "wrong nonce",
            "{\"nonce\":\"guessed\",\"action\":\"select_option\",\"option_index\":0,\
             \"reason\":\"r\"}"
                .to_string(),
            Refusal::WrongNonce {
                got: "guessed".to_string(),
            },
        ),
        (
            "non-string nonce",
            "{\"nonce\":42,\"action\":\"select_option\",\"option_index\":0,\"reason\":\"r\"}"
                .to_string(),
            Refusal::MissingNonce,
        ),
        (
            "unknown action",
            format!("{{\"nonce\":\"{NONCE}\",\"action\":\"approve_everything\",\"reason\":\"r\"}}"),
            Refusal::UnknownAction {
                got: "approve_everything".to_string(),
            },
        ),
        (
            "missing action",
            format!("{{\"nonce\":\"{NONCE}\",\"reason\":\"r\"}}"),
            Refusal::UnknownAction { got: String::new() },
        ),
        (
            "missing index",
            format!("{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\"reason\":\"r\"}}"),
            Refusal::MissingIndex,
        ),
        (
            "float index",
            format!(
                "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\
                 \"option_index\":1.5,\"reason\":\"r\"}}"
            ),
            Refusal::MissingIndex,
        ),
        (
            "string index",
            format!(
                "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\
                 \"option_index\":\"1\",\"reason\":\"r\"}}"
            ),
            Refusal::MissingIndex,
        ),
        (
            "high index",
            format!(
                "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\
                 \"option_index\":2,\"reason\":\"r\"}}"
            ),
            Refusal::IndexOutOfRange { got: 2, options: 2 },
        ),
        (
            "negative index",
            format!(
                "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\
                 \"option_index\":-1,\"reason\":\"r\"}}"
            ),
            Refusal::IndexOutOfRange {
                got: -1,
                options: 2,
            },
        ),
        (
            "free text outside option grant",
            format!(
                "{{\"nonce\":\"{NONCE}\",\"action\":\"answer\",\
                 \"text\":\"do whatever you like\",\"reason\":\"r\"}}"
            ),
            Refusal::OutsideGrant {
                action: "answer".to_string(),
                grant: Grant::PickOption,
            },
        ),
    ];

    for (name, raw, expected) in cases {
        assert_eq!(
            validate(&consult, &raw),
            Err(expected),
            "unexpected result for {name}"
        );
    }
}

#[test]
fn selecting_without_options_is_outside_the_free_answer_grant() {
    let raw = format!(
        "{{\"nonce\":\"{NONCE}\",\"action\":\"select_option\",\"option_index\":0,\
         \"reason\":\"r\"}}"
    );
    assert_eq!(
        validate(&consult_free(), &raw),
        Err(Refusal::OutsideGrant {
            action: "select_option".to_string(),
            grant: Grant::FreeAnswer,
        })
    );
}

#[test]
fn control_bytes_are_rejected_in_answer_and_reason_fields() {
    for payload in [
        "ok\u{1b}[Z",
        "ok\ryes",
        "ok\nyes",
        "ok\tyes",
        "ok\u{7f}",
        "ok\u{9b}2J",
        "ok\u{0}",
    ] {
        let in_text = serde_json::json!({
            "nonce": NONCE, "action": "answer", "text": payload, "reason": "r"
        })
        .to_string();
        assert_eq!(
            validate(&consult_free(), &in_text),
            Err(Refusal::ControlBytes {
                field: "answer".to_string(),
            })
        );

        let in_reason = serde_json::json!({
            "nonce": NONCE, "action": "answer", "text": "fine", "reason": payload
        })
        .to_string();
        assert_eq!(
            validate(&consult_free(), &in_reason),
            Err(Refusal::ControlBytes {
                field: "reason".to_string(),
            })
        );
    }
}

#[test]
fn supervisor_text_accepts_the_cap_and_rejects_overruns_by_character_count() {
    let at_cap = "é".repeat(MAX_SUPERVISOR_TEXT);
    let accepted = serde_json::json!({
        "nonce": NONCE, "action": "answer", "text": at_cap, "reason": "r"
    })
    .to_string();
    assert!(matches!(
        validate(&consult_free(), &accepted),
        Ok(Verdict::Answer { .. })
    ));

    let too_long = "é".repeat(MAX_SUPERVISOR_TEXT + 1);
    for (field, raw) in [
        (
            "answer",
            serde_json::json!({
                "nonce": NONCE, "action": "answer", "text": too_long, "reason": "r"
            })
            .to_string(),
        ),
        (
            "reason",
            serde_json::json!({
                "nonce": NONCE, "action": "answer", "text": "fine", "reason": too_long
            })
            .to_string(),
        ),
    ] {
        assert_eq!(
            validate(&consult_free(), &raw),
            Err(Refusal::TooLong {
                field: field.to_string(),
                chars: MAX_SUPERVISOR_TEXT + 1,
            })
        );
    }
}

#[test]
fn absent_or_blank_answer_text_is_refused() {
    for text in [None, Some(""), Some("   ")] {
        let mut value = serde_json::json!({
            "nonce": NONCE, "action": "answer", "reason": "r"
        });
        if let Some(text) = text {
            value["text"] = text.into();
        }
        assert_eq!(
            validate(&consult_free(), &value.to_string()),
            Err(Refusal::EmptyText)
        );
    }
}

#[test]
fn explicit_refusal_preserves_reason_or_supplies_a_human_readable_default() {
    let with_reason = serde_json::json!({
        "nonce": NONCE, "action": "refuse", "reason": "the goal is silent"
    })
    .to_string();
    assert_eq!(
        validate(&consult_free(), &with_reason),
        Err(Refusal::SupervisorRefused {
            reason: "the goal is silent".to_string(),
        })
    );

    let without_reason = serde_json::json!({
        "nonce": NONCE, "action": "refuse"
    })
    .to_string();
    assert_eq!(
        validate(&consult_free(), &without_reason),
        Err(Refusal::SupervisorRefused {
            reason: "no reason given".to_string(),
        })
    );
}

#[test]
fn malformed_brace_order_and_invalid_nested_results_are_no_json() {
    for raw in [
        "}{".to_string(),
        envelope("}{"),
        envelope("prefix {not-json} suffix"),
        "{\"result\":\"not json\"}".to_string(),
    ] {
        assert_eq!(validate(&consult_free(), &raw), Err(Refusal::NoJson));
    }
}

#[test]
fn refusal_display_covers_validation_and_operational_failures() {
    let cases = [
        (Refusal::NoJson, "no parseable JSON"),
        (Refusal::MissingNonce, "carried no nonce"),
        (
            Refusal::WrongNonce {
                got: "stale".to_string(),
            },
            "wrong nonce",
        ),
        (
            Refusal::UnknownAction {
                got: "approve".to_string(),
            },
            "unknown action",
        ),
        (
            Refusal::OutsideGrant {
                action: "answer".to_string(),
                grant: Grant::PickOption,
            },
            "outside this decision's grant",
        ),
        (Refusal::MissingIndex, "no usable option_index"),
        (
            Refusal::IndexOutOfRange { got: 4, options: 2 },
            "only 2 options exist",
        ),
        (Refusal::EmptyText, "answer text was empty"),
        (
            Refusal::TooLong {
                field: "answer".to_string(),
                chars: 401,
            },
            "401 characters",
        ),
        (
            Refusal::ControlBytes {
                field: "reason".to_string(),
            },
            "control/escape bytes",
        ),
        (
            Refusal::SupervisorRefused {
                reason: "not enough context".to_string(),
            },
            "not enough context",
        ),
        (
            Refusal::NoResult {
                detail: "timed out".to_string(),
            },
            "timed out",
        ),
        (Refusal::NoProgress, "unchanged pane"),
    ];

    for (refusal, expected) in cases {
        let rendered = refusal.to_string();
        assert!(
            rendered.contains(expected),
            "{refusal:?} rendered as {rendered:?}"
        );
    }
}
