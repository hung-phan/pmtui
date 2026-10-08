//! Repository-evidence acceptance (spec §6, §9): a worker's success claim or a
//! clean exit is NEVER acceptance for a code phase. The harness inspects the git
//! repository itself — a new commit must exist, and any commit the worker claimed
//! must be real — before it will advance the phase. Also the risky-surface
//! predicate that decides when a slice earns extra adversarial verification.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::worker::CompletionDigest;

/// Facts gathered from the git repository after a code worker returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFacts {
    pub head_before: String,
    pub head_after: String,
    /// HEAD advanced *forward*: it changed AND `head_before` is an ancestor of
    /// `head_after`. A reset/checkout that merely moves HEAD to a *different*
    /// commit (e.g. backwards, or onto an unrelated branch) is NOT acceptance —
    /// the worker must have built new history on top of the starting point.
    pub head_moved_forward: bool,
    /// Whether the worker's claimed `digest.commit_ref` resolves to a real commit
    /// (`true` when no commit was claimed — nothing to disprove).
    pub commit_ref_exists: bool,
    /// Files changed between `head_before` and `head_after`.
    pub changed_files: Vec<String>,
}

/// The acceptance decision for a code phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acceptance {
    Accepted,
    Rejected(String),
}

/// Pure acceptance decision from gathered repo facts + the worker's digest. A
/// code phase is accepted only if the worker actually moved HEAD (produced a real
/// commit) and any commit it claimed exists. Exit-0 / self-claim alone never pass.
pub fn accept_code_phase(facts: &RepoFacts, digest: Option<&CompletionDigest>) -> Acceptance {
    if !facts.head_moved_forward {
        return Acceptance::Rejected(
            "no forward commit: the worker built no new history on head_before (exit 0, a reset, \
             or a sideways/backward HEAD move is not acceptance)"
                .into(),
        );
    }
    if let Some(d) = digest
        && d.commit_ref.is_some()
        && !facts.commit_ref_exists
    {
        return Acceptance::Rejected(format!(
            "claimed commit {} does not exist in the repository",
            d.commit_ref.as_deref().unwrap_or("")
        ));
    }
    Acceptance::Accepted
}

/// Does a slice diff touch a risky surface that warrants extra adversarial
/// verification? (spec §12: `auth|session|token|crypto|payment|migration|...`).
///
/// Matches on *identifier segments*, not raw substrings: the diff is split on
/// non-alphanumerics AND camelCase/snake_case boundaries, and a segment must
/// equal a marker (case-insensitive). This flags `refresh_auth_token`,
/// `SESSION_SECRET`, and `migrateSchema` while ignoring the false positives a
/// substring scan trips on — `author`, `tokenizer`, `sessionId`-free prose.
pub fn is_risky_surface(diff: &str) -> bool {
    const MARKERS: &[&str] = &[
        "auth",
        "session",
        "token",
        "crypto",
        "password",
        "secret",
        "credential",
        "payment",
        "migration",
        "migrate",
    ];
    segments(diff).any(|seg| {
        let seg = seg.to_ascii_lowercase();
        MARKERS.contains(&seg.as_str())
    })
}

/// Split text into identifier segments: break on any non-alphanumeric character
/// and on camelCase humps (a lowercase/digit followed by an uppercase letter),
/// so `refresh_auth_token`, `refreshAuthToken`, and `AUTH-token` all yield an
/// `auth`/`token` segment, while `author`/`tokenizer` stay whole.
///
/// `pub(crate)` so [`crate::advise::hard_floor_hit`] can reuse the SAME splitter
/// for its own marker floor rather than copying it — two segment splitters that
/// drift apart would mean two different answers to "does `author` contain `auth`".
pub(crate) fn segments(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if c.is_alphanumeric() {
            // camelCase / ACRONYMWord boundary: lower|digit -> Upper starts a segment.
            if c.is_ascii_uppercase()
                && matches!(prev, Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit())
                && !cur.is_empty()
            {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
}

/// `git -C <repo> <args...>` returning trimmed stdout, erroring on non-zero exit.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("run git {args:?}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Current HEAD commit sha of `repo`.
pub fn head_commit(repo: &Path) -> Result<String> {
    git(repo, &["rev-parse", "HEAD"])
}

/// Does `refspec` resolve to a real commit object in `repo`?
fn commit_exists(repo: &Path, refspec: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{refspec}^{{commit}}")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Is `ancestor` an ancestor of `descendant` in `repo`? (`git merge-base
/// --is-ancestor` exits 0 iff so.) Used to prove HEAD moved *forward* rather
/// than sideways/backward. Any error (bad sha, git failure) → false, so an
/// unverifiable move is never treated as forward progress.
fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Gather the repository facts needed to accept/reject a code phase: the HEAD
/// before/after, whether a claimed commit exists, and the changed file set.
pub fn gather_repo_facts(
    repo: &Path,
    head_before: &str,
    claimed_commit: Option<&str>,
) -> Result<RepoFacts> {
    let head_after = head_commit(repo)?;
    let commit_ref_exists = match claimed_commit {
        Some(c) => commit_exists(repo, c),
        None => true, // nothing claimed → nothing to disprove
    };
    // Forward progress = HEAD changed AND the starting commit is an ancestor of
    // where it landed (new history built on top), not a reset or sideways move.
    let head_moved_forward =
        head_after != head_before && is_ancestor(repo, head_before, &head_after);
    let changed_files = if head_after == head_before {
        Vec::new()
    } else {
        git(
            repo,
            &["diff", "--name-only", &format!("{head_before}..HEAD")],
        )?
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
    };
    Ok(RepoFacts {
        head_before: head_before.to_string(),
        head_after,
        head_moved_forward,
        commit_ref_exists,
        changed_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: `forward` sets `head_moved_forward` directly so pure
    /// acceptance logic can be exercised without a real repo.
    fn facts(before: &str, after: &str, forward: bool, exists: bool) -> RepoFacts {
        RepoFacts {
            head_before: before.into(),
            head_after: after.into(),
            head_moved_forward: forward,
            commit_ref_exists: exists,
            changed_files: vec![],
        }
    }

    #[test]
    fn no_new_commit_is_rejected() {
        let f = facts("aaa", "aaa", false, true);
        assert!(matches!(
            accept_code_phase(&f, None),
            Acceptance::Rejected(_)
        ));
    }

    #[test]
    fn sideways_or_backward_head_move_is_rejected() {
        // HEAD changed (aaa -> bbb) but not forward (e.g. a reset/checkout onto a
        // different commit) → rejected: no new history was built on head_before.
        let f = facts("aaa", "bbb", false, true);
        assert!(matches!(
            accept_code_phase(&f, None),
            Acceptance::Rejected(_)
        ));
    }

    #[test]
    fn new_commit_with_no_claim_is_accepted() {
        let f = facts("aaa", "bbb", true, true);
        assert_eq!(accept_code_phase(&f, None), Acceptance::Accepted);
    }

    #[test]
    fn claimed_commit_must_exist() {
        let d = CompletionDigest {
            task_ids: vec![],
            commit_ref: Some("deadbeef".into()),
            tests: None,
            files: vec![],
        };
        // HEAD moved forward but the claimed commit doesn't resolve → rejected.
        let f = facts("aaa", "bbb", true, false);
        assert!(matches!(
            accept_code_phase(&f, Some(&d)),
            Acceptance::Rejected(_)
        ));
        // Claimed commit exists → accepted.
        let f = facts("aaa", "bbb", true, true);
        assert_eq!(accept_code_phase(&f, Some(&d)), Acceptance::Accepted);
    }

    #[test]
    fn risky_surface_flags_security_and_data_markers() {
        assert!(is_risky_surface("fn refresh_auth_token() { ... }"));
        assert!(is_risky_surface("+ ALTER TABLE users; -- migration"));
        assert!(is_risky_surface("let SESSION_SECRET = ..."));
        assert!(is_risky_surface("fn migrateSchema() {}")); // camelCase hump
        assert!(is_risky_surface("const apiToken = fetch()")); // camelCase hump
        assert!(!is_risky_surface("fn add(a: i32, b: i32) -> i32 { a + b }"));
    }

    #[test]
    fn risky_surface_does_not_over_trigger_on_lookalikes() {
        // Substring lookalikes must NOT flag: the marker is a proper substring
        // of a longer identifier, not a segment of its own.
        assert!(!is_risky_surface("let author = book.author_name;"));
        assert!(!is_risky_surface("fn tokenizer(input: &str) {}"));
        assert!(!is_risky_surface("struct Cryptography;")); // "crypto" is a prefix, not a segment
        assert!(!is_risky_surface(
            "// authorization is documented elsewhere"
        ));
    }

    #[test]
    fn gather_repo_facts_against_a_real_git_repo() {
        // Only run where git is available (it is, in this project's toolchain).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "one").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);
        let before = head_commit(repo).unwrap();

        // No new commit yet → head unchanged, accept rejects.
        let f = gather_repo_facts(repo, &before, None).unwrap();
        assert_eq!(f.head_after, before);
        assert!(matches!(
            accept_code_phase(&f, None),
            Acceptance::Rejected(_)
        ));

        // Make a real commit → head moves forward, changed file detected, accepted.
        std::fs::write(repo.join("b.txt"), "two").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "second"]);
        let after = head_commit(repo).unwrap();
        let f = gather_repo_facts(repo, &before, Some(&after)).unwrap();
        assert_ne!(f.head_after, before);
        assert!(f.head_moved_forward, "before is an ancestor of after");
        assert!(f.commit_ref_exists);
        assert!(
            f.changed_files.contains(&"b.txt".to_string()),
            "{:?}",
            f.changed_files
        );
        assert_eq!(accept_code_phase(&f, None), Acceptance::Accepted);

        // A bogus claimed commit is caught.
        let f = gather_repo_facts(
            repo,
            &before,
            Some("0000000000000000000000000000000000000000"),
        )
        .unwrap();
        assert!(!f.commit_ref_exists);

        // Sideways/backward move: create a divergent commit on a new root so
        // `after` is NOT an ancestor of the new HEAD. head_moved_forward must be
        // false and acceptance must reject, even though HEAD changed.
        run(&["checkout", "-q", "--orphan", "other"]);
        run(&["rm", "-rf", "--cached", "."]);
        std::fs::write(repo.join("c.txt"), "three").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "divergent"]);
        let f = gather_repo_facts(repo, &after, None).unwrap();
        assert_ne!(f.head_after, after, "HEAD did change");
        assert!(
            !f.head_moved_forward,
            "an unrelated/divergent HEAD is not forward progress"
        );
        assert!(matches!(
            accept_code_phase(&f, None),
            Acceptance::Rejected(_)
        ));
    }
}
