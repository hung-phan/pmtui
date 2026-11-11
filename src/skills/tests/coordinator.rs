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
    // A CONVERSATIONAL PART NOW HAS THREE ANSWERS, not two. The skill used to say such a part is
    // "not ready to dispatch" and to do it yourself, which was true while every child was a job —
    // a job cannot be asked anything. `--session` is exactly the missing third answer, so the
    // sentence that sent a coordinator away from it had to go with the feature.
    assert!(
        prose.contains("Such a part is not ready for a JOB"),
        "a conversational part is refused for the JOB form, not for every form"
    );
    assert!(
        prose.contains("Or stand up a child session for it"),
        "and the third answer must be named where the reader is turned away"
    );
    assert!(
        !prose.contains("is not ready to dispatch"),
        "the old blanket refusal would hide the form that handles it"
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
    // A STAR, AND NOW BY CHOICE RATHER THAN BY FACT. While every child was a job this was simply
    // true: a job has no `ManagedEnv` and cannot be reached. A child SESSION is a peer, so two
    // children can talk, and the skill has to say the rule is a decision and why — nobody watches
    // that conversation and every turn of it costs money.
    assert!(
        prose.contains("Keep the work going out from you, and the results coming back to you"),
        "the hub topology must survive as an explicit rule"
    );
    assert!(
        prose.contains("A child session is a peer, so two of them CAN talk"),
        "and it must admit the thing it is choosing against"
    );
    // The trap the job vocabulary sets for a session child: `ready` forever, and a coordinator
    // that treats it as "still running" waits for a result that is never coming.
    assert!(
        prose.contains("It stays `ready` in `spawn --status`. Never wait for that to change."),
        "a session child's `ready` must be disarmed where the loop reads it"
    );
    assert!(
        prose.contains("Wait for a result from a child session."),
        "and listed among the shapes with no governor"
    );
    // THERE IS NO PER-PARENT CAP to hold back for. The skill used to say a request over the limit
    // "waits its turn"; the cap it referred to counted ROWS, so dead siblings blocked a parent
    // forever. Pacing is `spawn::MAX_UNFINISHED_PER_PARENT` at discovery, which an agent never
    // sees and must never ration against.
    assert!(
        prose.contains("Ask for the children the work needs"),
        "a coordinator must not ration its own fan-out"
    );
    assert!(
        !prose.contains("waits its turn"),
        "no cap is left for a coordinator to wait on"
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
