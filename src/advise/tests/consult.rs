use super::super::consult::{
    MAX_CONSULT_DATA_BYTES, MAX_CONSULT_OPTIONS, MAX_DIRECTIVE_BYTES, MAX_GOAL_BYTES,
    MAX_SITUATION_BYTES,
};
use super::super::{Consult, Grant, clamp_directive, clamp_goal, clamp_situation};
use super::{consult_free, consult_with_options};

#[test]
fn grant_is_derived_from_the_consult_options() {
    assert_eq!(consult_with_options().grant(), Grant::PickOption);
    assert_eq!(consult_free().grant(), Grant::FreeAnswer);
}

#[test]
fn consultability_rejects_empty_oversized_and_over_budget_questions() {
    let base = consult_free();
    let blank = Consult {
        question: "   ".to_string(),
        ..base.clone()
    };
    assert!(!blank.is_consultable());

    let many = Consult {
        options: (0..MAX_CONSULT_OPTIONS + 1)
            .map(|i| i.to_string())
            .collect(),
        ..base.clone()
    };
    assert!(!many.is_consultable());

    let huge = Consult {
        question: "q".repeat(MAX_CONSULT_DATA_BYTES + 1),
        ..base.clone()
    };
    assert!(!huge.is_consultable());

    assert!(base.is_consultable());
    assert!(consult_with_options().is_consultable());
    assert!(
        Consult {
            question: String::new(),
            options: vec!["a".into(), "b".into()],
            ..base
        }
        .is_consultable(),
        "enumerated options remain a real question even when the prompt is blank"
    );
}

#[test]
fn goal_bytes_are_budgeted_and_clamping_restores_consultability() {
    let fat = "g".repeat(MAX_CONSULT_DATA_BYTES * 4);
    assert!(
        !Consult {
            goal: fat.clone(),
            ..consult_free()
        }
        .is_consultable()
    );

    let clamped = clamp_goal(&fat);
    assert!(clamped.len() < fat.len());
    assert!(clamped.contains("truncated"), "{clamped}");
    assert!(
        Consult {
            goal: clamped,
            ..consult_free()
        }
        .is_consultable()
    );
    assert_eq!(clamp_goal("ship the uploader"), "ship the uploader");
}

#[test]
fn situation_is_additive_but_budgeted_fields_still_enforce_the_limit() {
    let mut at_budget = consult_free();
    at_budget.question = "q".repeat(MAX_CONSULT_DATA_BYTES - at_budget.goal.len());
    assert!(at_budget.is_consultable());

    at_budget.situation = "s".repeat(MAX_SITUATION_BYTES);
    assert!(
        at_budget.is_consultable(),
        "situation context must not disable an otherwise consultable question"
    );

    at_budget.question.push('x');
    assert!(!at_budget.is_consultable());
}

#[test]
fn situation_clamp_keeps_the_recent_head_and_announces_the_cut() {
    assert_eq!(clamp_situation("- recent: ok"), "- recent: ok");

    let mut block = String::from("- NEWEST-SENTINEL\n");
    block.push_str(&"- filler line to push the block well past the cap\n".repeat(200));
    block.push_str("- OLDEST-SENTINEL");
    let clamped = clamp_situation(&block);

    assert!(clamped.contains("NEWEST-SENTINEL"));
    assert!(!clamped.contains("OLDEST-SENTINEL"));
    assert!(clamped.contains("truncated"), "{clamped}");
}

#[test]
fn directive_is_additive_and_clamped_with_an_announcement() {
    let long = "x".repeat(MAX_DIRECTIVE_BYTES + 50);
    let clamped = clamp_directive(&long);
    assert!(clamped.len() > MAX_DIRECTIVE_BYTES);
    assert!(clamped.contains("[the directive was truncated here"));
    assert_eq!(
        clamp_directive("stop auto-approving test edits"),
        "stop auto-approving test edits"
    );

    let consult = Consult {
        nonce: "n".into(),
        goal: "g".repeat(MAX_CONSULT_DATA_BYTES - 10),
        question: "proceed?".into(),
        options: vec![],
        reported_effect: None,
        situation: String::new(),
        directive: "no.".repeat(MAX_DIRECTIVE_BYTES / 3),
    };
    assert!(
        consult.is_consultable(),
        "directive must not count toward the consult data budget"
    );
}

#[test]
fn clamps_back_up_to_a_valid_utf8_boundary() {
    fn crossing(limit: usize) -> String {
        format!("a{}", "é".repeat(limit))
    }

    for (limit, clamped) in [
        (MAX_GOAL_BYTES, clamp_goal(&crossing(MAX_GOAL_BYTES))),
        (
            MAX_SITUATION_BYTES,
            clamp_situation(&crossing(MAX_SITUATION_BYTES)),
        ),
        (
            MAX_DIRECTIVE_BYTES,
            clamp_directive(&crossing(MAX_DIRECTIVE_BYTES)),
        ),
    ] {
        let visible = clamped
            .split_once("\n\n[")
            .map(|(head, _)| head)
            .expect("an over-limit value announces truncation");
        assert_eq!(visible.len(), limit - 1);
        assert!(visible.ends_with('é'));
    }
}
