use super::*;

#[test]
fn body_numbers_the_offered_options_and_is_unchanged_without_them() {
    // With options the human gets the choices they can reply with; without them the
    // body is byte-identical to before this field existed.
    let plain = stop("s1", "publish", RiskClass::Hard);
    assert_eq!(
        Escalation::for_stops("p", std::slice::from_ref(&plain)).body,
        "[s1] which path?"
    );
    let mut with_opts = plain;
    with_opts.options = vec!["ship".into(), " hold ".into()];
    assert_eq!(
        Escalation::for_stops("p", &[with_opts]).body,
        "[s1] which path?\n    1) ship  2) hold"
    );
}

#[test]
fn severity_takes_the_loudest_stop() {
    assert_eq!(severity_for_stops(&[]), Severity::Info);
    assert_eq!(
        severity_for_stops(&[
            stop("a", "ambiguity", RiskClass::Low),
            stop("b", "ambiguity", RiskClass::Medium)
        ]),
        Severity::Warn
    );
    // publish is forced hard -> urgent, even labelled low
    assert_eq!(
        severity_for_stops(&[stop("a", "publish", RiskClass::Low)]),
        Severity::Urgent
    );
}

#[test]
fn escalation_message_lists_stops() {
    let e = Escalation::for_stops("proj", &[stop("s1", "ambiguity", RiskClass::Medium)]);
    assert_eq!(e.title, "1 decision needs you");
    assert!(e.body.contains("[s1] which path?"));
    assert_eq!(e.severity, Severity::Warn);
}

#[test]
fn escalation_message_pluralizes_and_lists_both() {
    let e = Escalation::for_stops(
        "proj",
        &[
            stop("s1", "ambiguity", RiskClass::Medium),
            stop("s2", "publish", RiskClass::Low),
        ],
    );
    assert_eq!(e.title, "2 decisions need you");
    assert!(e.body.contains("[s1]"), "{}", e.body);
    assert!(e.body.contains("[s2]"), "{}", e.body);
    assert_eq!(e.severity, Severity::Urgent, "publish forces urgent");
}

/// A text-less stop must NOT notify a human with a bare kind name.
///
/// This test previously asserted the opposite -- it encoded the bug. `for_stops`
/// builds the message that LEAVES the machine, so `[s1] confirm_done` was the worst
/// of the three stop surfaces to say nothing on: a human got a desktop notification
/// naming an enum variant. It now shares `state::stop_product_text` with pmtui's
/// `Stops` block and answer overlay, so all three describe the same decision the same
/// way.
#[test]
fn a_text_less_synthesized_stop_notifies_with_words_not_a_bare_kind() {
    for (kind, want) in [
        ("confirm_done", "goal is met"),
        ("capability", "Needs your decision"),
        ("stuck", "No progress"),
    ] {
        let mut s = stop("s1", kind, RiskClass::Hard);
        s.question = String::new();
        let e = Escalation::for_stops("proj", &[s]);
        assert!(
            e.body.contains(want),
            "{kind}: a human must get words, not an enum variant: {}",
            e.body
        );
        assert!(
            !e.body.contains(&format!("[s1] {kind}")),
            "{kind}: the bare-kind body is what this test exists to prevent: {}",
            e.body
        );
    }
}

/// The other half, and the reason the fallback is not simply deleted: for a kind
/// the harness never synthesizes, a missing question means the agent dropped it,
/// and no fixed sentence could say what would be published. The bare kind is then
/// the only honest thing left.
#[test]
fn an_agent_authored_kind_with_no_question_still_falls_back_to_the_kind() {
    let mut s = stop("s1", "publish", RiskClass::Hard);
    s.question = String::new();
    let e = Escalation::for_stops("proj", &[s]);
    assert!(e.body.contains("[s1] publish"), "{}", e.body);
}
