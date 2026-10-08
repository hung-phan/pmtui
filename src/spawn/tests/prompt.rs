//! The prompt a job child runs: what every job is told, and the extra step a job in its own worktree
//! gets.

use crate::spawn::job_prompt;

/// THE MESSAGE IS LAST, after a fixed preamble, and it is the only part a parent writes. The harness says
/// what nobody can do for the child — there is no human at that terminal — and what to end with.
#[test]
fn the_preamble_comes_first_and_the_task_last() {
    let prompt = job_prompt("  Rename the flag.  ", None);

    assert!(prompt.starts_with("You are a job worker"), "{prompt}");
    assert!(
        prompt.trim_end().ends_with("Rename the flag."),
        "the task is the last thing it reads: {prompt}"
    );
    assert!(
        prompt.contains("Nobody is at this terminal"),
        "a job is told nobody will answer it: {prompt}"
    );
    assert!(
        prompt.contains("`needs_human`"),
        "and what to do about a decision only a person can make: {prompt}"
    );
    assert!(
        !prompt.contains("branch"),
        "a job with no worktree is told nothing about one: {prompt}"
    );
}

/// A JOB IN ITS OWN WORKTREE IS TOLD TO COMMIT, by name, BEFORE the task. The directory is deleted when
/// its row retires and a commit on that branch is the only thing that outlives it, so this is a step
/// rather than advice — and it says not to merge or push, because integrating is a human's decision.
#[test]
fn a_worktree_job_is_told_to_commit_on_its_own_branch() {
    let prompt = job_prompt("Rename the flag.", Some("pm/proj-2-550e8400"));

    assert!(
        prompt.contains("branch pm/proj-2-550e8400"),
        "the branch is named: {prompt}"
    );
    assert!(prompt.contains("Commit your work"), "{prompt}");
    // ONE commit, because one is what `a` applies: the receipt names the branch's HEAD while the file list
    // is the whole branch's, so a child that made three commits had two left behind under a status line
    // reporting the full count as applied.
    assert!(
        prompt.contains("as ONE commit"),
        "the number of commits is not left to the child: {prompt}"
    );
    assert!(
        prompt.contains("work you do not commit is lost"),
        "it says what uncommitted work costs: {prompt}"
    );
    assert!(
        prompt.contains("Do not merge"),
        "and that integrating is not its job: {prompt}"
    );
    // Order matters: the instruction is part of the procedure, not an afterthought under the task.
    let commit_at = prompt.find("Commit your work").expect("the commit step");
    let task_at = prompt.find("Rename the flag.").expect("the task");
    assert!(commit_at < task_at, "the step comes before the task");
    assert!(
        prompt.trim_end().ends_with("Rename the flag."),
        "and the task is still last: {prompt}"
    );
}
