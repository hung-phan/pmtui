//! The skill files themselves: where they are linked from, and what the harness writes.
//!
//! Moved out of `skills.rs`, beside this module's other tests. Wired with `#[path]` like its
//! siblings here, so the module keeps its name and nothing in the body changed.

use super::*;

// The skill loader parses the frontmatter as YAML. A prose `description:` invites the classic
// "mapping values are not allowed in this context" trap: an UNQUOTED value that itself contains
// a colon-space ("the operating rules: you reach ...") is read as a nested mapping and the whole
// skill fails to load. There is no YAML parser in this crate, so pin the one rule that bites:
// every frontmatter value is either quoted or free of a `": "`. This is the guard that would
// have caught the worker skill shipping with an unparseable description.
#[test]
fn embedded_skill_frontmatter_has_no_unquoted_colon_in_a_value() {
    for (name, md) in [
        ("worker", WORKER_SKILL_MD),
        ("decider", DECIDER_SKILL_MD),
        ("spawn", SPAWN_SKILL_MD),
    ] {
        let mut lines = md.lines();
        assert_eq!(
            lines.next(),
            Some("---"),
            "{name}: frontmatter opens with ---"
        );
        for line in lines {
            if line == "---" {
                break; // end of frontmatter
            }
            // Only `key: value` lines carry a value to check; skip blanks / mapping headers.
            let Some((_key, value)) = line.split_once(": ") else {
                continue;
            };
            let quoted = (value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"'));
            assert!(
                quoted || !value.contains(": "),
                "{name}: an unquoted colon-space in a frontmatter value breaks YAML \
                 (\"mapping values are not allowed\") — remove the colon or quote the value: \
                 {line}"
            );
        }
    }
}

// NON-VACUITY: this fails today because `WORKER_SKILL_MD` does not exist / is empty. It
// pins that EVERY non-negotiable operating rule and the FULL machine-report schema that
// Milestone E removes from the nudge actually survives in the skill body — so relocating
// the protocol out of the nudge cannot silently lose a rule.
#[test]
fn worker_skill_carries_the_non_negotiables_and_the_full_schema() {
    let s = WORKER_SKILL_MD;
    // Frontmatter so claude can auto-invoke on the trigger.
    assert!(s.starts_with("---\n"), "has YAML frontmatter");
    assert!(
        s.contains("description:"),
        "has a description for auto-invocation"
    );
    // Comms ownership — the non-negotiable rule that must never be lost. The harness is
    // the channel; NO external integration (e.g. a Slack MCP) may be assumed.
    assert!(s.contains("sends no messages on your behalf"));
    assert!(s.contains("channel to the human"));
    assert!(
        !s.contains("Slack MCP"),
        "the worker skill must not assume a Slack MCP integration exists"
    );
    assert!(
        !s.contains("own all communication"),
        "neither the body nor the frontmatter description may reinstate the 'agent owns \
         comms' framing — the harness is the channel"
    );
    assert!(
        s.contains("working toward the session goal on a heartbeat"),
        "the worker skill must frame the whole recorded brief without implying one atomic \
         objective"
    );
    assert!(
        !s.contains("working ONE goal"),
        "a goal brief may contain multiple objectives"
    );
    assert!(
        s.contains("the whole goal is now SATISFIED"),
        "completion must cover the whole recorded brief"
    );
    assert!(
        !s.contains("the goal's rule"),
        "completion must not imply one atomic rule"
    );
    // You do not decide when done — verbatim invariants relocated from the nudge.
    assert!(s.contains("There is no \"done\" you can set"));
    assert!(s.contains("A human decides when this session is complete"));
    assert!(s.contains("never mark the project finished yourself"));
    assert!(s.contains("or abandon it"));
    assert!(s.contains("Ending a finite wake"));
    // The confirm_done sanction (moved out of the nudge).
    assert!(s.contains("confirm_done"));
    assert!(s.contains("\"state\": \"blocked\""));
    assert!(s.contains("REQUEST FOR CONFIRMATION"));
    // The FULL WakeReport schema fields (moved out of the nudge).
    assert!(s.contains("\"seq\""));
    assert!(s.contains("\"next_step\""));
    assert!(s.contains("\"cadence_s\""));
    assert!(s.contains("\"next_check_s\""));
    assert!(s.contains("working") && s.contains("monitoring") && s.contains("blocked"));
    // Atomicity + audit-only seq + codex conversation_id rules.
    assert!(s.contains("tmp") && s.contains("rename"));
    assert!(s.contains("diagnostic stamp"));
    assert!(s.contains("assigns its own report generation"));
    assert!(s.contains("backward timestamp cannot block"));
    assert!(s.contains("conversation_id"));
    // Pick up your own plan.
    assert!(s.contains("your OWN plan"));
    // The skill must NOT hardcode a marker path (that is per-session, supplied by the nudge).
    assert!(
        !s.contains("/.project-state/"),
        "no baked-in per-session path"
    );
}

#[test]
fn worker_skill_name_and_relpath_are_stable() {
    assert_eq!(WORKER_SKILL_NAME, "agent-manager-worker");
    assert_eq!(
        WORKER_SKILL_REL_PATH,
        ".agents/skills/agent-manager-worker/SKILL.md"
    );
    assert_eq!(
        CODEX_WORKER_SKILL_REL_PATH,
        ".agents/skills/agent-manager-worker/SKILL.md"
    );
    assert_eq!(CLAUDE_SKILLS_REL_PATH, ".claude/skills");
    assert_eq!(
        CLAUDE_WORKER_SKILL_LINK_REL_PATH,
        ".claude/skills/agent-manager-worker"
    );
    assert_eq!(
        CLAUDE_WORKER_SKILL_LINK_TARGET,
        "../../.agents/skills/agent-manager-worker"
    );
}

#[test]
fn decider_skill_formalizes_the_consult_ladder() {
    let s = DECIDER_SKILL_MD;
    assert!(s.starts_with("---\n"), "has YAML frontmatter");
    assert!(s.contains("Auto-approve (act)"));
    assert!(s.contains("refuse"));
    assert!(s.contains("has NOT approved"));
    assert!(s.contains("validated verdict is required"));
    assert!(!s.contains("ALREADY approved"));
    assert!(
        s.contains("SITUATION"),
        "explains how to read the C situation block"
    );
    // verify-don't-defer-to-precedent (C's core anti-rubber-stamp rule).
    assert!(s.contains("verify"));
    assert!(s.contains("precedent"));
    // It must not weaken the read-only, two-writers rule.
    assert!(s.contains("READ-ONLY"));
    assert!(s.contains("cannot write"));
}
