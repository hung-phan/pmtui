//! A job's own git worktree: where a spawned child writes, and how its work comes back.
//!
//! Children run at the same time in the same project, so without this they share one checkout and
//! overwrite each other's edits. A worktree gives each one its own files and its own branch while
//! sharing the repository's `.git`, which is what makes the work reachable afterwards: a commit the
//! child made is already in the parent checkout's object store, on a branch, before anyone integrates
//! anything.
//!
//! ISOLATION IS NOT INTEGRATION. Nothing here merges a child's work on its own. A finished job reports
//! `{branch, commit, touched}` and a HUMAN decides, because this tool's rule is that no machine declares
//! a project done and the human's checkout is theirs. [`integrate`] is what that keystroke runs, one
//! commit at a time, and it refuses a dirty checkout rather than mixing a child's work into uncommitted
//! changes.
//!
//! What this module will NOT do: delete uncommitted work. [`prune`] removes a worktree only when the
//! child left nothing behind that is not already a commit. A dirty worktree outlives its row on purpose.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

/// A worktree created for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobWorktree {
    /// Where the child runs.
    pub path: PathBuf,
    /// The branch it was created on, which is where its commits land.
    pub branch: String,
    /// The commit it started from, so "what did this child change" has an answer.
    pub base: String,
}

/// What a job left in its worktree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JobWork {
    pub branch: String,
    /// The commit the child produced, when it committed anything at all.
    pub commit: Option<String>,
    /// Files its commits changed, relative to the base.
    pub touched: Vec<String>,
    /// Changes it did NOT commit. A dirty worktree is never pruned.
    pub dirty: bool,
}

impl JobWork {
    /// Whether this work is safe to throw away: nothing uncommitted, nothing to integrate.
    pub fn is_empty(&self) -> bool {
        self.commit.is_none() && !self.dirty
    }
}

/// What came of integrating one commit into the parent checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integration {
    /// Applied; the parent checkout's HEAD is now this commit.
    Applied(String),
    /// Refused before touching anything, with the reason.
    Refused(String),
    /// It conflicted. The cherry-pick was aborted, so the checkout is as it was.
    Conflicted(String),
}

/// `git -C <dir> <args…>`, trimmed stdout, non-zero exit is an error.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether `dir` is inside a git working tree — the one question that decides whether a job gets a
/// worktree at all. A project that is not a repository keeps the old behaviour: the child runs in the
/// project directory itself.
pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|out| out == "true")
        .unwrap_or(false)
}

/// The branch name for one job: its session id and the head of its request id.
///
/// The request id is in there because SESSION IDS ARE REUSED — a retired job frees its id for the next
/// child — and `git worktree add -b` refuses a branch that already exists. Two jobs that share an id
/// across time must not share a branch.
pub fn branch_name(session_id: &str, request_id: &str) -> String {
    let short: String = request_id.chars().take(8).collect();
    format!("pm/{session_id}-{short}")
}

/// Create `at` as a worktree of `repo` on a fresh branch from the repo's current HEAD.
///
/// The parent directory is created first; `at` itself must not exist, which `git worktree add` enforces.
pub fn create(repo: &Path, at: &Path, branch: &str) -> Result<JobWorktree> {
    let base =
        git(repo, &["rev-parse", "HEAD"]).context("the repository has no HEAD to branch from")?;
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let at_str = at.to_string_lossy().to_string();
    git(
        repo,
        &["worktree", "add", "--quiet", "-b", branch, &at_str, &base],
    )
    .with_context(|| format!("add a worktree for {branch}"))?;
    Ok(JobWorktree {
        path: at.to_path_buf(),
        branch: branch.to_string(),
        base,
    })
}

/// Read what the job did in its worktree: its commit, what that changed, and whether anything is
/// uncommitted.
///
/// A worktree that is gone reports empty work rather than an error — the caller is finishing a job
/// either way, and "there is nothing there" is the honest answer.
pub fn collect(worktree: &JobWorktree) -> JobWork {
    let mut work = JobWork {
        branch: worktree.branch.clone(),
        ..JobWork::default()
    };
    if !worktree.path.is_dir() {
        return work;
    }
    let head = git(&worktree.path, &["rev-parse", "HEAD"]).unwrap_or_default();
    if !head.is_empty() && head != worktree.base {
        work.commit = Some(head.clone());
        work.touched = git(
            &worktree.path,
            &["diff", "--name-only", &format!("{}..{head}", worktree.base)],
        )
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    }
    // `--porcelain` lists staged, unstaged AND untracked paths; anything at all means the child left
    // work that only exists in this directory. A status this CANNOT READ counts as dirty: the only
    // consumer of this flag is the cleanup that deletes the directory, so the unknown answer has to be
    // the one that keeps it. Reading a failure as clean made that cleanup run `--force` over work it
    // could not see.
    work.dirty = match git(&worktree.path, &["status", "--porcelain"]) {
        Ok(status) => !status.trim().is_empty(),
        Err(_) => true,
    };
    work
}

/// Remove a job's worktree directory, keeping its branch and commits.
///
/// REFUSES A DIRTY WORKTREE. Everything else about a job is reconstructible from its receipt, but
/// uncommitted changes exist nowhere else, so this is the one cleanup that declines to run. `git worktree
/// remove` is used rather than a plain delete so the repository's own bookkeeping goes with it.
pub fn prune(repo: &Path, worktree: &JobWorktree) -> Result<()> {
    if collect(worktree).dirty {
        bail!(
            "{} has uncommitted changes, so it is kept",
            worktree.path.display()
        );
    }
    if worktree.path.exists() {
        git(
            repo,
            &[
                "worktree",
                "remove",
                "--force",
                &worktree.path.to_string_lossy(),
            ],
        )?;
    }
    // A worktree whose directory a purge already removed leaves admin files behind; this is what makes
    // the repository forget it.
    let _ = git(repo, &["worktree", "prune"]);
    Ok(())
}

/// Delete a job's branch once nobody needs it. Separate from [`prune`] because a branch is the last
/// place a commit lives: it goes only when a human has integrated the work or thrown it away.
pub fn drop_branch(repo: &Path, branch: &str) -> Result<()> {
    git(repo, &["branch", "-D", branch]).map(|_| ())
}

/// Apply one of a child's commits to the parent checkout, as a cherry-pick onto its current HEAD.
///
/// Refuses rather than guesses: no commit, not a repository, or a checkout with changes of its own. On a
/// conflict the pick is aborted so the human's checkout is exactly as they left it — a half-applied
/// cherry-pick in someone else's working tree is the worst outcome available here.
pub fn integrate(repo: &Path, commit: &str) -> Integration {
    if commit.is_empty() {
        return Integration::Refused("there is no commit to integrate".into());
    }
    if !is_repo(repo) {
        return Integration::Refused(format!("{} is not a git repository", repo.display()));
    }
    // TRACKED changes only. Untracked paths are not a reason to refuse: every managed project already
    // has an untracked `.project-state/`, and that is where the children's own worktrees live — reading
    // those as "the human has uncommitted work" would refuse every integration in a repository that does
    // not ignore it. A cherry-pick that would actually overwrite an untracked file is stopped by git
    // itself, which lands below as a conflict with the checkout put back.
    // A status this cannot read is not a second kind of refusal: it falls through to the pick below,
    // which fails safely and puts the checkout back. One branch fewer for the same guarantee.
    if !git(repo, &["status", "--porcelain", "--untracked-files=no"])
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Integration::Refused(
            "this checkout has uncommitted changes; commit or stash them first".into(),
        );
    }
    match git(repo, &["cherry-pick", commit]) {
        // A pick that succeeded moved HEAD, so reading it cannot meaningfully fail; an empty answer is
        // reported as-is rather than turned into a refusal of work that already landed.
        Ok(_) => Integration::Applied(git(repo, &["rev-parse", "HEAD"]).unwrap_or_default()),
        Err(error) => {
            // Leave nothing half-applied. `--quit` would keep the index; `--abort` is what puts the
            // checkout back where the human left it.
            let _ = git(repo, &["cherry-pick", "--abort"]);
            Integration::Conflicted(format!("{error:#}"))
        }
    }
}

#[cfg(test)]
mod tests;
