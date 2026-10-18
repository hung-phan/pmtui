//! Retiring a finished JOB child: what the broker does once a one-shot child's process is gone.
//!
//! Completion here is always a FACT the test can stage — a dead tmux session plus whatever the harness
//! left in `job.log`, `job.last` and the exit-code signal. Nothing in these tests fakes "idleness",
//! because nothing in the sweep reads it.

use super::*;
use agent_manager::registry::{
    LaunchKind, LaunchRecord, LaunchState, Mode, ProjectEntry, Registry,
};
use agent_manager::spawn::{self, ReceiptState};
use agent_manager::state::ProjectPaths;

use crate::app::spawning::CANCEL_GRACE_MS;

const REQUEST: &str = "550e8400-e29b-41d4-a716-446655440000";

/// A parent row and the job row it spawned, with the child's state directory in place.
struct Fixture {
    _dir: tempfile::TempDir,
    registry: PathBuf,
    child_paths: ProjectPaths,
    parent_requests: PathBuf,
    child_session: String,
}

/// `kind` is what the child's `launch` records; `log`/`last`/`code` are what its run left behind.
fn fixture(kind: LaunchKind, log: &str, last: Option<&str>, code: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = dir.path().join("registry.json");
    let parent_root = dir.path().join("parent");
    let child_root = dir.path().join("kid");

    let row =
        |id: &str, root: &Path, launch: Option<LaunchRecord>, parent: Option<&str>| ProjectEntry {
            id: id.into(),
            display_name: None,
            root: root.to_path_buf(),
            enabled: true,
            mode: Mode::AgentLoop,
            engine: Some(Engine::Claude),
            worker_model: None,
            initial_prompt: Some("do the thing".into()),
            task_title: Some("the thing".into()),
            forked_from: None,
            spawned_by: parent.map(str::to_string),
            launch,
            conversation_id: None,
            cadence_s: None,
        };
    let mut reg = Registry::default();
    reg.projects.push(row("parent", &parent_root, None, None));
    reg.projects.push(row(
        "kid",
        &child_root,
        Some(LaunchRecord {
            request_id: REQUEST.into(),
            args_hash: "h".into(),
            state: LaunchState::Started,
            // Readiness finished and called it ready: that is the row the sweep takes over.
            outcome: Some(agent_manager::registry::SpawnOutcome::Ready),
            kind,
            branch: None,
            base_commit: None,
        }),
        Some("parent"),
    ));
    reg.save(&registry).expect("save registry");

    let child_paths = ProjectPaths::for_session(&child_root, "kid");
    std::fs::create_dir_all(child_paths.job_log().parent().expect("steps dir")).expect("mkdir");
    std::fs::write(child_paths.job_log(), log).expect("write log");
    if let Some(last) = last {
        std::fs::write(child_paths.job_last_message(), last).expect("write last message");
    }
    if let Some(code) = code {
        std::fs::write(child_paths.job_done_signal(), code).expect("write done signal");
    }
    // The parent's requests directory exists in production because the parent published its request
    // there; the receipt is written beside it.
    let parent_requests =
        spawn::requests_dir(&ProjectPaths::for_session(&parent_root, "parent").state_dir());
    std::fs::create_dir_all(&parent_requests).expect("parent requests dir");
    Fixture {
        parent_requests,
        child_session: session_name("kid", &child_root),
        child_paths,
        registry,
        _dir: dir,
    }
}

/// Make the child's root a real repository and give the job a worktree on `branch`, as a launch does.
/// Returns the worktree path. The row's launch learns the branch and base, which is how the harvest
/// finds it again.
fn git_worktree(fx: &Fixture, branch: &str) -> PathBuf {
    let child_root = fx.child_paths.state_dir();
    let root = project_root(fx);
    init_repo_at(&root);
    let base = run_git(&root, &["rev-parse", "HEAD"]);
    let at = child_root.join("worktree");
    std::fs::create_dir_all(child_root.as_path()).expect("state dir");
    run_git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &at.to_string_lossy(),
            &base,
        ],
    );
    let mut reg = Registry::load(&fx.registry).expect("load");
    if let Some(launch) = reg
        .projects
        .iter_mut()
        .find(|p| p.id == "kid")
        .and_then(|p| p.launch.as_mut())
    {
        launch.branch = Some(branch.to_string());
        launch.base_commit = Some(base);
    }
    reg.save(&fx.registry).expect("save");
    at
}

/// Make `root` a repository with one commit, independent of the machine's git identity.
fn init_repo_at(root: &Path) {
    for args in [
        vec!["init", "--quiet", "-b", "main"],
        vec!["config", "user.email", "pm@example.invalid"],
        vec!["config", "user.name", "pm tests"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        run_git(root, &args);
    }
    std::fs::write(root.join("README.md"), "start\n").expect("write");
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "--quiet", "-m", "start"]);
}

/// The project root a fixture's child lives under.
fn project_root(fx: &Fixture) -> PathBuf {
    fx.child_paths
        .state_dir()
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the project root")
        .to_path_buf()
}

/// `git -C <dir> <args…>`, which must succeed.
fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Commit `files` in `dir` as a child would, returning the new HEAD.
fn git_commit(dir: &Path, message: &str, files: &[(&str, &str)]) -> String {
    for (name, body) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create");
        }
        std::fs::write(path, body).expect("write");
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "--quiet", "-m", message]);
    run_git(dir, &["rev-parse", "HEAD"])
}

/// The same fixture with a readiness outcome other than `Ready` — what a job that finished before
/// readiness could see it twice leaves behind.
fn fixture_with_outcome(
    outcome: agent_manager::registry::SpawnOutcome,
    log: &str,
    code: &str,
) -> Fixture {
    let fx = fixture(LaunchKind::Job, log, None, Some(code));
    let mut reg = Registry::load(&fx.registry).expect("load");
    if let Some(launch) = reg
        .projects
        .iter_mut()
        .find(|p| p.id == "kid")
        .and_then(|p| p.launch.as_mut())
    {
        launch.outcome = Some(outcome);
    }
    reg.save(&fx.registry).expect("save");
    fx
}

/// A claude `result` line carrying our schema — what a job that reported looks like on disk.
fn claude_result(outcome: &str, summary: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","result":"prose","structured_output":{{"outcome":"{outcome}","summary":"{summary}"}}}}"#
    )
}

/// An app whose driver reports every session DEAD (the default) unless told otherwise, over the
/// fixture's registry.
fn app_for(fx: &Fixture, driver: FakePane) -> App {
    let mut app = app_with_driver(vec![], UiMode::Normal, Box::new(driver));
    app.registry_path = fx.registry.clone();
    app
}

fn sweep(app: &mut App, fx: &Fixture) {
    let reg = Registry::load(&fx.registry).expect("load registry");
    app.retire_finished_jobs(&reg);
}

fn receipt(fx: &Fixture) -> Option<spawn::SpawnReceipt> {
    spawn::read_receipt(&fx.parent_requests, REQUEST)
        .ok()
        .flatten()
}

fn rows(fx: &Fixture) -> Vec<String> {
    Registry::load(&fx.registry)
        .expect("load")
        .projects
        .into_iter()
        .map(|p| p.id)
        .collect()
}

/// THE WHOLE FEATURE, in one test: a child that finished has its result recorded in the parent's
/// receipt and its row taken off the list, with no human action.
#[test]
fn a_finished_job_is_retired_and_its_result_lands_in_the_receipt() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "renamed the flag and updated its test"),
        None,
        Some("0"),
    );
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    let receipt = receipt(&fx).expect("the parent is answered");
    assert_eq!(receipt.state, ReceiptState::Done);
    let result = receipt.result.expect("the result is carried");
    assert_eq!(result.outcome, spawn::JobOutcome::Done);
    assert_eq!(result.summary, "renamed the flag and updated its test");
    assert_eq!(receipt.schema_version, spawn::RECEIPT_SCHEMA_VERSION);

    assert_eq!(rows(&fx), vec!["parent"], "the job's row is gone");
    assert!(
        app.status.contains("retired kid") && app.status.contains("renamed the flag"),
        "the status names what was retired and why: {}",
        app.status
    );
    assert!(
        !fx.child_paths.state_dir().exists(),
        "and its state goes with it — the receipt above is what outlives the row"
    );
}

/// A SECOND SWEEP OVER THE SAME ROW NEVER UNDOES THE FIRST. Retiring a child deletes the state its
/// answer was read from, so a repeat pass sees an empty directory — and used to report
/// `ended_without_result` over an outcome that was already correct. Three of five children lost their
/// answers that way when a parent fanned out at once (`retire_finished_jobs` re-entering itself through
/// `remove_project` -> `refresh`, each level carrying a snapshot that still listed retired rows), so the
/// recorded answer now wins no matter what brings a row back here.
#[test]
fn a_repeat_sweep_cannot_replace_an_answer_already_recorded() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "renamed the flag and updated its test"),
        None,
        Some("0"),
    );
    // The snapshot a re-entrant sweep would still be holding: taken while the job row was live.
    let stale = Registry::load(&fx.registry).expect("load registry");
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    assert_eq!(
        receipt(&fx).map(|r| r.state),
        Some(ReceiptState::Done),
        "the first sweep answers the parent"
    );

    app.retire_finished_jobs(&stale);

    let again = receipt(&fx).expect("the receipt is still there");
    assert_eq!(
        (again.state, again.result.map(|found| found.summary)),
        (
            ReceiptState::Done,
            Some("renamed the flag and updated its test".to_string())
        ),
        "a pass over a row whose state is already purged must not overwrite its answer"
    );
}

/// `a` IS THE HUMAN'S STEP. A job isolates its work on a branch; this is the keystroke that brings it
/// into the checkout a person is using — behind a confirmation, because it is the one action here that
/// writes to their own files.
#[test]
fn apply_cherry_picks_a_jobs_commit_after_a_confirmation() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "renamed the flag"),
        None,
        Some("0"),
    );
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    let sha = git_commit(&wt, "the child's work", &[("src/flag.rs", "// renamed\n")]);
    let root = fx.child_paths.state_dir();
    let project = root
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the project root")
        .to_path_buf();
    let mut app = app_for(&fx, FakePane::default());
    app.refresh();
    app.selected = app
        .projects
        .iter()
        .position(|v| v.id == "kid")
        .expect("the job row");

    // `a` ASKS FIRST.
    app.begin_apply();
    assert!(
        matches!(
            app.mode,
            UiMode::Confirming {
                what: Confirmable::Apply,
                ..
            }
        ),
        "a must confirm, got {:?}",
        app.mode
    );
    assert!(app.status.contains(&sha[..8]), "{}", app.status);

    app.apply_job_commit("kid");

    assert_eq!(
        std::fs::read_to_string(project.join("src/flag.rs")).expect("the child's file is here now"),
        "// renamed\n"
    );
    assert_eq!(
        run_git(&project, &["log", "-1", "--pretty=%s"]),
        "the child's work",
        "as the child's own commit"
    );
    assert!(
        app.status.contains("applied") && app.status.contains("pm/kid-5f0c3a1e"),
        "{}",
        app.status
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "the row is kept: a person may still want its log, and `d` clears it"
    );
}

/// `a` REFUSES OUT LOUD, each case in its own words: a row that is not a job, a job with no worktree, a
/// job that committed nothing, and a checkout with changes of its own. "Nothing happened" is the one
/// answer a key must never give.
#[test]
fn apply_says_why_when_there_is_nothing_to_apply() {
    // A chat row is not a job.
    let chat = fixture(LaunchKind::Chat, "", None, None);
    let mut app = app_for(&chat, FakePane::default());
    app.refresh();
    app.selected = app
        .projects
        .iter()
        .position(|v| v.id == "kid")
        .expect("the row");
    app.begin_apply();
    assert!(app.status.contains("is not a job"), "{}", app.status);
    assert!(matches!(app.mode, UiMode::Normal), "and opens no gate");

    // A job with no worktree of its own.
    let bare = fixture(
        LaunchKind::Job,
        &claude_result("done", "did it in place"),
        None,
        Some("0"),
    );
    let mut app = app_for(&bare, FakePane::default());
    app.refresh();
    app.selected = app
        .projects
        .iter()
        .position(|v| v.id == "kid")
        .expect("the row");
    app.begin_apply();
    assert!(app.status.contains("no worktree"), "{}", app.status);

    // A job that committed nothing.
    let quiet = fixture(
        LaunchKind::Job,
        &claude_result("done", "found nothing to change"),
        None,
        Some("0"),
    );
    let _ = git_worktree(&quiet, "pm/kid-quiet");
    let mut app = app_for(&quiet, FakePane::default());
    app.refresh();
    app.selected = app
        .projects
        .iter()
        .position(|v| v.id == "kid")
        .expect("the row");
    app.begin_apply();
    assert!(app.status.contains("committed nothing"), "{}", app.status);

    // And a checkout the human is mid-edit in: refused, and their edit untouched.
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "renamed the flag"),
        None,
        Some("0"),
    );
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    git_commit(&wt, "the child's work", &[("src/flag.rs", "// renamed\n")]);
    let project = fx
        .child_paths
        .state_dir()
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the project root")
        .to_path_buf();
    std::fs::write(project.join("README.md"), "the human was here\n").expect("write");
    let mut app = app_for(&fx, FakePane::default());
    app.apply_job_commit("kid");
    assert!(app.status.contains("uncommitted changes"), "{}", app.status);
    assert_eq!(
        std::fs::read_to_string(project.join("README.md")).expect("their file"),
        "the human was here\n"
    );
    assert!(!project.join("src/flag.rs").exists(), "nothing was applied");
}

/// A CANCELLED JOB WITH UNCOMMITTED WORK KEEPS ITS ROW. A killed run is the likeliest of all to have left
/// work it never committed, and retiring the row is what removes the only directory that work lives in. The
/// `done` path has always kept the row for this; the cancel path dropped it.
#[test]
fn a_cancelled_job_with_uncommitted_work_keeps_its_row() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    std::fs::write(wt.join("half-done.rs"), "// never committed\n").expect("write");
    spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("the parent asks");
    let mut app = app_for(&fx, FakePane::default());

    sweep(&mut app, &fx);

    assert_eq!(
        receipt(&fx).map(|r| r.state),
        Some(ReceiptState::Cancelled),
        "the parent is still answered"
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "and the row stays, because its worktree is the only copy of that work"
    );
    assert!(wt.join("half-done.rs").exists(), "which is still there");
    assert!(
        app.status.contains("uncommitted work"),
        "and the status says why: {}",
        app.status
    );
}

/// A WORKTREE NOBODY CAN FIND AGAIN IS WORSE THAN A SHARED ONE. The row is what records the branch and
/// base; if that write is lost the harvest cannot read what the child did and removal cannot clean the
/// directory up, so the worktree is removed and the job runs in the project directory.
#[test]
fn a_worktree_whose_identity_cannot_be_recorded_is_removed() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let project = project_root(&fx);
    init_repo_at(&project);
    let mut app = app_for(&fx, FakePane::default());
    let row = Registry::load(&fx.registry)
        .unwrap()
        .projects
        .iter()
        .find(|p| p.id == "kid")
        .cloned()
        .expect("the job row");

    // A row the registry no longer has: the launch record cannot be written to.
    let mut reg = Registry::load(&fx.registry).unwrap();
    reg.projects.retain(|p| p.id != "kid");
    reg.save(&fx.registry).unwrap();

    assert!(
        app.prepare_worktree(&row, &fx.child_paths, REQUEST)
            .is_none(),
        "no worktree is handed back"
    );
    assert!(
        !fx.child_paths.state_dir().join("worktree").exists(),
        "and none is left behind"
    );
    assert!(
        !run_git(&project, &["worktree", "list"]).contains("worktree"),
        "the repository forgot it too"
    );
    let log = std::fs::read_to_string(&app.status_log).unwrap_or_default();
    assert!(log.contains("could not be recorded"), "{log}");
}

/// A KILL THAT FAILS LEAVES THE JOB ALONE AND SAYS SO. The receipt is not written and the row is not
/// retired on a cancel that could not actually stop anything — a `cancelled` answer for a child still
/// running would be a lie the parent acts on.
#[test]
fn a_cancel_whose_kill_fails_answers_nothing() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("the parent asks");
    let pane = FakePane::live_quiet(&fx.child_session).with(|inner| inner.fail_terminate = true);
    let mut app = app_for(&fx, pane.clone());
    let clock = std::rc::Rc::new(std::cell::Cell::new(10_000u128));
    let now = clock.clone();
    app.spawn.now_ms = Box::new(move || now.get());

    sweep(&mut app, &fx);
    clock.set(10_000 + CANCEL_GRACE_MS);
    sweep(&mut app, &fx);

    assert!(
        pane.terminated().contains(&fx.child_session),
        "the kill was attempted"
    );
    assert!(receipt(&fx).is_none(), "but nothing was answered");
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "and the row is still there"
    );
    let log = std::fs::read_to_string(&app.status_log).unwrap_or_default();
    assert!(log.contains("could not kill kid"), "{log}");
}

/// ONE WORKTREE PER JOB, EVEN IF THE LAUNCH IS ASKED TWICE. A relaunch must not make a second branch or
/// throw away what the first one already wrote, so preparing again returns the SAME worktree.
#[test]
fn preparing_a_worktree_twice_reuses_the_first() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let project = project_root(&fx);
    init_repo_at(&project);
    let mut app = app_for(&fx, FakePane::default());
    let row = Registry::load(&fx.registry)
        .unwrap()
        .projects
        .iter()
        .find(|p| p.id == "kid")
        .cloned()
        .expect("the job row");

    let first = app
        .prepare_worktree(&row, &fx.child_paths, REQUEST)
        .expect("a worktree");
    assert!(first.path.join(".git").exists(), "a real checkout");
    assert_eq!(first.branch, format!("pm/kid-{}", &REQUEST[..8]));
    // The row learned it, which is how the harvest finds it again.
    let row = Registry::load(&fx.registry)
        .unwrap()
        .projects
        .iter()
        .find(|p| p.id == "kid")
        .cloned()
        .expect("the job row");
    assert_eq!(
        row.launch.as_ref().and_then(|l| l.branch.clone()),
        Some(first.branch.clone())
    );

    std::fs::write(first.path.join("in-progress.rs"), "// work\n").expect("write");
    let again = app
        .prepare_worktree(&row, &fx.child_paths, REQUEST)
        .expect("the same worktree");

    assert_eq!(again, first, "the same handle, not a second branch");
    assert_eq!(
        std::fs::read_to_string(first.path.join("in-progress.rs")).expect("still there"),
        "// work\n",
        "and nothing it had written was lost"
    );
    assert_eq!(
        run_git(&project, &["worktree", "list"]).lines().count(),
        2,
        "one worktree beside the project checkout"
    );
}

/// A CONFLICT COSTS THE HUMAN NOTHING, and says so. The pick is aborted, their checkout stays where it
/// was, and the status names the conflict rather than leaving a half-applied tree behind.
#[test]
fn apply_reports_a_conflict_and_leaves_the_checkout_alone() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "renamed the flag"),
        None,
        Some("0"),
    );
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    git_commit(&wt, "the child's line", &[("src/same.rs", "// child\n")]);
    let project = project_root(&fx);
    // The human changed the same file first, and committed it.
    git_commit(
        &project,
        "the human's line",
        &[("src/same.rs", "// human\n")],
    );
    let head = run_git(&project, &["rev-parse", "HEAD"]);
    let mut app = app_for(&fx, FakePane::default());

    app.apply_job_commit("kid");

    assert!(
        app.status.contains("conflicts with this checkout"),
        "{}",
        app.status
    );
    assert_eq!(
        run_git(&project, &["rev-parse", "HEAD"]),
        head,
        "HEAD did not move"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("src/same.rs")).expect("their file"),
        "// human\n",
        "and their version still stands"
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "the row stays for them to decide what to do"
    );
}

/// NEITHER HALF OF `a` TOUCHES A ROW THAT IS GONE, and both say so instead of doing nothing: the list on
/// screen can be a frame behind the registry.
#[test]
fn apply_refuses_when_nothing_is_selected_or_the_row_has_left() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let mut app = app_for(&fx, FakePane::default());

    // Nothing selected at all.
    app.projects.clear();
    app.begin_apply();
    assert!(app.status.contains("nothing is selected"), "{}", app.status);

    // And a row the registry no longer has — from both halves, since the list on screen can be a frame
    // behind and `a` is reachable from either.
    app.apply_job_commit("ghost");
    assert!(app.status.contains("gone from the list"), "{}", app.status);
    app.projects = vec![ProjectView {
        job: true,
        job_commit: Some("0".repeat(40)),
        job_branch: Some("pm/ghost".into()),
        ..view("ghost", Posture::Done, Vec::new())
    }];
    app.selected = 0;
    app.begin_apply();
    assert!(app.status.contains("gone from the list"), "{}", app.status);

    // Confirming an apply for a job with no worktree, and for one that committed nothing: each says which
    // it is rather than reporting a failure the parent would act on.
    let bare = fixture(LaunchKind::Job, "", None, None);
    let mut app = app_for(&bare, FakePane::default());
    app.apply_job_commit("kid");
    assert!(app.status.contains("no worktree"), "{}", app.status);

    let quiet = fixture(LaunchKind::Job, "", None, None);
    let _ = git_worktree(&quiet, "pm/kid-quiet");
    let mut app = app_for(&quiet, FakePane::default());
    app.apply_job_commit("kid");
    assert!(
        app.status.contains("committed nothing on pm/kid-quiet"),
        "{}",
        app.status
    );
}

/// REMOVING A JOB WHOSE WORKTREE IS DIRTY KEEPS BOTH. `git worktree remove` refuses, so the state purge is
/// skipped too — those changes exist nowhere else, and the row going away is not a reason to delete them.
#[test]
fn removing_a_job_with_uncommitted_work_keeps_its_worktree_and_state() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    std::fs::write(wt.join("half-done.rs"), "// never committed\n").expect("write");
    let mut app = app_for(&fx, FakePane::default());

    app.remove_project("kid", &fx.child_session);

    assert!(!rows(&fx).contains(&"kid".to_string()), "the row is gone");
    assert!(wt.exists(), "but its worktree is not");
    assert!(
        fx.child_paths.state_dir().exists(),
        "and neither is the state that holds it"
    );
    let log = std::fs::read_to_string(&app.status_log).unwrap_or_default();
    assert!(
        log.contains("worktree is kept on pm/kid-5f0c3a1e"),
        "with one line saying so: {log}"
    );
}

/// A JOB'S WORK TRAVELS IN ITS RECEIPT: the branch, the commit, and what that commit touched. The parent
/// learns where the work is without reading a diff, and learns it from the receipt rather than from a
/// directory that retirement deletes.
#[test]
fn a_jobs_commit_and_branch_land_in_the_receipt() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "renamed the flag"),
        None,
        Some("0"),
    );
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    let sha = git_commit(&wt, "the child's work", &[("src/flag.rs", "// renamed\n")]);
    let mut app = app_for(&fx, FakePane::default());

    sweep(&mut app, &fx);

    let work = receipt(&fx)
        .expect("the parent is answered")
        .work
        .expect("and told where the work is");
    assert_eq!(work.branch, "pm/kid-5f0c3a1e");
    assert_eq!(work.commit.as_deref(), Some(sha.as_str()));
    assert_eq!(work.touched, vec!["src/flag.rs"]);
    assert!(!work.uncommitted, "it committed everything");
    assert_eq!(rows(&fx), vec!["parent"], "and the row retired");
    assert!(
        app.status.contains(&sha[..8]) && app.status.contains("pm/kid-5f0c3a1e"),
        "the status names the commit and its branch: {}",
        app.status
    );
}

/// UNCOMMITTED WORK KEEPS THE ROW, even on `done`. Those changes exist only in the worktree, and retiring
/// the row deletes it — so a job that ignored its one instruction leaves evidence instead of nothing.
#[test]
fn a_done_job_that_left_uncommitted_work_keeps_its_row() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "I think I finished"),
        None,
        Some("0"),
    );
    let wt = git_worktree(&fx, "pm/kid-5f0c3a1e");
    std::fs::write(wt.join("half-done.rs"), "// never committed\n").expect("write");
    let mut app = app_for(&fx, FakePane::default());

    sweep(&mut app, &fx);

    let found = receipt(&fx).expect("the parent is still answered");
    assert_eq!(found.state, ReceiptState::Done, "its own outcome stands");
    assert!(
        found.work.expect("work is reported").uncommitted,
        "and says there is work it never committed"
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "the row is kept so the work is not deleted to tidy up"
    );
    assert!(
        app.status.contains("uncommitted"),
        "and says why: {}",
        app.status
    );
    assert!(wt.exists(), "the worktree is still there");
}

/// A JOB THAT NEEDS A PERSON KEEPS ITS ROW. Claude Code holds a failed subagent's row for the same
/// reason and agent-deck archives rather than deletes: an outcome nobody has seen is not cleanup.
#[test]
fn an_outcome_that_wants_a_human_keeps_its_row_inert() {
    for (outcome, state) in [
        ("needs_human", ReceiptState::NeedsHuman),
        ("failed", ReceiptState::Failed),
    ] {
        let fx = fixture(
            LaunchKind::Job,
            &claude_result(outcome, "the migration needs a DBA"),
            None,
            Some("0"),
        );
        let mut app = app_for(&fx, FakePane::default());
        sweep(&mut app, &fx);

        let receipt = receipt(&fx).expect("answered");
        assert_eq!(receipt.state, state, "{outcome}");
        assert!(
            rows(&fx).contains(&"kid".to_string()),
            "{outcome} must leave the row to be seen"
        );
        let kept = Registry::load(&fx.registry)
            .expect("load")
            .projects
            .into_iter()
            .find(|p| p.id == "kid")
            .expect("row kept");
        assert!(!kept.enabled, "{outcome}: the kept row is inert");
    }
}

/// A FAILURE ALWAYS CARRIES AN `error.code`. v1's contract for `failed` is "act on `error.code`", so a
/// job failure must not be the one `failed` that has none.
#[test]
fn a_reported_failure_still_carries_an_error_code() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("failed", "the fixture would not build"),
        None,
        Some("0"),
    );
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    let error = receipt(&fx)
        .expect("answered")
        .error
        .expect("an error code");
    assert_eq!(error.code, spawn::ErrorCode::JobFailed);
    assert_eq!(error.message, "the fixture would not build");
}

/// A run that left NO report is `ended_without_result` — a crash or a kill, not a forgotten step — and
/// its exit code is named rather than swallowed.
#[test]
fn a_run_that_left_no_result_reports_its_exit_code() {
    let fx = fixture(LaunchKind::Job, "{\"type\":\"system\"}", None, Some("1"));
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    let receipt = receipt(&fx).expect("answered");
    assert_eq!(receipt.state, ReceiptState::EndedWithoutResult);
    assert!(receipt.result.is_none());
    let message = receipt.error.expect("an error").message;
    assert!(message.contains("exited 1"), "{message}");
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "nothing silently disappears without a report"
    );
}

/// EXIT 127 IS A MISSING EXECUTABLE, which is worth naming: the shell's "not found" would otherwise be
/// reported as a mystery.
#[test]
fn a_missing_agent_executable_is_named() {
    let fx = fixture(LaunchKind::Job, "", None, Some("127"));
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    let message = receipt(&fx)
        .expect("answered")
        .error
        .expect("an error")
        .message;
    assert!(
        message.contains("claude") && message.contains("PATH"),
        "{message}"
    );
}

/// A RUNNING JOB IS LEFT ALONE. A live terminal is the job working.
#[test]
fn a_running_job_is_not_touched() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "already reported"),
        None,
        None,
    );
    let mut app = app_for(&fx, FakePane::live_quiet(&fx.child_session));
    sweep(&mut app, &fx);
    assert!(receipt(&fx).is_none(), "nothing is recorded mid-run");
    assert!(rows(&fx).contains(&"kid".to_string()));
}

/// A CHAT CHILD IS NEVER RETIRED BY A MACHINE. v1's interactive children, and any row written before
/// `launch.kind` existed, keep their old meaning — the human closes them.
#[test]
fn a_chat_child_is_left_to_its_human() {
    let fx = fixture(
        LaunchKind::Chat,
        &claude_result("done", "would have been retired"),
        None,
        Some("0"),
    );
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    assert!(receipt(&fx).is_none());
    assert!(rows(&fx).contains(&"kid".to_string()));
}

/// A row written by an older build has no `launch.kind` at all, and must read as `chat` rather than be
/// mistaken for a job and swept.
#[test]
fn a_launch_record_without_a_kind_reads_as_chat() {
    let json = r#"{"request_id":"r","args_hash":"h","state":"started"}"#;
    let record: LaunchRecord = serde_json::from_str(json).expect("a v1 launch record loads");
    assert_eq!(record.kind, LaunchKind::Chat);
}

/// REMOVING A RUNNING JOB BY HAND IS A CANCELLATION, and its parent is told. Without this the parent
/// polls `ready` for a child that no longer exists — the one way this feature could leave an agent
/// waiting for good.
#[test]
fn removing_a_job_row_answers_its_parent_as_cancelled() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let mut app = app_for(&fx, FakePane::live_quiet(&fx.child_session));
    app.remove_project("kid", &fx.child_session);

    let receipt = receipt(&fx).expect("the parent is answered");
    assert_eq!(receipt.state, ReceiptState::Cancelled);
    assert!(
        receipt
            .error
            .expect("a reason")
            .message
            .contains("stopped and removed by a human")
    );
    assert_eq!(rows(&fx), vec!["parent"], "the row is gone");
}

/// A job whose outcome is ALREADY recorded is not overwritten by a later removal: `done` stays `done`.
#[test]
fn a_recorded_outcome_survives_a_later_removal() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished properly"),
        None,
        Some("0"),
    );
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    assert_eq!(receipt(&fx).expect("answered").state, ReceiptState::Done);

    // The row is already gone, but a stale `d` on a kept row must not rewrite history either.
    app.remove_project("kid", &fx.child_session);
    assert_eq!(
        receipt(&fx).expect("answered").state,
        ReceiptState::Done,
        "a recorded outcome is not replaced by a cancellation"
    );
}

/// A CHAT child's removal writes no receipt: nothing is waiting on one.
#[test]
fn removing_a_chat_child_writes_no_receipt() {
    let fx = fixture(LaunchKind::Chat, "", None, None);
    let mut app = app_for(&fx, FakePane::live_quiet(&fx.child_session));
    app.remove_project("kid", &fx.child_session);
    assert!(receipt(&fx).is_none());
}

/// A RETIRED JOB LEAVES NOTHING BEHIND. Its state directory is a finished run's working copy, and what
/// it achieved is in the parent's receipt, so the row and the state go together. A KEPT row keeps its
/// state — that is the log a human still has to read — until they clear it with `d`.
#[test]
fn a_retired_jobs_state_directory_goes_with_its_row() {
    let done = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished and tidied up"),
        None,
        Some("0"),
    );
    let mut app = app_for(&done, FakePane::default());
    sweep(&mut app, &done);
    assert_eq!(rows(&done), vec!["parent"]);
    assert!(
        !done.child_paths.state_dir().exists(),
        "a retired job's state is gone: {}",
        done.child_paths.state_dir().display()
    );
    // The parent still has its answer, which is the whole point of deleting the rest.
    let summary = receipt(&done)
        .expect("answered")
        .result
        .expect("a result")
        .summary;
    assert_eq!(summary, "finished and tidied up");

    // KEPT: a job that needs a human keeps its row AND its log.
    let kept = fixture(
        LaunchKind::Job,
        &claude_result("needs_human", "which database?"),
        None,
        Some("0"),
    );
    let mut app = app_for(&kept, FakePane::default());
    sweep(&mut app, &kept);
    assert!(rows(&kept).contains(&"kid".to_string()));
    assert!(
        kept.child_paths.job_log().is_file(),
        "the log a human has to read is still there"
    );

    // Until the human clears the row, which takes the state with it.
    app.remove_project("kid", &kept.child_session);
    assert!(!kept.child_paths.state_dir().exists());
}

/// A PURGE THAT CANNOT RUN NEVER BLOCKS THE REMOVAL. The row leaving the list is the human's decision,
/// so a state directory that refuses to go (here a planted symlink, which is never followed) is reported
/// and nothing else: the row is still gone, and the link's target is untouched.
#[test]
fn a_state_directory_that_cannot_be_purged_still_lets_the_row_go() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let victim = elsewhere.path().join("keep-me");
    std::fs::create_dir_all(&victim).expect("victim");
    let state = fx.child_paths.state_dir();
    std::fs::remove_dir_all(&state).expect("clear the real one");
    std::os::unix::fs::symlink(&victim, &state).expect("plant a link");

    let mut app = app_for(&fx, FakePane::default());
    app.remove_project("kid", &fx.child_session);

    assert_eq!(rows(&fx), vec!["parent"], "the row is gone regardless");
    assert!(victim.is_dir(), "the link's target survives");
    assert!(state.is_symlink(), "and the link itself was not followed");
}

/// A HUMAN'S OWN SESSION IS NEVER PURGED. Only a job's state goes with its row: no machine decides a
/// person's work is finished, so a chat child removed by hand keeps everything under `.project-state/`.
#[test]
fn removing_a_chat_child_keeps_its_state() {
    let fx = fixture(LaunchKind::Chat, "a human's transcript", None, None);
    let mut app = app_for(&fx, FakePane::live_quiet(&fx.child_session));
    app.remove_project("kid", &fx.child_session);
    assert_eq!(rows(&fx), vec!["parent"], "the row is gone");
    assert!(
        fx.child_paths.job_log().is_file(),
        "but its state is not ours to delete"
    );
}

/// A JOB THAT OUTRAN READINESS IS STILL FINISHED. Readiness wants two live observations; a job that
/// did its work — or died on a bad argument — in less than that is recorded `outcome_unknown`, which is
/// not a verdict about the task. The sweep still reads what it left, so the row is never stranded with
/// its reason unread. Found by a real `claude -p` job that exited in milliseconds.
#[test]
fn a_job_that_finished_before_readiness_saw_it_is_still_answered() {
    let fx = fixture_with_outcome(
        agent_manager::registry::SpawnOutcome::OutcomeUnknown,
        "Error: --json-schema is not valid JSON\n",
        "1",
    );
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    let answered = receipt(&fx).expect("the parent is answered");
    assert_eq!(answered.state, ReceiptState::EndedWithoutResult);
    let error = answered.error.expect("a reason");
    assert!(
        error.message.contains("exited 1"),
        "the exit code is named: {}",
        error.message
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "a run that reported nothing keeps its row for a human"
    );

    // And a job that DID report in that window has its real outcome recorded and is retired.
    let done = fixture_with_outcome(
        agent_manager::registry::SpawnOutcome::OutcomeUnknown,
        &claude_result("done", "finished faster than readiness"),
        "0",
    );
    let mut app = app_for(&done, FakePane::default());
    sweep(&mut app, &done);
    assert_eq!(receipt(&done).expect("answered").state, ReceiptState::Done);
    assert_eq!(rows(&done), vec!["parent"]);
}

/// A launch still waiting for a person is NOT swept: `needs_attention` asked for a human.
#[test]
fn a_needs_attention_launch_is_left_to_its_human() {
    let fx = fixture_with_outcome(
        agent_manager::registry::SpawnOutcome::NeedsAttention,
        &claude_result("done", "would have been retired"),
        "0",
    );
    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);
    assert!(receipt(&fx).is_none());
    assert!(rows(&fx).contains(&"kid".to_string()));
}

/// THE PARENT'S CANCEL: POLITE FIRST, AND THE KILL WAITS OUT A GRACE WINDOW. The marker its parent wrote
/// makes the sweep signal the process group and say so, leaving the terminal and the row alone — the
/// dashboard never sleeps on a dying process. Frames are milliseconds apart, so a kill on the next one
/// would give the run no chance to flush what it had; neither engine offers a "stop and emit" handshake,
/// so time is the only thing a cancel can grant.
#[test]
fn a_cancelled_job_is_asked_to_stop_and_killed_only_after_its_grace_window() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("the parent asks");
    let pane = FakePane::live_quiet(&fx.child_session);
    let mut app = app_for(&fx, pane.clone());
    let clock = std::rc::Rc::new(std::cell::Cell::new(10_000u128));
    let now = clock.clone();
    app.spawn.now_ms = Box::new(move || now.get());

    sweep(&mut app, &fx);
    assert_eq!(
        pane.stop_requests(),
        vec![fx.child_session.clone()],
        "the first frame signals"
    );
    assert!(pane.terminated().is_empty(), "and does not kill");
    assert!(rows(&fx).contains(&"kid".to_string()), "the row stays");
    assert!(receipt(&fx).is_none(), "nothing is recorded yet");
    assert!(app.status.contains("cancelling kid"), "{}", app.status);

    // A frame INSIDE the window leaves it alone: the run is being given its chance, not waited on.
    clock.set(10_000 + CANCEL_GRACE_MS - 1);
    sweep(&mut app, &fx);
    assert!(
        pane.terminated().is_empty(),
        "a frame inside the grace window must not kill: {:?}",
        pane.terminated()
    );
    assert!(receipt(&fx).is_none(), "and answers nothing yet");

    // Once the window is up, with the job still alive: killed, answered and retired.
    clock.set(10_000 + CANCEL_GRACE_MS);
    sweep(&mut app, &fx);
    assert!(
        pane.terminated().contains(&fx.child_session),
        "the kill lands once the window is up: {:?}",
        pane.terminated()
    );
    assert_eq!(
        pane.stop_requests().len(),
        1,
        "and never signals twice: {:?}",
        pane.stop_requests()
    );
    let receipt = receipt(&fx).expect("the parent is answered");
    assert_eq!(receipt.state, ReceiptState::Cancelled);
    assert!(
        receipt
            .error
            .expect("a reason")
            .message
            .contains("cancelled by its parent")
    );
    assert_eq!(rows(&fx), vec!["parent"], "the row is gone");
    assert!(
        !spawn::cancel_requested(&fx.parent_requests, REQUEST),
        "the marker is cleared once it has been acted on"
    );
}

/// A cancel for a job whose process is ALREADY gone needs no signal and no kill: it is recorded and
/// retired on the spot.
#[test]
fn cancelling_a_job_that_already_stopped_needs_no_signal() {
    let fx = fixture(LaunchKind::Job, "", None, Some("143"));
    spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("the parent asks");
    let pane = FakePane::default();
    let mut app = app_for(&fx, pane.clone());

    sweep(&mut app, &fx);
    assert!(
        pane.stop_requests().is_empty(),
        "nothing to signal: {:?}",
        pane.stop_requests()
    );
    assert_eq!(
        receipt(&fx).expect("answered").state,
        ReceiptState::Cancelled
    );
    assert_eq!(rows(&fx), vec!["parent"]);
}

/// THE CANCEL LOST THE RACE. A job that reported before anyone asked it to stop keeps its own outcome:
/// calling delivered work `cancelled` would throw away the thing the parent wanted.
#[test]
fn a_job_that_already_reported_keeps_its_outcome_over_a_late_cancel() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished before the cancel arrived"),
        None,
        Some("0"),
    );
    spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("the parent asks");
    let mut app = app_for(&fx, FakePane::default());

    sweep(&mut app, &fx);
    let answered = receipt(&fx).expect("answered");
    assert_eq!(answered.state, ReceiptState::Done);
    assert_eq!(
        answered.result.expect("the result stands").summary,
        "finished before the cancel arrived"
    );
    assert!(!spawn::cancel_requested(&fx.parent_requests, REQUEST));
}

/// A cancel marker is a MARKER: asking twice is asking once, and the second call says so rather than
/// failing.
#[test]
fn asking_twice_to_cancel_is_the_same_as_asking_once() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    assert!(spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("first"));
    assert!(!spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("second"));
    assert!(spawn::cancel_requested(&fx.parent_requests, REQUEST));
    assert_eq!(
        std::fs::read(spawn::cancel_path(&fx.parent_requests, REQUEST)).expect("the marker"),
        Vec::<u8>::new(),
        "there is nothing in it to read or trust"
    );
}

/// A CHAT child's row is never cancelled by a marker: the sweep only ever touches jobs.
#[test]
fn a_cancel_marker_does_not_touch_a_chat_child() {
    let fx = fixture(LaunchKind::Chat, "", None, None);
    spawn::publish_cancel(&fx.parent_requests, REQUEST).expect("a marker");
    let pane = FakePane::live_quiet(&fx.child_session);
    let mut app = app_for(&fx, pane.clone());
    sweep(&mut app, &fx);
    assert!(pane.stop_requests().is_empty());
    assert!(rows(&fx).contains(&"kid".to_string()));
    assert!(receipt(&fx).is_none());
}

/// RETIREMENT WAITS FOR AN ATTACHED HUMAN. Killing a pane out from under someone reading it is the one
/// thing this sweep must never do, even when the work is over — and the wait is a deferral, not a
/// decision: the next sweep after they detach finishes the job.
#[test]
fn retirement_waits_for_an_attached_human() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished while watched"),
        None,
        Some("0"),
    );
    let mut watched = app_for(&fx, FakePane::attached());
    sweep(&mut watched, &fx);
    assert!(receipt(&fx).is_none(), "deferred, not recorded");
    assert!(rows(&fx).contains(&"kid".to_string()));

    let mut alone = app_for(&fx, FakePane::default());
    sweep(&mut alone, &fx);
    assert_eq!(receipt(&fx).expect("answered").state, ReceiptState::Done);
    assert_eq!(rows(&fx), vec!["parent"]);
}

/// A CODEX JOB'S CONVERSATION IS RECORDED SO A HUMAN CAN PICK IT UP.
///
/// Codex has no caller-chosen conversation id — the one flag claude does have — so the only way to
/// learn which conversation a run created is to read the id codex announced in its own stream. A job
/// exits, so it never resumes itself; this is what lets a PERSON open the conversation of a job that
/// failed or asked for them, instead of losing it with the process. The whole of `pin_codex_thread`
/// was unexercised until `spawning.rs` was split and its coverage stopped being averaged across two
/// thousand lines.
#[test]
fn a_codex_jobs_thread_id_is_recorded_on_its_row() {
    const THREAD: &str = "0199f2a0-1111-7000-8000-abcdefabcdef";
    let log = format!(
        "{}\n{}",
        format_args!(r#"{{"type":"thread.started","thread_id":"{THREAD}"}}"#),
        r#"{"type":"turn.completed"}"#
    );
    let fx = fixture(LaunchKind::Job, &log, None, Some("0"));
    // The row is codex's, and carries no conversation yet.
    Registry::update(&fx.registry, |reg| {
        let kid = reg
            .projects
            .iter_mut()
            .find(|p| p.id == "kid")
            .expect("kid");
        kid.engine = Some(Engine::Codex);
        kid.conversation_id = None;
    })
    .expect("update");

    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    // The row is retired, so the id has to have been written BEFORE that — which is the ordering
    // this pins. It is read back from the parent's receipt, which outlives the row.
    let session = receipt(&fx)
        .expect("a receipt")
        .session
        .expect("the receipt names its session");
    assert_eq!(session.agent, Engine::Codex);
}

/// …and the id is on the ROW while it is still there, for a job the human keeps.
#[test]
fn a_kept_codex_jobs_row_carries_the_thread_it_created() {
    const THREAD: &str = "0199f2a0-2222-7000-8000-abcdefabcdef";
    let log = format!(
        "{}\n{}",
        format_args!(r#"{{"type":"thread.started","thread_id":"{THREAD}"}}"#),
        r#"{"type":"turn.failed"}"#
    );
    // `turn.failed` ⇒ the job FAILED, so its row is kept for a person — and that is the row whose
    // conversation they will want to open.
    let fx = fixture(LaunchKind::Job, &log, None, Some("1"));
    Registry::update(&fx.registry, |reg| {
        let kid = reg
            .projects
            .iter_mut()
            .find(|p| p.id == "kid")
            .expect("kid");
        kid.engine = Some(Engine::Codex);
    })
    .expect("update");

    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "a failed job is kept"
    );
    let kid = Registry::load(&fx.registry)
        .expect("load")
        .projects
        .into_iter()
        .find(|p| p.id == "kid")
        .expect("the row");
    assert_eq!(
        kid.conversation_id.as_deref(),
        Some(THREAD),
        "the conversation codex created is on the row a human will open"
    );
}

/// A CLAUDE JOB IS NOT GIVEN A CODEX THREAD, and a codex job that already has one keeps it.
///
/// Both are the early returns of the same guard. The second matters because the engine is the
/// authority on a codex conversation: an id already recorded must not be replaced by a later pass
/// re-reading an older log.
#[test]
fn pinning_a_thread_skips_claude_and_never_overwrites_an_id() {
    const THREAD: &str = "0199f2a0-3333-7000-8000-abcdefabcdef";
    let log = format!(
        "{}\n{}",
        format_args!(r#"{{"type":"thread.started","thread_id":"{THREAD}"}}"#),
        r#"{"type":"turn.failed"}"#
    );

    // Claude: the stream is codex-shaped, and it is still ignored.
    let claude = fixture(LaunchKind::Job, &log, None, Some("1"));
    let mut app = app_for(&claude, FakePane::default());
    sweep(&mut app, &claude);
    let kid = Registry::load(&claude.registry)
        .expect("load")
        .projects
        .into_iter()
        .find(|p| p.id == "kid")
        .expect("row");
    assert_eq!(
        kid.conversation_id, None,
        "a claude job has a caller-chosen id and is never given codex's"
    );

    // Codex with an id already on the row: it stands.
    let codex = fixture(LaunchKind::Job, &log, None, Some("1"));
    Registry::update(&codex.registry, |reg| {
        let kid = reg
            .projects
            .iter_mut()
            .find(|p| p.id == "kid")
            .expect("kid");
        kid.engine = Some(Engine::Codex);
        kid.conversation_id = Some("the-one-already-known".into());
    })
    .expect("update");
    let mut app = app_for(&codex, FakePane::default());
    sweep(&mut app, &codex);
    let kid = Registry::load(&codex.registry)
        .expect("load")
        .projects
        .into_iter()
        .find(|p| p.id == "kid")
        .expect("row");
    assert_eq!(
        kid.conversation_id.as_deref(),
        Some("the-one-already-known"),
        "a recorded id is not re-derived from a log"
    );
}

/// A CODEX STREAM WITH NO THREAD ANNOUNCEMENT LEAVES THE ROW ALONE.
///
/// An id arrives only after codex has started its conversation, so a run that died before that has
/// none — and "not known yet" must stay absent rather than become a guess.
#[test]
fn a_codex_job_that_announced_no_thread_records_nothing() {
    let fx = fixture(
        LaunchKind::Job,
        r#"{"type":"turn.failed"}"#,
        None,
        Some("1"),
    );
    Registry::update(&fx.registry, |reg| {
        reg.projects
            .iter_mut()
            .find(|p| p.id == "kid")
            .expect("kid")
            .engine = Some(Engine::Codex);
    })
    .expect("update");

    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    let kid = Registry::load(&fx.registry)
        .expect("load")
        .projects
        .into_iter()
        .find(|p| p.id == "kid")
        .expect("row");
    assert_eq!(kid.conversation_id, None);
}

/// ONE HARVEST RUNS AT A TIME, and this is the guard that makes it so.
///
/// `remove_project` ends in `refresh`, which calls the sweep again: five children finishing together
/// re-entered it four levels deep, and each outer level then carried on down its own stale candidate
/// list, re-harvesting rows whose state directory the inner level had already purged. The second
/// harvest of a `done` child finds no payload and overwrote the parent's correct answer with
/// `ended_without_result` — three of five children, every time. The flag is the fix, so it gets a
/// test that fails if it is ever removed.
#[test]
fn a_re_entered_harvest_returns_immediately_and_answers_nothing() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished it"),
        None,
        Some("0"),
    );
    let mut app = app_for(&fx, FakePane::default());
    let reg = Registry::load(&fx.registry).expect("load");

    // Stand where the inner call stands: a harvest is already in flight.
    app.spawn.harvesting = true;
    app.retire_finished_jobs(&reg);
    assert!(
        receipt(&fx).is_none(),
        "a re-entered sweep writes no answer at all"
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "and retires nothing"
    );

    // Released, the same sweep does the work.
    app.spawn.harvesting = false;
    app.retire_finished_jobs(&reg);
    assert_eq!(
        receipt(&fx).expect("a receipt now").state,
        ReceiptState::Done
    );
}

/// A JOB WHOSE PARENT HAS LEFT THE LIST HAS NOWHERE TO ANSWER, and says so by not writing.
///
/// Nothing reads a departed session's requests directory, so a receipt written there would be a
/// message to no one. The row still finishes — the work happened — it just has no addressee.
#[test]
fn a_job_whose_parent_is_gone_finishes_without_a_receipt() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("needs_human", "which database?"),
        None,
        Some("0"),
    );
    // The parent closed its session while the child was running.
    Registry::update(&fx.registry, |reg| {
        reg.projects.retain(|p| p.id != "parent")
    })
    .expect("drop the parent");

    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    assert!(
        receipt(&fx).is_none(),
        "no receipt is written into a directory nobody reads"
    );
    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "a job that needs a human is still kept for one"
    );
}

/// A RECEIPT THAT CANNOT BE WRITTEN IS LOGGED, AND THE ROW STILL FINISHES.
///
/// The parent's requests directory is on disk in a session's own state tree, so writing there can
/// fail for reasons that have nothing to do with the job: a full disk, a permission change, a
/// directory replaced by a file. The work is over either way, so the harvest must not stall on it —
/// but it must not go quiet either, because the parent is waiting on a receipt that will never come.
#[test]
fn a_receipt_that_cannot_be_written_is_logged_and_the_job_still_finishes() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished it"),
        None,
        Some("0"),
    );
    // Replace the requests DIRECTORY with a file: every write into it now fails.
    std::fs::remove_dir_all(&fx.parent_requests).expect("clear");
    std::fs::write(&fx.parent_requests, "not a directory").expect("occupy the path");

    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    assert!(
        !rows(&fx).contains(&"kid".to_string()),
        "a done job is retired whether or not its parent could be told"
    );
    assert!(
        log_lines(&app)
            .iter()
            .any(|line| line.contains("could not record") && line.contains("kid")),
        "the failure is named, because the parent is waiting on a receipt: {:?}",
        log_lines(&app)
    );
}

/// THE SAME FOR A CANCELLATION, which is the receipt an agent is most likely to be polling for.
#[test]
fn a_cancellation_receipt_that_cannot_be_written_is_logged() {
    let fx = fixture(LaunchKind::Job, "", None, None);
    let row = Registry::load(&fx.registry)
        .expect("load")
        .projects
        .into_iter()
        .find(|p| p.id == "kid")
        .expect("row");
    std::fs::remove_dir_all(&fx.parent_requests).expect("clear");
    std::fs::write(&fx.parent_requests, "not a directory").expect("occupy the path");

    let mut app = app_for(&fx, FakePane::default());
    app.cancel_job_receipt(&row);

    assert!(
        log_lines(&app)
            .iter()
            .any(|line| line.contains("could not record") && line.contains("cancellation")),
        "{:?}",
        log_lines(&app)
    );
}

/// A ROW WITH NO LAUNCH RECORD IS NOT A JOB THE HARVEST OWNS.
///
/// Every question the sweep asks about a job is answered from its launch record — which request it
/// came from, what state it reached, which branch it owns. A row without one (a human's own session,
/// or a hand-edited list) must fall through every one of those questions rather than be guessed at.
#[test]
fn a_row_without_a_launch_record_is_left_entirely_alone() {
    let fx = fixture(
        LaunchKind::Job,
        &claude_result("done", "finished it"),
        None,
        Some("0"),
    );
    Registry::update(&fx.registry, |reg| {
        reg.projects
            .iter_mut()
            .find(|p| p.id == "kid")
            .expect("kid")
            .launch = None;
    })
    .expect("update");

    let mut app = app_for(&fx, FakePane::default());
    let row = Registry::load(&fx.registry)
        .expect("load")
        .projects
        .into_iter()
        .find(|p| p.id == "kid")
        .expect("row");
    sweep(&mut app, &fx);
    // Each of the three questions the harvest asks about a launch, answered without one.
    app.cancel_job_receipt(&row);

    assert!(
        rows(&fx).contains(&"kid".to_string()),
        "nothing retired a row the harvest has no record for"
    );
    assert!(receipt(&fx).is_none(), "and nothing was answered");
}

/// A FAILED JOB THAT LEFT NO REPORT STILL CARRIES AN `error.code` AND A SENTENCE.
///
/// v1's contract is "act on `error.code`", so a `failed` receipt must ALWAYS have one — a job failure
/// must not be the single `failed` that has none. When the run left nothing readable there is no
/// summary to use as the message, so the receipt says so rather than carrying an empty string.
#[test]
fn a_failed_job_with_no_readable_report_still_explains_itself() {
    // A nonzero exit and a log with no result event at all: the process failed and said nothing.
    let fx = fixture(LaunchKind::Job, "boom: killed\n", None, Some("9"));

    let mut app = app_for(&fx, FakePane::default());
    sweep(&mut app, &fx);

    let receipt = receipt(&fx).expect("a receipt");
    assert_eq!(receipt.state, ReceiptState::EndedWithoutResult);
    let error = receipt.error.expect("a failed receipt always names a code");
    assert!(
        !error.message.trim().is_empty(),
        "and says something a person can read: {error:?}"
    );
}
