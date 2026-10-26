//! The coordinator skill: the standing loop that turns `pmtui spawn` into a queue a session runs
//! over many wakes.
//!
//! These pin the rules that were MEASURED rather than invented, and the boundaries that keep this
//! skill from repeating the two it composes.

use super::*;

const MD: &str = COORDINATOR_SKILL_MD;

/// The prose without fenced blocks, so a rule cannot be "present" only inside an example, with
/// whitespace collapsed because the file is hard-wrapped and a reflow must not break a test.
fn prose() -> String {
    MD.split("```")
        .step_by(2)
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_skill_is_addressable_and_scoped_away_from_its_neighbours() {
    assert!(MD.starts_with("---\n"), "has YAML frontmatter");
    assert!(
        MD.contains(&format!("\nname: {COORDINATOR_SKILL_NAME}\n")),
        "the frontmatter name is the skill's address"
    );
    let description = MD
        .lines()
        .find(|line| line.starts_with("description:"))
        .expect("a description");
    // Both confusions are live: doing the work itself, and treating peers as subordinates.
    assert!(
        description.contains("Not for doing the work yourself"),
        "it must say it coordinates rather than implements: {description}"
    );
    assert!(
        description.contains("a peer has its own human"),
        "it must not read as a licence to supervise other sessions: {description}"
    );
}

/// IT COMPOSES, IT DOES NOT REPEAT. The spawn skill already owns the Message spec, request ids,
/// the outcome table and cancelling; duplicating any of that here would make two sources of truth
/// that drift. This skill's content is the standing loop around it.
#[test]
fn it_defers_to_the_spawn_skill_instead_of_restating_it() {
    let prose = prose();
    assert!(
        prose.contains("pmtui-spawn"),
        "it must send the reader to the dispatch skill"
    );
    assert!(
        prose.contains("Read `pmtui-spawn` before your first dispatch"),
        "and say so before the first dispatch"
    );
    // The outcome/error vocabulary belongs to the spawn skill. A handful of state words are
    // unavoidable when saying where each one goes in the queue; the error CODES are not.
    for borrowed in [
        "request_conflict",
        "job_failed",
        "outcome_unknown",
        "--wait",
    ] {
        assert!(
            !MD.contains(borrowed),
            "{borrowed:?} belongs to the spawn skill; two copies will drift"
        );
    }
    assert!(
        prose.contains("Do not invent a retry policy here"),
        "retries are the spawn skill's rules, not this one's"
    );
}

/// THE MEASURED RULE. `stale_plan_streak` increments only when `status` AND `next_step` repeat
/// verbatim, and six in a row escalates the session to the human as stuck in a loop. A coordinator
/// that reports a static "waiting on children" at a short cadence trips that in minutes — so the
/// cadence advice is unusable without this rule beside it.
#[test]
fn it_ties_a_short_cadence_to_a_fresh_report() {
    let prose = prose();
    assert!(
        prose.contains("cadence_s"),
        "the control loop is the agent's own cadence"
    );
    assert!(
        prose.contains("your cadence sets how long a finished child waits for you"),
        "it must say why the cadence matters"
    );
    assert!(
        prose.contains("escalates you to the human as stuck in a loop"),
        "the hazard must be named, not implied"
    );
    assert!(
        prose.contains("always report new facts"),
        "and the rule that avoids it must be explicit"
    );
}

/// The queue has to survive compaction, and it is re-read into context on every reload — so both
/// halves have to be said: where it lives, and why it stays short.
#[test]
fn it_puts_the_queue_in_the_checkpoint_and_says_why_to_prune() {
    let prose = prose();
    for field in ["`next`", "`in_progress`", "`blockers`", "`important_files`"] {
        assert!(
            prose.contains(field),
            "the queue's fields must be named: {field}"
        );
    }
    assert!(
        prose.contains("printed back into your context on every reload"),
        "the token cost is the reason to prune, and must be stated"
    );
    assert!(
        prose.contains("request id"),
        "an in-flight child is tracked by its request id"
    );
}

/// DECOMPOSITION IS THE JOB. A child cannot ask a question and nobody is at its terminal, so
/// specification quality is the throughput limit. This is the one thing no other skill covers.
#[test]
fn it_says_how_to_split_work_and_when_not_to_dispatch() {
    let prose = prose();
    assert!(
        prose.contains("It cannot ask you a question"),
        "the constraint that makes specification the job"
    );
    assert!(
        prose.contains("owns its own files"),
        "two children in one file both commit and a person must pick"
    );
    assert!(
        prose.contains("names its own acceptance test"),
        "`done` has to mean something checkable"
    );
    assert!(
        prose.contains("is not ready to dispatch"),
        "work that needs a conversation must stay in the queue"
    );
}

/// The invariants this skill must not let a coordinator break: integration is the human's, and no
/// machine declares the project finished.
#[test]
fn it_keeps_integration_and_completion_with_the_human() {
    let prose = prose();
    assert!(
        prose.contains("not in the human's checkout"),
        "a child's commit is not applied"
    );
    assert!(
        prose.contains("apply it with `a`"),
        "the human's integration key must be named"
    );
    assert!(
        prose.contains("Never merge, rebase or push on their behalf"),
        "and merging on their behalf must be forbidden outright"
    );
    assert!(
        prose.contains("Never report the project finished"),
        "no machine declares a project done"
    );
}

/// The anti-patterns, each with a real failure behind it.
#[test]
fn it_forbids_the_shapes_that_have_no_governor() {
    let prose = prose();
    assert!(
        prose.contains("Work goes out from you, and results come back to you"),
        "children messaging each other has nothing limiting it at the hub"
    );
    assert!(
        prose.contains("waits its turn"),
        "the concurrency limit is not an answer to act on"
    );
    assert!(
        prose.contains("One decision at a time"),
        "a batched escalation is not an escalation"
    );
    assert!(
        prose.contains("Do not sleep, poll in a loop, or hold the turn open"),
        "the wake must end; the cadence is the loop"
    );
}

#[test]
fn the_shipped_paths_and_aliases_are_exact() {
    assert_eq!(COORDINATOR_SKILL_NAME, "agent-manager-coordinator");
    assert_eq!(
        COORDINATOR_SKILL_REL_PATH,
        ".agents/skills/agent-manager-coordinator/SKILL.md"
    );
    assert_eq!(
        claude_skill_link_target(COORDINATOR_SKILL_NAME).to_string_lossy(),
        "../../.agents/skills/agent-manager-coordinator"
    );
}
