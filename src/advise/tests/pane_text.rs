use super::super::{
    Consult, Refusal, Verdict, apply_text, has_forbidden_control_bytes, sanitize_control_bytes,
    text_hash,
};
use super::{NONCE, consult_free, consult_with_options};

#[test]
fn forbidden_controls_cover_c0_del_and_c1_but_not_printable_unicode() {
    for (name, payload) in [
        ("escape", "ok\u{1b}[Z"),
        ("carriage return", "ok\ryes"),
        ("newline", "ok\nyes"),
        ("tab", "ok\tyes"),
        ("delete", "ok\u{7f}"),
        ("c1 csi", "ok\u{9b}2J"),
        ("nul", "ok\u{0}"),
    ] {
        assert!(
            has_forbidden_control_bytes(payload),
            "{name} must be rejected"
        );
    }
    assert!(!has_forbidden_control_bytes(
        "plain text with spaces — and ünïcode"
    ));
}

#[test]
fn sanitizer_replaces_controls_and_collapses_whitespace() {
    assert_eq!(
        sanitize_control_bytes("  alpha\t\u{1b}[Z\r\n beta\u{7f}  "),
        "alpha [Z beta"
    );
}

#[test]
fn worker_text_is_sanitized_before_it_is_quoted_in_the_pane() {
    let consult = Consult {
        nonce: NONCE.to_string(),
        goal: "g".to_string(),
        question: "which\u{1b}[Z \"one\"\r\n?".to_string(),
        options: vec!["a\u{7f}lpha".to_string(), "beta".to_string()],
        reported_effect: None,
        situation: String::new(),
        directive: String::new(),
    };
    let verdict = Verdict::Select {
        index: 0,
        option: consult.options[0].clone(),
        reason: "alpha suits the goal".to_string(),
    };

    let text = apply_text(&consult, &verdict).expect("safe worker text assembles");
    assert!(!has_forbidden_control_bytes(&text));
    assert!(text.contains("which [Z 'one' ?"), "{text}");
    assert!(text.contains("a lpha"), "{text}");
}

#[test]
fn quoted_worker_text_is_character_truncated_with_an_ellipsis() {
    let mut consult = consult_with_options();
    consult.question = format!("{}QUESTION-TAIL", "é".repeat(301));
    let verdict = Verdict::Select {
        index: 0,
        option: format!("{}OPTION-TAIL", "ø".repeat(301)),
        reason: "bounded".to_string(),
    };

    let text = apply_text(&consult, &verdict).expect("bounded text assembles");
    assert!(text.contains(&format!("{}…", "é".repeat(300))), "{text}");
    assert!(text.contains(&format!("{}…", "ø".repeat(300))), "{text}");
    assert!(!text.contains("QUESTION-TAIL"), "{text}");
    assert!(!text.contains("OPTION-TAIL"), "{text}");
}

#[test]
fn validated_answer_and_selection_render_as_single_actionable_lines() {
    let answer = Verdict::Answer {
        text: "Use 'Migration notes' as the header.".to_string(),
        reason: "matches the goal".to_string(),
    };
    let answer_text = apply_text(&consult_free(), &answer).expect("answer assembles");
    assert!(answer_text.contains("Migration notes"), "{answer_text}");
    assert!(answer_text.contains("do not re-ask"), "{answer_text}");

    let selection = Verdict::Select {
        index: 1,
        option: "dprint".to_string(),
        reason: "already configured".to_string(),
    };
    let selection_text =
        apply_text(&consult_with_options(), &selection).expect("selection assembles");
    assert!(
        selection_text.contains("option 2 — dprint"),
        "{selection_text}"
    );
}

#[test]
fn final_assembly_rejects_controls_even_if_a_verdict_bypasses_validation() {
    let verdict = Verdict::Answer {
        text: "first line\nsecond line".to_string(),
        reason: "unsafe direct construction".to_string(),
    };
    assert_eq!(
        apply_text(&consult_free(), &verdict),
        Err(Refusal::ControlBytes {
            field: "assembled decision".to_string(),
        })
    );
}

#[test]
fn pane_hash_is_stable_and_byte_sensitive() {
    assert_eq!(text_hash("same"), text_hash("same"));
    assert_ne!(text_hash("pane a"), text_hash("pane b"));
    assert_ne!(text_hash(""), text_hash(" "));
}
