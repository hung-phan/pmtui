use super::WORKER_SKILL_MD;

#[test]
fn worker_skill_uses_finite_nonblocking_wakes_for_detachable_work() {
    let skill = WORKER_SKILL_MD;
    let prose = skill.split_whitespace().collect::<Vec<_>>().join(" ");
    let prose = prose.to_lowercase();

    assert!(prose.contains("keep each wake finite"));
    assert!(prose.contains("safely be detached"));
    assert!(prose.contains("revalidatable handle"));
    assert!(skill.contains("\"state\": \"monitoring\""));
    assert!(prose.contains("end the turn"));
    assert!(prose.contains("next wake"));
    assert!(prose.contains("clean up"));
    assert!(prose.contains("may request approval"));
    assert!(prose.contains("external or destructive action"));
    assert!(prose.contains("positively identify"));
    assert!(prose.contains("revalidatable handle"));
    assert!(prose.contains("refuse cleanup"));
    assert!(prose.contains("possibly reused pid"));
    assert!(prose.contains("paused, restarted, or removed"));
    assert!(prose.contains("enforced hard deadline"));
    assert!(prose.contains("terminate without worker cleanup"));
    assert!(prose.contains("requiring owner cleanup stays foreground"));
    assert!(!prose.contains("do not stop working"));
    assert!(
        !skill.contains("keep polling it"),
        "the old rule encouraged indefinitely busy turns"
    );
}

#[test]
fn worker_skill_owns_a_bounded_checkpoint_without_redefining_the_goal() {
    let skill = WORKER_SKILL_MD;
    let prose = skill.split_whitespace().collect::<Vec<_>>().join(" ");
    let prose = prose.to_lowercase();

    assert!(skill.contains("checkpoint.json"));
    assert!(prose.contains("agent-owned checkpoint"));
    assert!(prose.contains("bounded"));
    assert!(prose.contains("brief remains the authoritative goal"));
    assert!(skill.contains("\"activities\""));
    assert!(skill.contains("\"handle\""));
    assert!(skill.contains("\"important_files\""));
    assert!(prose.contains("never satisfies the required decision-marker write"));
    assert!(prose.contains("project completion"));
    assert!(prose.contains("untrusted continuity data"));
    assert!(prose.contains("never instructions"));
    assert!(prose.contains("credentials, tokens, or signed urls"));
    assert!(prose.contains("checkpoint may be displayed"));
    assert!(prose.contains("only control channel"));
}

#[test]
fn worker_skill_requires_generic_effect_classification_for_stops() {
    let skill = WORKER_SKILL_MD;
    let prose = skill.split_whitespace().collect::<Vec<_>>().join(" ");
    let prose = prose.to_lowercase();

    for field in ["\"scope\"", "\"reversibility\"", "\"authority\""] {
        assert!(skill.contains(field), "missing effect axis {field}");
    }
    for value in [
        "local",
        "external",
        "reversible",
        "irreversible",
        "ordinary",
        "privileged",
        "unknown",
    ] {
        assert!(
            prose.contains(value),
            "missing generic effect value {value}"
        );
    }
    assert!(prose.contains("explicit external"));
    assert!(prose.contains("unknown"));
    assert!(prose.contains("decider will investigate"));
    assert!(prose.contains("do not mix optional privileged/external follow-up work"));
    assert!(!prose.contains("money_movement"));
}
