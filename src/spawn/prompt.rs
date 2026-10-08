//! The prompt a JOB child runs: the parent's Message, under a fixed harness preamble.
//!
//! Procedural, not narrative — the house rule for agent-facing copy. It says what to do, what nobody
//! can do for the child (there is no human at this terminal), and what to end with. It does NOT ask
//! the child to write a file: the result is the harness's final payload, shaped by the schema the argv
//! passes. Everything here is harness-authored except the Message itself.

/// The preamble, kept whole so a reader sees exactly what every job is told.
const PREAMBLE: &str = "\
You are a job worker in a managed session. Do the task below, then stop.

1. Do the task in this directory. Nothing else will assign you work, and no one will follow up.
2. Nobody is at this terminal. Do not ask a question and do not wait for approval. If the task needs
   a decision only a person can make, stop and report `needs_human`, naming the decision.
3. End by returning your report as your final message, with these fields:
   - `outcome`: `done`, `needs_human`, or `failed`
   - `summary`: one or two sentences on what became of the task
   - `detail`: optional, what a person should read afterwards
4. Report `done` only for work you finished and checked. Report `failed` with what you tried.
";

/// What a job in its own git worktree is told, inserted before the task.
///
/// COMMITTING IS HOW THE WORK LEAVES. The directory is deleted once the row is retired, and a commit on
/// this branch is the only thing that outlives it — so this is a step, not advice. It is also why the
/// child is told not to merge or push: integrating is a human's decision, made from the dashboard.
///
/// ONE commit, because one is what integration applies. The receipt names the branch's HEAD and `a`
/// cherry-picks exactly that, while the file list is the whole branch's — so a child that made three
/// commits had two left behind while the dashboard reported the full count as applied. Asking for one
/// commit makes what is reported and what lands the same thing.
const WORKTREE: &str = "\
5. You are on branch %BRANCH% in a git worktree of your own. Commit your work on it as ONE commit
   before you report; work you do not commit is lost when this directory is cleaned up. Do not merge,
   rebase onto another branch, or push: a person integrates it.
";

/// The full prompt for a job carrying `message`, and `branch` when it runs in its own worktree.
pub fn job_prompt(message: &str, branch: Option<&str>) -> String {
    let worktree = branch
        .map(|branch| WORKTREE.replace("%BRANCH%", branch))
        .unwrap_or_default();
    format!("{PREAMBLE}{worktree}\nTASK\n{}\n", message.trim())
}
