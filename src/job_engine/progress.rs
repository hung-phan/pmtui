//! The NO-PROGRESS circuit breaker (slice 1): a *product-of-output* stall signal that
//! complements the report/pane signals the rest of the engine already has. Every other
//! stall guard here reasons about the agent's REPORTS (`stale_plan_streak`,
//! `marker_less_rechecks`) or the PANE (`busy_since`, the turn-end hook). This one asks a
//! different question — *did the last N nudges actually change the repository?* — because
//! an agent can keep looking idle-at-its-prompt and even keep writing fresh markers while
//! producing nothing. Borrowed from `ralphex`'s diff-fingerprint idea.
//!
//! Two pieces, kept apart so the decision is byte-pure and unit-testable without git or a
//! terminal: [`tree_fingerprint`] shells out to git for a fingerprint of the working tree
//! (returning `None` — the detector does nothing — for a non-git tree or any git error),
//! and [`step_no_progress`] is the pure evaluator that turns two fingerprints + a streak
//! into "continue" or "escalate". The wiring on `JobScheduler`
//! ([`JobScheduler::no_progress_backstop`]) holds only the in-memory streak, exactly like
//! `busy_since`.
//!
//! Fail-safe by construction: the detector can only ever ESCALATE (surface to the human,
//! who resets it by answering "keep going"), never terminate; a git error disables it for
//! that tick; and it is gated on a threshold that a kill switch (`PM_NO_PROGRESS`) or a
//! test can set to `0` (off).

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::clock::Epoch;
use crate::job::AgentLoopState;

use super::{JobScheduler, JobTick};

/// The kill-switch env var: `off`/`0`/`false`/`no` disables the backstop; a bare integer
/// overrides the threshold; anything else (or unset) uses [`DEFAULT_NO_PROGRESS_NUDGES`].
pub(super) const NO_PROGRESS_ENV: &str = "PM_NO_PROGRESS";

/// Default: escalate once the working tree is byte-identical across this many CONSECUTIVE
/// nudges (i.e. this many inter-nudge intervals produced no change). Deliberately generous
/// — at the 5-minute default cadence this is ~15 minutes of an agent looking busy while
/// producing nothing — so a legitimately investigating agent is not surfaced too eagerly,
/// and a false positive only ever costs the human one glance (they answer "keep going").
pub(super) const DEFAULT_NO_PROGRESS_NUDGES: u32 = 3;

/// Parse [`NO_PROGRESS_ENV`] into a threshold (`0` = disabled). Pure so it is unit-testable
/// without touching process env. Unrecognised text falls back to the default (fail toward
/// the feature ON, since the whole point is to catch a class of silent wedge).
pub(super) fn no_progress_threshold_from_env(var: Option<&str>) -> u32 {
    let Some(v) = var else {
        return DEFAULT_NO_PROGRESS_NUDGES;
    };
    let t = v.trim();
    if t.eq_ignore_ascii_case("off")
        || t.eq_ignore_ascii_case("false")
        || t.eq_ignore_ascii_case("no")
        || t == "0"
    {
        0
    } else {
        t.parse::<u32>().unwrap_or(DEFAULT_NO_PROGRESS_NUDGES)
    }
}

/// The result of one [`step_no_progress`] evaluation: the updated streak + fingerprint to
/// store, and whether the run has hit the threshold and should escalate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProgressStep {
    pub streak: u32,
    pub fp: Option<u64>,
    pub escalate: bool,
}

/// The byte-pure heart of the circuit breaker. Given the fingerprint stored at the previous
/// nudge, the current one, the current streak, and the threshold, decide the next state.
///
/// - `cur_fp == None` (git unavailable / not a repo): the detector cannot observe, so it
///   forgets the baseline and never escalates — we must not compare across a gap we could
///   not see.
/// - `threshold == 0`: disabled — never escalate (baseline still tracked so re-enabling is
///   clean).
/// - `prev == None`: first observation since a reset — establish the baseline; no interval
///   has been observed yet, so streak stays `0`.
/// - `prev == cur`: the tree is byte-identical to the previous nudge, so that whole interval
///   produced nothing — count it, and escalate once the run reaches `threshold`.
/// - `prev != cur`: the tree changed — real progress — so reset the run and re-baseline.
pub(super) fn step_no_progress(
    prev_fp: Option<u64>,
    cur_fp: Option<u64>,
    streak: u32,
    threshold: u32,
) -> ProgressStep {
    let Some(cur) = cur_fp else {
        return ProgressStep {
            streak: 0,
            fp: None,
            escalate: false,
        };
    };
    if threshold == 0 {
        return ProgressStep {
            streak: 0,
            fp: Some(cur),
            escalate: false,
        };
    }
    match prev_fp {
        None => ProgressStep {
            streak: 0,
            fp: Some(cur),
            escalate: false,
        },
        Some(prev) if prev == cur => {
            let streak = streak.saturating_add(1);
            ProgressStep {
                streak,
                fp: Some(cur),
                escalate: streak >= threshold,
            }
        }
        Some(_) => ProgressStep {
            streak: 0,
            fp: Some(cur),
            escalate: false,
        },
    }
}

/// Fingerprint the working tree at `work_dir`, EXCLUDING the harness's own `.project-state/`
/// bookkeeping (which changes every tick and is not agent output). `None` means "cannot
/// observe" — the directory is not a usable git repo, or git is missing/failed — and the
/// caller then does nothing (fail-safe). Read-only: uses `git status`/`diff`/`rev-parse`
/// with `GIT_OPTIONAL_LOCKS=0` so it never takes the index lock the live agent may be using.
///
/// The fingerprint combines the set of pending changes (staged/unstaged/untracked, so a NEW
/// file counts), the *content* of tracked modifications (porcelain alone shows only each path
/// and its status flag, not the edit), and the current commit (so a commit counts as progress).
/// Known gap (acceptable for v1): repeated content churn WITHIN a single untracked file keeps
/// the same fingerprint.
pub(super) fn tree_fingerprint(work_dir: &Path) -> Option<u64> {
    // Exclude the harness's own state dir from BOTH the status and the diff, so our per-tick
    // ledger/marker writes are never mistaken for the agent making progress. A git `:(exclude)`
    // pathspec covers all three ways `.project-state` can show up — untracked (the common case,
    // via `-uall`), and staged/tracked content (via `diff HEAD`, if a project happens to track
    // it) — which a plain line-filter on the porcelain output would miss. `:(exclude)` needs a
    // positive pathspec beside it, hence the leading `.`.
    const EXCLUDE_STATE: [&str; 3] = ["--", ".", ":(exclude).project-state"];
    // Primary signal. If this can't run, `work_dir` is not a usable git tree ⇒ `None`.
    let status = {
        let mut a = vec!["status", "--porcelain=v1", "--untracked-files=all"];
        a.extend_from_slice(&EXCLUDE_STATE);
        run_git(work_dir, &a)?
    };
    // A repo with no commits yet makes `diff HEAD`/`rev-parse HEAD` fail — treat those as
    // empty rather than disabling the whole detector (the porcelain signal still works).
    let diff = {
        let mut a = vec!["diff", "HEAD"];
        a.extend_from_slice(&EXCLUDE_STATE);
        run_git(work_dir, &a).unwrap_or_default()
    };
    let head = run_git(work_dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    status.hash(&mut h);
    diff.hash(&mut h);
    head.hash(&mut h);
    Some(h.finish())
}

/// Run `git -C <work_dir> <args…>` read-only; `Some(stdout)` on a clean exit, else `None`.
fn run_git(work_dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(work_dir)
        .args(args)
        // Never take the index lock: the live agent is editing this same repo concurrently.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl JobScheduler {
    /// Slice-1 backstop, called on the confirmed-idle heartbeat path just before a nudge:
    /// compare the working tree now against the tree at the previous nudge. Returns
    /// `Some(Stuck)` when the agent has produced no change across `no_progress_threshold`
    /// consecutive nudges (it is spinning) — escalating a human-dismissable `Stuck` instead
    /// of nudging it forever — and `None` otherwise (having updated the in-memory streak so
    /// the ordinary nudge proceeds). Disabled (threshold `0`) or unobservable (non-git tree)
    /// ⇒ always `None`, so it is inert unless the tree is a git repo AND the feature is on.
    pub(super) fn no_progress_backstop(
        &mut self,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<Option<JobTick>> {
        if self.no_progress_threshold == 0 {
            return Ok(None);
        }
        let cur = tree_fingerprint(&self.work_dir);
        let step = step_no_progress(
            self.last_progress_fp,
            cur,
            self.no_progress_streak,
            self.no_progress_threshold,
        );
        self.last_progress_fp = step.fp;
        self.no_progress_streak = step.streak;
        if step.escalate {
            let reason = format!(
                "the working tree has not changed across {} nudges — the agent looks idle at \
                 its prompt but is producing no edits or commits, so it may be spinning without \
                 making progress; close it, or answer to keep it going",
                step.streak
            );
            return Ok(Some(self.park_stuck(now, base, reason)?));
        }
        Ok(None)
    }

    /// Reset the no-progress run: a human touch (an answer, or attaching) is a fresh start,
    /// exactly like `busy_since`. Called from the same places that reset `busy_since` for a
    /// "human here / fresh start" reason.
    pub(super) fn reset_no_progress(&mut self) {
        self.no_progress_streak = 0;
        self.last_progress_fp = None;
    }
}
