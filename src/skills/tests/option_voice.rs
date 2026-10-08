use super::WORKER_SKILL_MD;

#[test]
fn worker_skill_defines_options_as_human_replies() {
    let skill = WORKER_SKILL_MD;
    let prose = skill.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(prose.contains("reply the human selects"));
    assert!(prose.contains("delivers back to you verbatim"));
    assert!(prose.contains("HUMAN'S point of view"));
    assert!(prose.contains("`I`/`me`/`my` means the human"));
    assert!(prose.contains("`you`/`your` means the worker"));
    assert!(prose.contains("Never use `I` in an option to mean the worker"));
    assert!(skill.contains("Continue by investigating the fallback"));
}
