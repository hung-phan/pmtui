//! `pmtui spawn`: an agent inside a managed session asks the dashboard for a Standard child.
//!
//! The command parses its flags, reads exactly two env vars, publishes one request into its
//! own session's state dir and waits for the dashboard's receipt. These tests pin that it
//! never reaches anything else — no registry, no tmux, no dashboard singleton, no `$HOME` —
//! and that every outcome maps to the documented state, JSON envelope, line and exit code.

use super::*;
use crate::spawn_cli::*;
use agent_manager::registry::LaunchState;
use agent_manager::spawn::{
    self, ErrorCode, NextAction, NextActionKind, ReceiptSession, ReceiptState, SpawnArgs,
    SpawnError, SpawnReceipt, SpawnRequest,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;

const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
/// A second valid id, for a listing that must hold more than one child.
const OTHER_ID: &str = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";
const CREATED_AT: Epoch = 1_790_000_000;
const MESSAGE: &str = "Fix the flaky fork test.\nOwnership: src/bin/pmtui/tests/forking.rs only.";

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// `pmtui spawn <extra…>`, which must parse.
fn cli(extra: &[&str]) -> SpawnCli {
    match parse_action(&argv(&[&["spawn"][..], extra].concat())) {
        Ok(Action::Spawn(cli)) => cli,
        other => panic!("expected a spawn action, got {other:?}"),
    }
}

/// `pmtui spawn <extra…>`, which must be refused.
fn usage_error(extra: &[&str]) -> UsageError {
    match parse_action(&argv(&[&["spawn"][..], extra].concat())) {
        Err(error) => error,
        other => panic!("expected a usage error, got {other:?}"),
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Epoch {
        CREATED_AT
    }
}

/// Time moves only when the command sleeps, so a 20 s wait costs nothing. `on_sleep` runs
/// after the n-th sleep (1-based): that is where a test plays the dashboard.
struct FakeWaiter<'a> {
    now: u128,
    sleeps: Vec<u64>,
    on_sleep: Box<dyn FnMut(usize) + 'a>,
}

impl<'a> FakeWaiter<'a> {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(on_sleep: impl FnMut(usize) + 'a) -> Self {
        Self {
            now: 5_000,
            sleeps: Vec::new(),
            on_sleep: Box::new(on_sleep),
        }
    }

    fn slept_ms(&self) -> u64 {
        self.sleeps.iter().sum()
    }
}

impl Waiter for FakeWaiter<'_> {
    fn now_ms(&mut self) -> u128 {
        self.now
    }

    fn sleep_ms(&mut self, ms: u64) {
        self.sleeps.push(ms);
        self.now += u128::from(ms);
        (self.on_sleep)(self.sleeps.len());
    }
}

/// A managed session's state dir inside a scratch project, the env its terminal carries (with
/// `HOME` pointed at a read-only, empty dir), and a record of every env var the command read.
struct Session {
    _scratch: tempfile::TempDir,
    scratch: PathBuf,
    state_dir: PathBuf,
    home: PathBuf,
    vars: HashMap<String, String>,
    asked: RefCell<Vec<String>>,
}

impl Session {
    fn new() -> Self {
        let scratch_dir = tempfile::tempdir().unwrap();
        let scratch = scratch_dir.path().to_path_buf();
        let state_dir = scratch.join("service/.project-state/sessions/service-1a2b3c4d");
        std::fs::create_dir_all(&state_dir).unwrap();
        let home = scratch.join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
        let vars = HashMap::from([
            ("PMTUI_SESSION".to_string(), "service".to_string()),
            (
                "PMTUI_STATE_DIR".to_string(),
                state_dir.display().to_string(),
            ),
            ("PMTUI_BIN".to_string(), "/opt/am/pmtui".to_string()),
            ("HOME".to_string(), home.display().to_string()),
        ]);
        Self {
            _scratch: scratch_dir,
            scratch,
            state_dir,
            home,
            vars,
            asked: RefCell::new(Vec::new()),
        }
    }

    fn without(mut self, key: &str) -> Self {
        self.vars.remove(key);
        self
    }

    fn with_var(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_string(), value.to_string());
        self
    }

    fn env(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |key| {
            self.asked.borrow_mut().push(key.to_string());
            self.vars.get(key).cloned()
        }
    }

    fn requests(&self) -> PathBuf {
        spawn::requests_dir(&self.state_dir)
    }

    fn run(&self, cli: &SpawnCli, waiter: &mut dyn Waiter) -> CliOutput {
        run_spawn(cli, &self.env(), &FixedClock, waiter)
    }

    /// The whole command line, through `main`'s own dispatch: `(exit code, stdout, stderr)`.
    fn dispatch(&self, line: &[&str], waiter: &mut dyn Waiter) -> (i32, String, String) {
        let env = self.env();
        let mut deps = SpawnDeps {
            env: &env,
            clock: &FixedClock,
            waiter,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = dispatch(
            parse_action(&argv(line)),
            &mut |_| panic!("a spawn command must never open the dashboard"),
            &mut deps,
            &mut out,
            &mut err,
        )
        .unwrap();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn request_on_disk(&self, id: &str) -> SpawnRequest {
        serde_json::from_slice(&std::fs::read(spawn::request_path(&self.requests(), id)).unwrap())
            .unwrap()
    }

    /// Every file under the scratch dir, relative to it.
    fn files(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, base, found);
                } else {
                    found.push(path.strip_prefix(base).unwrap().display().to_string());
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.scratch, &self.scratch, &mut found);
        found.sort();
        found
    }
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

fn child_session() -> ReceiptSession {
    ReceiptSession {
        id: "service-2".into(),
        title: Some("Fix flaky fork test".into()),
        display_name: None,
        root: PathBuf::from("/workspace/service"),
        agent: Engine::Codex,
        model: None,
        spawned_by: "service".into(),
        tmux_session: "pm-service-2-1a2b3c4d".into(),
        state_dir: PathBuf::from("/workspace/service/.project-state/sessions/service-2-1a2b3c4d"),
    }
}

fn receipt(id: &str, state: ReceiptState) -> SpawnReceipt {
    SpawnReceipt {
        schema_version: spawn::RECEIPT_SCHEMA_VERSION,
        request_id: id.into(),
        state,
        claimed_by: Some("pid:4242".into()),
        session: Some(child_session()),
        launch_state: Some(LaunchState::Started),
        error: None,
        next_action: None,
        args_hash: None,
        result: None,
        work: None,
        updated_at: CREATED_AT + 4,
    }
}

fn output(state: CliState, receipt: Option<SpawnReceipt>, error: Option<SpawnError>) -> CliOutput {
    CliOutput {
        state,
        request_id: Some(REQUEST_ID.into()),
        receipt,
        error,
    }
}

#[test]
fn parse_accepts_every_documented_flag_and_defaults_the_rest() {
    let full = cli(&[
        "--message",
        "  do the thing  ",
        "--title",
        "Title",
        "--name",
        "Worker",
        "--dir",
        "/work/service",
        "--agent",
        "codex",
        "--model",
        "gpt-5",
        "--request-id",
        REQUEST_ID,
        "--wait",
        "120",
        "--json",
    ]);
    assert_eq!(
        full,
        SpawnCli {
            args: SpawnArgs {
                message: "  do the thing  ".into(),
                title: Some("Title".into()),
                name: Some("Worker".into()),
                dir: Some(PathBuf::from("/work/service")),
                agent: Some(Engine::Codex),
                model: Some("gpt-5".into()),
            },
            request_id: Some(REQUEST_ID.into()),
            wait_s: 120,
            json: true,
            cancel: false,
            status: false,
        }
    );

    let minimal = cli(&["--message", "m", "--agent", "claude", "--wait", "0"]);
    assert_eq!(minimal.args.agent, Some(Engine::Claude));
    assert_eq!(minimal.wait_s, 0);
    assert_eq!(cli(&["--message", "m"]).wait_s, DEFAULT_WAIT_S);
    assert_eq!(DEFAULT_WAIT_S, 20);
    let defaults = cli(&["--message", "m"]);
    assert_eq!(
        (defaults.args.title, defaults.request_id, defaults.json),
        (None, None, false)
    );

    for help in [["spawn", "--help"], ["spawn", "-h"]] {
        assert_eq!(parse_action(&argv(&help)), Ok(Action::SpawnHelp));
    }
    assert_eq!(
        parse_action(&argv(&["spawn", "--message", "m", "--help"])),
        Ok(Action::SpawnHelp)
    );
    assert_eq!(parse_action(&argv(&["--help"])), Ok(Action::Help));
    assert_eq!(parse_action(&argv(&["-h"])), Ok(Action::Help));
    match parse_action(&argv(&["--registry", "/tmp/r.json", "--socket", "s"])) {
        Ok(Action::Dashboard(args)) => {
            assert_eq!(args.registry, PathBuf::from("/tmp/r.json"));
            assert_eq!(args.socket, "s");
            assert!(!args.help);
        }
        other => panic!("expected the dashboard, got {other:?}"),
    }
}

#[test]
fn parse_rejects_missing_message_bad_uuid_unknown_agent_and_out_of_range_wait() {
    let cases: &[(&[&str], &str)] = &[
        (&[], "--message is required"),
        (&["--message"], "--message needs a value"),
        (&["--message", "  \n "], "--message must not be empty"),
        (
            &["--message", "m", "--request-id", "not-a-uuid"],
            "--request-id",
        ),
        (
            &[
                "--message",
                "m",
                "--request-id",
                "550E8400-E29B-41D4-A716-446655440000",
            ],
            "lowercase UUID",
        ),
        (
            &["--message", "m", "--agent", "gemini"],
            "--agent must be claude or codex",
        ),
        (&["--message", "m", "--wait", "121"], "--wait"),
        (&["--message", "m", "--wait", "-1"], "--wait"),
        (&["--message", "m", "--wait", "soon"], "--wait"),
        (&["--message", "m", "--title"], "--title needs a value"),
        (
            &["--message", "m", "--frobnicate"],
            "unknown argument \"--frobnicate\"",
        ),
        (
            &["--message", "a", "--message", "b"],
            "--message was given twice",
        ),
    ];
    for (extra, wanted) in cases {
        let error = usage_error(extra);
        assert!(
            error.message.contains(wanted),
            "{extra:?}: {:?} should mention {wanted:?}",
            error.message
        );
        assert!(!error.json, "{extra:?}: no --json, so a plain usage error");
    }

    let dashboard = parse_action(&argv(&["--frobnicate"])).unwrap_err();
    assert_eq!(dashboard.message, "unknown argument \"--frobnicate\"");
    assert!(!dashboard.json);
    assert!(
        parse_action(&argv(&["--json", "--frobnicate"]))
            .unwrap_err()
            .json
    );
}

#[test]
fn json_usage_error_prints_the_envelope_with_exit_2() {
    let session = Session::new();
    let (code, out, err) = session.dispatch(
        &["spawn", "--json", "--message", "m", "--wait", "500"],
        &mut FakeWaiter::new(),
    );
    assert_eq!(code, 2);
    assert_eq!(err, "");
    let doc = json(&out);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["state"], "usage_error");
    assert_eq!(doc["request_id"], serde_json::Value::Null);
    assert_eq!(doc["receipt"], serde_json::Value::Null);
    assert_eq!(doc["error"]["code"], "invalid_argument");
    assert!(
        doc["error"]["message"].as_str().unwrap().contains("--wait"),
        "{doc}"
    );

    // `--json` anywhere in argv, even before the value that broke the parse.
    let (code, out, _) = session.dispatch(&["spawn", "--json", "--agent"], &mut FakeWaiter::new());
    assert_eq!(
        (code, json(&out)["state"].as_str()),
        (2, Some("usage_error"))
    );

    // Without --json the refusal is one line on stderr and stdout stays empty.
    let (code, out, err) = session.dispatch(&["spawn", "--title", "t"], &mut FakeWaiter::new());
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("--message is required"), "{err}");
    assert!(err.contains("pmtui spawn --help"), "{err}");

    assert!(
        session.asked.borrow().is_empty(),
        "a usage error is decided before any env var is read"
    );
    assert!(
        !session.requests().exists(),
        "a usage error publishes nothing"
    );
}

/// A `ready` TOMBSTONE IS NOT A LICENCE TO RE-DISPATCH. "Was this id already answered?" decides whether
/// to publish; "does the caller have its answer?" decides whether to return. Conflating them made a
/// replay publish the request again under an id that had already created a child — one broker pass away
/// from a second one.
#[test]
fn a_replay_under_a_running_job_waits_without_publishing_again() {
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    // The dashboard launched a child for this id and cleaned the request file up.
    spawn::write_receipt(
        &session.requests(),
        &SpawnReceipt {
            args_hash: Some(recorded_args("m").args_hash()),
            ..receipt(REQUEST_ID, ReceiptState::Ready)
        },
    )
    .unwrap();

    let mut waiter = FakeWaiter::new();
    let out = session.run(
        &cli(&["--message", "m", "--request-id", REQUEST_ID, "--wait", "1"]),
        &mut waiter,
    );

    assert_eq!(out.state, CliState::InProgress, "it waits for the result");
    assert_eq!(waiter.slept_ms(), 1_000);
    assert!(
        !spawn::request_path(&session.requests(), REQUEST_ID).exists(),
        "and never publishes a second request under an id that already made a child"
    );

    // The job reports, and the same replay now returns that answer.
    spawn::write_receipt(
        &session.requests(),
        &SpawnReceipt {
            args_hash: Some(recorded_args("m").args_hash()),
            ..receipt(REQUEST_ID, ReceiptState::Done)
        },
    )
    .unwrap();
    let out = session.run(
        &cli(&["--message", "m", "--request-id", REQUEST_ID, "--wait", "1"]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.state, CliState::Receipt(ReceiptState::Done));
}

/// `--status` IS THE ANSWER TO "HOW LONG WILL IT TAKE?" — a parent cannot know, so it dispatches, does
/// its own work, and asks what became of ALL its children in one call with no ids to carry.
#[test]
fn status_lists_every_child_oldest_first_with_what_became_of_it() {
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    let running = SpawnReceipt {
        updated_at: 1_000,
        ..receipt(REQUEST_ID, ReceiptState::Ready)
    };
    let finished = SpawnReceipt {
        updated_at: 2_000,
        result: Some(agent_manager::spawn::JobResult {
            outcome: agent_manager::spawn::JobOutcome::Done,
            summary: "renamed the flag\nand updated its test".into(),
            detail: None,
        }),
        ..receipt(OTHER_ID, ReceiptState::Done)
    };
    spawn::write_receipt(&session.requests(), &finished).unwrap();
    spawn::write_receipt(&session.requests(), &running).unwrap();

    let (code, out, err) = session.dispatch(&["spawn", "--status"], &mut FakeWaiter::new());

    assert_eq!((code, err.as_str()), (0, ""), "a listing is not an outcome");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    // Oldest first: the newest line is the thing that just changed.
    assert!(
        lines[0].starts_with("running service-2"),
        "a `ready` job is RUNNING, not ready: {out}"
    );
    assert!(
        lines[1].starts_with("done service-2") && lines[1].ends_with("renamed the flag"),
        "a finished child reports its summary's first line: {out}"
    );
    assert!(
        lines[1].contains("\"Fix flaky fork test\""),
        "the title says which task it was: {out}"
    );
}

/// The JSON form is an ARRAY even for one child, and an empty directory says so in words.
#[test]
fn status_json_is_always_an_array_and_empty_says_so() {
    let session = Session::new();
    let (code, out, _) = session.dispatch(&["spawn", "--status", "--json"], &mut FakeWaiter::new());
    assert_eq!(code, 0);
    let doc = json(&out);
    assert_eq!(doc["children"].as_array().map(Vec::len), Some(0));
    assert_eq!(doc["schema_version"], 1);

    let (_, plain, _) = session.dispatch(&["spawn", "--status"], &mut FakeWaiter::new());
    assert!(plain.contains("has not spawned any"), "{plain}");

    std::fs::create_dir_all(session.requests()).unwrap();
    spawn::write_receipt(
        &session.requests(),
        &receipt(REQUEST_ID, ReceiptState::NeedsHuman),
    )
    .unwrap();
    let (code, out, _) = session.dispatch(&["spawn", "--status", "--json"], &mut FakeWaiter::new());
    assert_eq!(code, 0);
    let doc = json(&out);
    let children = doc["children"].as_array().expect("an array");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["state"], "needs_human");
    assert_eq!(children[0]["request_id"], REQUEST_ID);
    assert_eq!(children[0]["id"], "service-2");
}

/// A LISTING TAKES NO ARGUMENTS: an id or a message on that line would describe something it does not do.
#[test]
fn status_refuses_other_arguments_and_an_unmanaged_session() {
    for extra in [
        vec!["--message", "m"],
        vec!["--request-id", REQUEST_ID],
        vec!["--cancel"],
    ] {
        let line = [&["--status"][..], &extra].concat();
        let error = usage_error(&line);
        assert!(
            error.message.contains("--status"),
            "{extra:?} was accepted: {}",
            error.message
        );
    }

    // Outside a managed session there is nothing to list, and it says which variables were missing.
    let session = Session::new().without("PMTUI_SESSION");
    let (code, out, err) = session.dispatch(&["spawn", "--status"], &mut FakeWaiter::new());
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("PMTUI_SESSION"), "{err}");
}

/// EVERY STATE HAS A WORD A PARENT CAN ACT ON, and none of them is "ready": for a job that means the run
/// is still going, which is the one word that would send a parent off to read a transcript again.
#[test]
fn every_receipt_state_has_a_plain_status_word() {
    use crate::spawn_status::status_word;

    for (state, word) in [
        (ReceiptState::Claimed, "starting"),
        (ReceiptState::Staged, "starting"),
        (ReceiptState::Launching, "starting"),
        (ReceiptState::Ready, "running"),
        (ReceiptState::Done, "done"),
        (ReceiptState::NeedsHuman, "needs a human"),
        (ReceiptState::Failed, "failed"),
        (ReceiptState::EndedWithoutResult, "ended without a result"),
        (ReceiptState::Cancelled, "cancelled"),
        (ReceiptState::NeedsAttention, "needs attention"),
        (ReceiptState::OutcomeUnknown, "outcome unknown"),
    ] {
        assert_eq!(status_word(state), word, "{state:?}");
    }
}

/// TWO CHILDREN ANSWERED IN THE SAME SECOND still list in a FIXED order, because a parent that reads
/// this listing twice and sees its children swap places cannot tell a reordering from a change.
#[test]
fn children_updated_in_the_same_second_order_by_request_id() {
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    for id in [OTHER_ID, REQUEST_ID] {
        spawn::write_receipt(&session.requests(), &receipt(id, ReceiptState::Done)).unwrap();
    }

    let (_, first, _) = session.dispatch(&["spawn", "--status", "--json"], &mut FakeWaiter::new());
    let (_, again, _) = session.dispatch(&["spawn", "--status", "--json"], &mut FakeWaiter::new());

    let ids = |out: &str| {
        json(out)["children"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|child| child["request_id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&first),
        vec![REQUEST_ID.to_string(), OTHER_ID.to_string()]
    );
    assert_eq!(ids(&first), ids(&again), "the same listing reads the same");
}

/// A SPARSE RECEIPT still produces a usable line: a request answered before any row existed has no child
/// id and no title, so the line falls back to the request id and says only what it knows.
#[test]
fn status_renders_a_receipt_with_no_session_or_result() {
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    spawn::write_receipt(
        &session.requests(),
        &SpawnReceipt {
            session: None,
            error: Some(SpawnError {
                code: ErrorCode::ChildLimitReached,
                message: "bot already has five children".into(),
            }),
            result: None,
            ..receipt(REQUEST_ID, ReceiptState::Failed)
        },
    )
    .unwrap();

    let (code, out, _) = session.dispatch(&["spawn", "--status"], &mut FakeWaiter::new());

    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        format!("failed {REQUEST_ID} \u{2014} bot already has five children"),
        "the error stands in for a result the child never reported"
    );

    // And the JSON form carries the same absences honestly rather than inventing an id.
    let (_, out, _) = session.dispatch(&["spawn", "--status", "--json"], &mut FakeWaiter::new());
    let child = &json(&out)["children"][0];
    assert!(child["id"].is_null() && child["title"].is_null(), "{out}");
    assert_eq!(child["summary"], "bot already has five children");
}

/// `--cancel` NAMES A CHILD and nothing else: it needs the request id, and refuses the flags that
/// describe a new one, so a caller cannot believe it re-dispatched the task.
#[test]
fn cancel_needs_a_request_id_and_takes_no_other_arguments() {
    let cancel = cli(&["--request-id", REQUEST_ID, "--cancel"]);
    assert!(cancel.cancel);
    assert_eq!(cancel.request_id.as_deref(), Some(REQUEST_ID));
    assert_eq!(
        cancel.args,
        SpawnArgs::default(),
        "no arguments are carried"
    );

    assert!(
        usage_error(&["--cancel"])
            .message
            .contains("--cancel needs the --request-id")
    );
    for extra in [
        vec!["--message", "m"],
        vec!["--title", "t"],
        vec!["--agent", "codex"],
        vec!["--wait", "5"],
    ] {
        let line = [&["--request-id", REQUEST_ID, "--cancel"][..], &extra].concat();
        let error = usage_error(&line);
        assert!(
            error.message.contains("cannot be used with --cancel"),
            "{} was accepted: {}",
            extra[0],
            error.message
        );
    }
}

/// A cancel of a RUNNING child writes the marker and reports `cancel_requested` with exit 3: the
/// dashboard does the stopping, so the caller polls the receipt as it does for everything else.
#[test]
fn cancel_writes_the_marker_and_reports_cancel_requested_exit_3() {
    let session = Session::new();
    let published = session.run(
        &cli(&["--message", MESSAGE, "--request-id", REQUEST_ID]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(
        published.state,
        CliState::Queued,
        "staged for the dashboard"
    );

    let (code, out, _) = session.dispatch(
        &["spawn", "--request-id", REQUEST_ID, "--cancel", "--json"],
        &mut FakeWaiter::new(),
    );
    assert_eq!(code, 3, "{out}");
    let doc = json(&out);
    assert_eq!(doc["state"], "cancel_requested");
    assert_eq!(doc["request_id"], REQUEST_ID);
    assert!(
        spawn::cancel_requested(&session.requests(), REQUEST_ID),
        "the marker is on disk"
    );
    assert!(
        !out.contains(MESSAGE),
        "a cancel must not echo the Message: {out}"
    );

    // Asking again is the same ask, not an error.
    let (again, _, _) = session.dispatch(
        &["spawn", "--request-id", REQUEST_ID, "--cancel", "--json"],
        &mut FakeWaiter::new(),
    );
    assert_eq!(again, 3);
}

/// REPORTED, NEVER INVENTED: a cancel for an id this session never asked for writes no marker, and a
/// cancel for a job that already ended returns that outcome rather than claiming a cancellation.
#[test]
fn cancel_refuses_an_unknown_request_and_reports_a_finished_one() {
    let session = Session::new();
    let (code, out, _) = session.dispatch(
        &["spawn", "--request-id", REQUEST_ID, "--cancel", "--json"],
        &mut FakeWaiter::new(),
    );
    assert_eq!(code, 1, "{out}");
    let doc = json(&out);
    assert_eq!(doc["state"], "failed");
    assert_eq!(doc["error"]["code"], "invalid_argument");
    assert!(
        !spawn::cancel_path(&session.requests(), REQUEST_ID).exists(),
        "an unknown id leaves no marker to strand"
    );

    // A job that already reported `done` is reported as `done`, with no marker written.
    std::fs::create_dir_all(session.requests()).unwrap();
    spawn::write_receipt(
        &session.requests(),
        &receipt(REQUEST_ID, ReceiptState::Done),
    )
    .unwrap();
    let (code, out, _) = session.dispatch(
        &["spawn", "--request-id", REQUEST_ID, "--cancel", "--json"],
        &mut FakeWaiter::new(),
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(json(&out)["state"], "done");
    assert!(
        !spawn::cancel_requested(&session.requests(), REQUEST_ID),
        "finished work is not cancelled"
    );
}

#[test]
fn missing_session_env_is_not_in_a_session_exit_1() {
    let unset = [
        Session::new().without("PMTUI_SESSION"),
        Session::new().without("PMTUI_STATE_DIR"),
        Session::new().with_var("PMTUI_SESSION", ""),
        Session::new().with_var("PMTUI_STATE_DIR", "relative/state"),
        Session::new().with_var("PMTUI_STATE_DIR", "/nonexistent/am-spawn-state"),
    ];
    for session in unset {
        let (code, out, _) = session.dispatch(
            &[
                "spawn",
                "--message",
                "m",
                "--json",
                "--request-id",
                REQUEST_ID,
            ],
            &mut FakeWaiter::new(),
        );
        assert_eq!(code, 1, "{out}");
        let doc = json(&out);
        assert_eq!(doc["state"], "failed");
        assert_eq!(doc["error"]["code"], "not_in_a_session");
        assert_eq!(doc["request_id"], REQUEST_ID);
        assert!(!session.requests().exists(), "nothing is published");
    }

    let session = Session::new().without("PMTUI_SESSION");
    let out = session.run(&cli(&["--message", "m"]), &mut FakeWaiter::new());
    assert_eq!(out.state, CliState::Failed);
    assert_eq!(exit_code(&out), 1);
    let error = out.error.expect("an error explains the refusal");
    assert!(error.message.contains("PMTUI_SESSION"), "{}", error.message);
}

#[test]
fn publishes_then_times_out_as_queued_exit_3_without_touching_home() {
    let session = Session::new();
    let mut waiter = FakeWaiter::new();
    let out = session.run(
        &cli(&[
            "--message",
            MESSAGE,
            "--title",
            "  Fix flaky fork test  ",
            "--request-id",
            REQUEST_ID,
        ]),
        &mut waiter,
    );

    assert_eq!(out.state, CliState::Queued);
    assert_eq!(exit_code(&out), 3);
    assert_eq!(out.request_id.as_deref(), Some(REQUEST_ID));
    assert_eq!((out.receipt, out.error), (None, None));
    assert_eq!(waiter.slept_ms(), 20_000, "waits the default 20 s");
    assert!(
        waiter.sleeps.iter().all(|ms| *ms <= 250),
        "polls every 250 ms: {:?}",
        waiter.sleeps
    );

    let request = session.request_on_disk(REQUEST_ID);
    assert_eq!(
        request,
        SpawnRequest {
            schema_version: spawn::SCHEMA_VERSION,
            request_id: REQUEST_ID.into(),
            parent_session: "service".into(),
            created_at: CREATED_AT,
            args: SpawnArgs {
                message: MESSAGE.into(),
                title: Some("Fix flaky fork test".into()),
                name: None,
                dir: None,
                agent: None,
                model: None,
            },
        }
    );
    assert_eq!(
        session.files(),
        vec![format!(
            "service/.project-state/sessions/service-1a2b3c4d/spawn-requests/{REQUEST_ID}.request.json"
        )],
        "the request is the only file written, inside the session's own state dir"
    );
    assert_eq!(
        std::fs::read_dir(&session.home).unwrap().count(),
        0,
        "HOME stays untouched"
    );
    let asked = session.asked.borrow().clone();
    assert!(
        asked
            .iter()
            .all(|key| key == "PMTUI_SESSION" || key == "PMTUI_STATE_DIR"),
        "the command reads only its two routing vars, never HOME: {asked:?}"
    );
}

#[test]
fn returns_the_receipt_written_during_the_wait_exit_0_for_done() {
    let session = Session::new();
    let requests = session.requests();
    // The dashboard's two writes, as a parent sees them: the job starts (`ready`, which `--wait` does
    // NOT stop at), then it reports. Only the second ends the wait.
    let mut waiter = FakeWaiter::with(|n| {
        if n == 2 {
            spawn::write_receipt(&requests, &receipt(REQUEST_ID, ReceiptState::Ready)).unwrap();
        }
        if n == 4 {
            spawn::write_receipt(&requests, &receipt(REQUEST_ID, ReceiptState::Done)).unwrap();
        }
    });
    let (code, out, err) = session.dispatch(
        &[
            "spawn",
            "--message",
            MESSAGE,
            "--request-id",
            REQUEST_ID,
            "--json",
        ],
        &mut waiter,
    );

    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(
        waiter.sleeps.len(),
        4,
        "waits through `ready` and stops when the job reports"
    );
    let doc = json(&out);
    assert_eq!(doc["state"], "done");
    assert_eq!(doc["request_id"], REQUEST_ID);
    assert_eq!(doc["receipt"]["state"], "done");
    assert_eq!(doc["receipt"]["session"]["id"], "service-2");
    assert_eq!(doc["receipt"]["session"]["spawned_by"], "service");
    assert_eq!(doc["error"], serde_json::Value::Null);
    assert!(
        !out.contains("flaky fork test.") && !out.contains("Ownership"),
        "the Message is never echoed: {out}"
    );

    // A final failure is final too, and its error is lifted to the top of the envelope.
    let session = Session::new();
    let requests = session.requests();
    let mut failed = receipt(REQUEST_ID, ReceiptState::Failed);
    failed.session = None;
    failed.error = Some(SpawnError {
        code: ErrorCode::ChildLimitReached,
        message: "service already has 5 spawned sessions".into(),
    });
    let mut waiter = FakeWaiter::with(|_| spawn::write_receipt(&requests, &failed).unwrap());
    let out = session.run(
        &cli(&["--message", "m", "--request-id", REQUEST_ID]),
        &mut waiter,
    );
    assert_eq!(out.state, CliState::Receipt(ReceiptState::Failed));
    assert_eq!(exit_code(&out), 1);
    assert_eq!(out.error.unwrap().code, ErrorCode::ChildLimitReached);
    assert_eq!(waiter.sleeps.len(), 1);
}

#[test]
fn a_non_final_receipt_at_deadline_is_in_progress() {
    let session = Session::new();
    let requests = session.requests();
    let mut waiter = FakeWaiter::with(|n| {
        if n == 1 {
            spawn::write_receipt(&requests, &receipt(REQUEST_ID, ReceiptState::Launching)).unwrap();
        }
    });
    let (code, out, _) = session.dispatch(
        &[
            "spawn",
            "--message",
            "m",
            "--request-id",
            REQUEST_ID,
            "--wait",
            "1",
            "--json",
        ],
        &mut waiter,
    );
    assert_eq!(code, 3);
    assert_eq!(waiter.slept_ms(), 1_000);
    let doc = json(&out);
    assert_eq!(doc["state"], "in_progress");
    assert_eq!(doc["receipt"]["state"], "launching");

    // A receipt the command cannot read counts as no receipt yet: the dashboard replaces it.
    let session = Session::new();
    let requests = session.requests();
    let mut waiter = FakeWaiter::with(|n| {
        if n == 1 {
            std::fs::write(spawn::receipt_path(&requests, REQUEST_ID), "{ torn").unwrap();
        }
    });
    let out = session.run(
        &cli(&["--message", "m", "--request-id", REQUEST_ID, "--wait", "1"]),
        &mut waiter,
    );
    assert_eq!((out.state, out.receipt), (CliState::Queued, None));
}

#[test]
fn identical_replay_waits_and_different_args_is_request_conflict() {
    let session = Session::new();
    let first = cli(&[
        "--message",
        "m",
        "--title",
        "t",
        "--request-id",
        REQUEST_ID,
        "--wait",
        "0",
    ]);
    assert_eq!(
        session.run(&first, &mut FakeWaiter::new()).state,
        CliState::Queued,
        "--wait 0 returns right after publishing"
    );
    let published = std::fs::read(spawn::request_path(&session.requests(), REQUEST_ID)).unwrap();

    // Same id, same args after normalization: the agent is checking on its own request.
    let replay = cli(&[
        "--message",
        " m ",
        "--title",
        "t  ",
        "--request-id",
        REQUEST_ID,
        "--wait",
        "1",
    ]);
    let mut waiter = FakeWaiter::new();
    let out = session.run(&replay, &mut waiter);
    assert_eq!((out.state, out.error), (CliState::Queued, None));
    assert_eq!(waiter.slept_ms(), 1_000, "an identical replay waits again");

    // A `ready` receipt is the job RUNNING, not an answer, so a replay keeps waiting for one.
    spawn::write_receipt(
        &session.requests(),
        &receipt(REQUEST_ID, ReceiptState::Ready),
    )
    .unwrap();
    let mut waiter = FakeWaiter::new();
    let out = session.run(&replay, &mut waiter);
    assert_eq!(out.state, CliState::InProgress);
    assert_eq!(waiter.slept_ms(), 1_000, "a running job is waited on");

    // Once the job has reported, a replay returns that answer without waiting.
    spawn::write_receipt(
        &session.requests(),
        &receipt(REQUEST_ID, ReceiptState::Done),
    )
    .unwrap();
    let mut waiter = FakeWaiter::new();
    let out = session.run(&replay, &mut waiter);
    assert_eq!(out.state, CliState::Receipt(ReceiptState::Done));
    assert!(waiter.sleeps.is_empty());

    // Same id, different args: refused, and the published request is left exactly as it was.
    let conflict = cli(&["--message", "something else", "--request-id", REQUEST_ID]);
    let mut waiter = FakeWaiter::new();
    let out = session.run(&conflict, &mut waiter);
    assert_eq!(out.state, CliState::Failed);
    assert_eq!(exit_code(&out), 1);
    assert_eq!(out.error.unwrap().code, ErrorCode::RequestConflict);
    assert!(waiter.sleeps.is_empty());
    assert_eq!(
        std::fs::read(spawn::request_path(&session.requests(), REQUEST_ID)).unwrap(),
        published
    );

    // A request under that id from another session is a conflict too.
    let other = Session::new();
    std::fs::create_dir_all(other.requests()).unwrap();
    std::fs::write(
        spawn::request_path(&other.requests(), REQUEST_ID),
        &published,
    )
    .unwrap();
    let sibling = other.with_var("PMTUI_SESSION", "sibling");
    let out = sibling.run(&first, &mut FakeWaiter::new());
    assert_eq!(out.error.unwrap().code, ErrorCode::RequestConflict);
}

#[test]
fn arguments_are_validated_and_normalized_before_anything_is_published() {
    let refusals: &[(&[&str], ErrorCode, &str)] = &[
        (
            &["--title", &"x".repeat(121)],
            ErrorCode::InvalidArgument,
            "120 characters",
        ),
        (
            &["--title", "line one\u{7}bell"],
            ErrorCode::InvalidArgument,
            "control characters",
        ),
        (
            &["--name", &"n".repeat(65)],
            ErrorCode::InvalidArgument,
            "64 characters",
        ),
        (&["--dir", ""], ErrorCode::InvalidArgument, "--dir"),
    ];
    for (extra, code, wanted) in refusals {
        let session = Session::new();
        let line = [&["--message", "m"][..], extra].concat();
        let out = session.run(&cli(&line), &mut FakeWaiter::new());
        assert_eq!(out.state, CliState::Failed, "{extra:?}");
        let error = out.error.unwrap();
        assert_eq!(error.code, *code, "{extra:?}");
        assert!(
            error.message.contains(wanted),
            "{extra:?}: {}",
            error.message
        );
        assert!(!session.requests().exists(), "{extra:?} published nothing");
    }

    // Exactly 120 characters is fine; a relative dir is made absolute, never canonicalized.
    let session = Session::new();
    let title = "t".repeat(120);
    let out = session.run(
        &cli(&[
            "--message",
            "m",
            "--title",
            &title,
            "--name",
            " Worker ",
            "--dir",
            "sub/../child",
            "--model",
            "  ",
            "--request-id",
            REQUEST_ID,
            "--wait",
            "0",
        ]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.state, CliState::Queued);
    let args = session.request_on_disk(REQUEST_ID).args;
    assert_eq!(args.title.as_deref(), Some(title.as_str()));
    assert_eq!(args.name.as_deref(), Some("Worker"));
    assert_eq!(args.model, None);
    assert_eq!(
        args.dir,
        Some(std::env::current_dir().unwrap().join("sub/../child"))
    );
}

#[test]
fn a_request_the_dashboard_could_never_read_or_write_is_refused_by_the_command() {
    let session = Session::new();
    let huge = "x".repeat(spawn::MAX_REQUEST_BYTES as usize);
    let out = session.run(
        &cli(&["--message", &huge, "--request-id", REQUEST_ID]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.state, CliState::Failed);
    assert_eq!(out.error.unwrap().code, ErrorCode::MessageTooLong);
    assert!(!session.requests().exists());

    let session = Session::new();
    std::fs::write(session.requests(), "a file where the requests dir belongs").unwrap();
    let out = session.run(
        &cli(&["--message", "m", "--request-id", REQUEST_ID]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.state, CliState::Failed);
    assert_eq!(exit_code(&out), 1);
    assert_eq!(out.error.unwrap().code, ErrorCode::RequestUnwritable);
}

#[test]
fn plain_output_matches_the_spec_line() {
    let ready = output(
        CliState::Receipt(ReceiptState::Ready),
        Some(receipt(REQUEST_ID, ReceiptState::Ready)),
        None,
    );
    assert_eq!(
        render_plain(&ready),
        r#"spawned service-2 "Fix flaky fork test" (codex, from service): ready"#
    );

    // End to end, the line a parent actually reads: the wait ends at the job's own report.
    let session = Session::new();
    let requests = session.requests();
    let mut waiter = FakeWaiter::with(|_| {
        spawn::write_receipt(&requests, &receipt(REQUEST_ID, ReceiptState::Done)).unwrap();
    });
    let (code, out, _) = session.dispatch(
        &["spawn", "--message", "m", "--request-id", REQUEST_ID],
        &mut waiter,
    );
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "spawned service-2 \"Fix flaky fork test\" (codex, from service): done\n"
    );
}

#[test]
fn plain_output_names_the_state_the_error_and_the_next_step_for_every_outcome() {
    let mut attention = receipt(REQUEST_ID, ReceiptState::NeedsAttention);
    attention.session.as_mut().unwrap().title = None;
    attention.next_action = Some(NextAction {
        kind: NextActionKind::Attach,
        argv: argv(&[
            "tmux",
            "-L",
            "pmd",
            "attach-session",
            "-t",
            "=pm-service-2-1a2b3c4d",
        ]),
    });
    assert_eq!(
        render_plain(&output(
            CliState::Receipt(ReceiptState::NeedsAttention),
            Some(attention),
            None
        )),
        "spawned service-2 (codex, from service): needs_attention; next: \
         'tmux' '-L' 'pmd' 'attach-session' '-t' '=pm-service-2-1a2b3c4d'"
    );

    let unknown = SpawnError {
        code: ErrorCode::ReadinessUnknown,
        message: "the terminal did not settle".into(),
    };
    assert_eq!(
        render_plain(&output(
            CliState::Receipt(ReceiptState::OutcomeUnknown),
            Some(receipt(REQUEST_ID, ReceiptState::OutcomeUnknown)),
            Some(unknown),
        )),
        "spawned service-2 \"Fix flaky fork test\" (codex, from service): outcome_unknown — \
         readiness_unknown: the terminal did not settle"
    );

    let conflict = SpawnError {
        code: ErrorCode::RequestConflict,
        message: "different arguments".into(),
    };
    assert_eq!(
        render_plain(&output(CliState::Failed, None, Some(conflict))),
        format!("spawn {REQUEST_ID}: failed — request_conflict: different arguments")
    );

    let queued = render_plain(&output(CliState::Queued, None, None));
    assert!(
        queued.starts_with(&format!("spawn {REQUEST_ID}: queued — ")),
        "{queued}"
    );
    assert!(
        queued.contains(&format!("same arguments and --request-id {REQUEST_ID}")),
        "{queued}"
    );
    let waiting = render_plain(&output(
        CliState::InProgress,
        Some(receipt(REQUEST_ID, ReceiptState::Staged)),
        None,
    ));
    assert!(
        waiting.starts_with(
            "spawned service-2 \"Fix flaky fork test\" (codex, from service): in_progress — "
        ),
        "{waiting}"
    );
    assert!(
        waiting.ends_with(&format!("--request-id {REQUEST_ID} to check again")),
        "a named child still shows the id to check on: {waiting}"
    );
    let mut anonymous = output(CliState::Queued, None, None);
    anonymous.request_id = None;
    let anonymous = render_plain(&anonymous);
    assert!(anonymous.contains("--request-id <id>"), "{anonymous}");

    let usage = CliOutput::usage(&UsageError {
        message: "spawn: --message is required".into(),
        json: false,
    });
    assert_eq!(
        render_plain(&usage),
        "spawn: usage_error — invalid_argument: spawn: --message is required"
    );
}

#[test]
fn every_state_has_its_documented_exit_code_and_json_name() {
    let cases = [
        (CliState::Receipt(ReceiptState::Ready), 0, "ready"),
        (
            CliState::Receipt(ReceiptState::NeedsAttention),
            3,
            "needs_attention",
        ),
        (CliState::Receipt(ReceiptState::Failed), 1, "failed"),
        (
            CliState::Receipt(ReceiptState::OutcomeUnknown),
            1,
            "outcome_unknown",
        ),
        (CliState::Receipt(ReceiptState::Claimed), 3, "in_progress"),
        (CliState::Receipt(ReceiptState::Staged), 3, "in_progress"),
        (CliState::Receipt(ReceiptState::Launching), 3, "in_progress"),
        (CliState::Queued, 3, "queued"),
        (CliState::InProgress, 3, "in_progress"),
        (CliState::Failed, 1, "failed"),
        (CliState::UsageError, 2, "usage_error"),
    ];
    for (state, code, name) in cases {
        let out = output(state, None, None);
        assert_eq!(exit_code(&out), code, "{state:?}");
        let doc = json(&render_json(&out));
        assert_eq!(doc["state"], name, "{state:?}");
        assert_eq!(doc["schema_version"], 1);
        let keys: Vec<&str> = doc
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys.len(),
            5,
            "schema_version, request_id, state, receipt, error: {keys:?}"
        );
    }
}

#[test]
fn generated_request_id_is_a_valid_uuid_and_is_printed() {
    let session = Session::new();
    let (code, out, _) = session.dispatch(
        &["spawn", "--message", "m", "--wait", "0", "--json"],
        &mut FakeWaiter::new(),
    );
    assert_eq!(code, 3);
    let doc = json(&out);
    assert_eq!(doc["state"], "queued");
    let id = doc["request_id"].as_str().unwrap().to_string();
    assert!(spawn::is_valid_request_id(&id), "{id}");
    assert_eq!(session.request_on_disk(&id).request_id, id);

    let (_, plain, _) = session.dispatch(
        &["spawn", "--message", "m", "--wait", "0"],
        &mut FakeWaiter::new(),
    );
    let second = plain
        .strip_prefix("spawn ")
        .and_then(|rest| rest.split(':').next())
        .unwrap_or_else(|| panic!("{plain}"));
    assert!(spawn::is_valid_request_id(second), "{plain}");
    assert_ne!(
        second, id,
        "every run without --request-id names a new operation"
    );
}

#[test]
fn help_goes_to_stderr_and_exits_0_without_starting_anything() {
    let session = Session::new();
    let (code, out, err) = session.dispatch(&["spawn", "--help"], &mut FakeWaiter::new());
    assert_eq!((code, out.as_str()), (0, ""));
    for flag in [
        "--message",
        "--title",
        "--name",
        "--dir",
        "--agent",
        "--model",
        "--request-id",
        "--wait",
        "--json",
        "PMTUI_SESSION",
        "PMTUI_STATE_DIR",
    ] {
        assert!(err.contains(flag), "spawn help names {flag}: {err}");
    }

    let (code, out, err) = session.dispatch(&["--help"], &mut FakeWaiter::new());
    assert_eq!((code, out.as_str()), (0, ""));
    assert!(
        err.contains("--registry") && err.contains("pmtui spawn"),
        "{err}"
    );
    assert!(
        err.contains("\n       pmtui spawn --message")
            && crate::spawn_cli::HELP.contains("\n                   [--dir"),
        "usage continuation lines keep their alignment: {err}"
    );

    let (code, _, err) = session.dispatch(&["--frobnicate"], &mut FakeWaiter::new());
    assert_eq!(code, 2);
    assert!(err.contains("unknown argument \"--frobnicate\""), "{err}");
    assert!(session.asked.borrow().is_empty());
}

#[test]
fn the_dashboard_action_runs_the_dashboard_and_reports_its_result() {
    let env = |_: &str| -> Option<String> { panic!("the dashboard path reads no spawn env") };
    let mut waiter = FakeWaiter::new();
    let mut deps = SpawnDeps {
        env: &env,
        clock: &FixedClock,
        waiter: &mut waiter,
    };
    let args = Args {
        registry: PathBuf::from("/tmp/r.json"),
        socket: "s".into(),
        help: false,
    };
    let mut seen = None;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = dispatch(
        Ok(Action::Dashboard(args)),
        &mut |args| {
            seen = Some(args.socket);
            Ok(())
        },
        &mut deps,
        &mut out,
        &mut err,
    )
    .unwrap();
    assert_eq!((code, seen.as_deref()), (0, Some("s")));

    let args = Args {
        registry: PathBuf::from("/tmp/r.json"),
        socket: "s".into(),
        help: false,
    };
    let error = dispatch(
        Ok(Action::Dashboard(args)),
        &mut |_| anyhow::bail!("dashboard failed"),
        &mut deps,
        &mut out,
        &mut err,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "dashboard failed");
    assert!(out.is_empty() && err.is_empty());
}

#[test]
fn the_process_waiter_and_env_are_real_and_bounded() {
    let mut waiter = ThreadWaiter::new();
    let before = waiter.now_ms();
    waiter.sleep_ms(2);
    assert!(waiter.now_ms() >= before + 2);
    assert_eq!(process_env("PMTUI_TEST_SURELY_UNSET_VARIABLE"), None);
    assert!(process_env("PATH").is_some());
}

/// The normalized arguments `pmtui spawn --message <message>` records, as the dashboard hashes them.
fn recorded_args(message: &str) -> SpawnArgs {
    SpawnArgs {
        message: message.into(),
        title: None,
        name: None,
        dir: None,
        agent: None,
        model: None,
    }
}

#[test]
fn a_replay_whose_request_was_cleaned_up_returns_its_final_receipt() {
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    // A tombstone for a job that FINISHED: that is the answer a replay gets back.
    let tombstone = SpawnReceipt {
        args_hash: Some(recorded_args("m").args_hash()),
        ..receipt(REQUEST_ID, ReceiptState::Done)
    };
    spawn::write_receipt(&session.requests(), &tombstone).unwrap();

    let mut waiter = FakeWaiter::new();
    let out = session.run(
        &cli(&["--message", " m ", "--request-id", REQUEST_ID]),
        &mut waiter,
    );
    assert_eq!(out.state, CliState::Receipt(ReceiptState::Done));
    assert_eq!(exit_code(&out), 0);
    assert_eq!(out.receipt, Some(tombstone.clone()));
    assert!(waiter.sleeps.is_empty(), "the answer is already final");
    assert!(
        !spawn::request_path(&session.requests(), REQUEST_ID).exists(),
        "nothing is published again"
    );

    // A receipt from before `args_hash` existed answers the same way.
    let old = SpawnReceipt {
        args_hash: None,
        ..tombstone
    };
    spawn::write_receipt(&session.requests(), &old).unwrap();
    let out = session.run(
        &cli(&["--message", "anything", "--request-id", REQUEST_ID]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.state, CliState::Receipt(ReceiptState::Done));
    assert!(!spawn::request_path(&session.requests(), REQUEST_ID).exists());
}

#[test]
fn other_arguments_under_an_answered_id_are_a_conflict_even_after_cleanup() {
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    for state in [ReceiptState::Ready, ReceiptState::Staged] {
        spawn::write_receipt(
            &session.requests(),
            &SpawnReceipt {
                args_hash: Some(recorded_args("the original task").args_hash()),
                ..receipt(REQUEST_ID, state)
            },
        )
        .unwrap();
        let mut waiter = FakeWaiter::new();
        let out = session.run(
            &cli(&["--message", "another task", "--request-id", REQUEST_ID]),
            &mut waiter,
        );
        assert_eq!(out.state, CliState::Failed, "{state:?}");
        assert_eq!(out.error.unwrap().code, ErrorCode::RequestConflict);
        assert!(waiter.sleeps.is_empty());
        assert!(
            !spawn::request_path(&session.requests(), REQUEST_ID).exists(),
            "{state:?}: a conflict publishes nothing"
        );
    }
}

#[test]
fn an_unfinished_receipt_for_the_same_arguments_is_waited_on() {
    let session = Session::new();
    let requests = session.requests();
    std::fs::create_dir_all(&requests).unwrap();
    spawn::write_receipt(
        &requests,
        &SpawnReceipt {
            args_hash: Some(recorded_args("m").args_hash()),
            ..receipt(REQUEST_ID, ReceiptState::Staged)
        },
    )
    .unwrap();
    let mut waiter = FakeWaiter::with(|_| {
        spawn::write_receipt(&requests, &receipt(REQUEST_ID, ReceiptState::Done)).unwrap();
    });
    let out = session.run(
        &cli(&["--message", "m", "--request-id", REQUEST_ID]),
        &mut waiter,
    );
    assert_eq!(out.state, CliState::Receipt(ReceiptState::Done));
    assert!(spawn::request_path(&session.requests(), REQUEST_ID).exists());
}

#[test]
fn a_conflict_prints_the_existing_receipt() {
    // The request file is still there with other arguments, and the dashboard answered it.
    let session = Session::new();
    let first = cli(&[
        "--message",
        "the original task",
        "--request-id",
        REQUEST_ID,
        "--wait",
        "0",
    ]);
    session.run(&first, &mut FakeWaiter::new());
    let answered = receipt(REQUEST_ID, ReceiptState::Ready);
    spawn::write_receipt(&session.requests(), &answered).unwrap();
    let (code, out, _) = session.dispatch(
        &[
            "spawn",
            "--message",
            "another task",
            "--request-id",
            REQUEST_ID,
            "--json",
        ],
        &mut FakeWaiter::new(),
    );
    assert_eq!(code, 1);
    let printed = json(&out);
    assert_eq!(printed["state"], "failed");
    assert_eq!(printed["error"]["code"], "request_conflict");
    assert_eq!(
        serde_json::from_value::<SpawnReceipt>(printed["receipt"].clone()).unwrap(),
        answered,
        "the agent sees what its id already created"
    );

    // Found through the receipt alone, after the request file was cleaned up.
    let session = Session::new();
    std::fs::create_dir_all(session.requests()).unwrap();
    let tombstone = SpawnReceipt {
        args_hash: Some(recorded_args("the original task").args_hash()),
        ..answered.clone()
    };
    spawn::write_receipt(&session.requests(), &tombstone).unwrap();
    let out = session.run(
        &cli(&["--message", "another task", "--request-id", REQUEST_ID]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.error.unwrap().code, ErrorCode::RequestConflict);
    assert_eq!(out.receipt, Some(tombstone));

    // With no receipt yet, the conflict carries none.
    let session = Session::new();
    session.run(&first, &mut FakeWaiter::new());
    let out = session.run(
        &cli(&["--message", "another task", "--request-id", REQUEST_ID]),
        &mut FakeWaiter::new(),
    );
    assert_eq!(out.error.unwrap().code, ErrorCode::RequestConflict);
    assert_eq!(out.receipt, None);
}
