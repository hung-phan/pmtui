//! `pmtui spawn`: how an agent inside a managed session asks the running dashboard for a
//! Standard child session.
//!
//! The command is deliberately small. It parses its flags, reads exactly two environment
//! variables (`PMTUI_SESSION`, `PMTUI_STATE_DIR`), publishes one request file into its own
//! session's state directory, and waits a bounded time for the dashboard's receipt beside it.
//! It never reads or writes the registry, never calls tmux, never takes the dashboard
//! singleton and never resolves `$HOME`, so it works from a sandbox whose home is read-only.
//! The dashboard is the only process that turns the request into a registry row.
//!
//! Every outcome is one [`CliOutput`]: the receipt's own state once the dashboard has answered,
//! or `queued` / `in_progress` when the wait ran out first, or a command-side `failed` /
//! `usage_error`. It renders as one JSON document ([`render_json`]) or one line
//! ([`render_plain`]), and maps to the documented exit code ([`exit_code`]). The Message is
//! never echoed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use agent_manager::clock::Clock;
use agent_manager::job_engine;
use agent_manager::registry::{Engine, normalize_display_name};
use agent_manager::spawn::{
    self, ErrorCode, Publish, ReceiptState, SpawnArgs, SpawnError, SpawnReceipt, SpawnRequest,
};
use agent_manager::tmux::{self, ENV_SESSION, ENV_STATE_DIR};

/// How long the command waits for a final receipt when `--wait` is not given.
pub(crate) const DEFAULT_WAIT_S: u64 = 20;
/// The longest `--wait` accepted.
pub(crate) const MAX_WAIT_S: u64 = 120;
/// How often the receipt is re-read while waiting.
pub(crate) const POLL_MS: u64 = 250;

pub(crate) const HELP: &str = concat!(
    "pmtui spawn — ask the running dashboard for a Standard child session\n\n",
    "Usage: pmtui spawn --message <text> [--title <text>] [--name <display-name>]\n",
    "                   [--dir <existing-directory>] [--agent claude|codex] [--model <id>]\n",
    "                   [--request-id <uuid>] [--wait <seconds>] [--json]\n",
    "       pmtui spawn --request-id <uuid> --cancel [--json]\n",
    "       pmtui spawn --status [--json]\n\n",
    "Run it inside a pmtui-managed session. It reads PMTUI_SESSION and PMTUI_STATE_DIR, writes\n",
    "one request into that session's state dir, and waits for the dashboard to answer it.\n\n",
    "--message <text>      the child's one-time first message (required)\n",
    "--title <text>        the Task title, at most 120 characters (default: the message's first line)\n",
    "--name <name>         a display name for the child\n",
    "--dir <path>          an existing folder inside this session's folder, or another session's\n",
    "                      root (default: this session's folder)\n",
    "--agent claude|codex  the child's agent (default: this session's)\n",
    "--model <id>          the child's model (default: this session's, when the agent matches)\n",
    "--request-id <uuid>   name the operation; rerun with the same id to check on it (default: new)\n",
    "--wait <seconds>      how long to wait for the child's RESULT, 0 to 120 (default: 20); a job\n",
    "                      that is still running reports in_progress, so rerun with the same\n",
    "                      --request-id to check again\n",
    "--status              list every child this session asked for, with what became of each; takes\n",
    "                      no other argument, and is how to check back when a task's length is\n",
    "                      unknown\n",
    "--cancel              stop the child this --request-id names; the dashboard signals it, then\n",
    "                      kills it, and its receipt becomes `cancelled`\n",
    "--json                print the result as one JSON document\n\n",
    "Exit codes: 0 ready or done; 3 queued, in_progress, needs_attention, needs_human or\n",
    "cancel_requested; 1 failed, outcome_unknown, ended_without_result or cancelled; 2 usage error.\n",
);

/// A parsed `pmtui spawn` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnCli {
    /// The arguments exactly as given; [`run_spawn`] normalizes them. Empty under `cancel`, which
    /// publishes no request.
    pub args: SpawnArgs,
    pub request_id: Option<String>,
    pub wait_s: u64,
    pub json: bool,
    /// Stop the child `request_id` names instead of asking for one.
    pub cancel: bool,
    /// List every child this session asked for instead of asking for one.
    pub status: bool,
}

/// A command line that cannot run. `json` is set when `--json` appears anywhere in it, so an
/// agent that asked for JSON gets JSON even for a malformed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageError {
    pub message: String,
    pub json: bool,
}

/// What the command reports: the receipt's state, or one the command synthesized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliState {
    Receipt(ReceiptState),
    /// The request is durable, but no dashboard claimed it before the deadline.
    Queued,
    /// A dashboard is working on it, but its receipt was not final at the deadline.
    InProgress,
    UsageError,
    /// The command itself refused, before or while publishing; nothing ran.
    Failed,
    /// A cancel was recorded for a child that has not stopped yet.
    CancelRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliOutput {
    pub state: CliState,
    pub request_id: Option<String>,
    pub receipt: Option<SpawnReceipt>,
    pub error: Option<SpawnError>,
}

impl CliOutput {
    /// The envelope a usage error prints under `--json`.
    pub(crate) fn usage(error: &UsageError) -> Self {
        Self {
            state: CliState::UsageError,
            request_id: None,
            receipt: None,
            error: Some(SpawnError {
                code: ErrorCode::InvalidArgument,
                message: error.message.clone(),
            }),
        }
    }

    fn failed(request_id: &str, code: ErrorCode, message: String) -> Self {
        Self {
            state: CliState::Failed,
            request_id: Some(request_id.to_string()),
            receipt: None,
            error: Some(SpawnError { code, message }),
        }
    }
}

/// The command's only notion of time while it waits, so a test can wait 20 s instantly.
pub(crate) trait Waiter {
    /// Milliseconds on a monotonic clock.
    fn now_ms(&mut self) -> u128;
    fn sleep_ms(&mut self, ms: u64);
}

/// The real [`Waiter`]: a monotonic clock and a blocking sleep. The command is a short-lived
/// process of its own, so blocking here never stalls a dashboard.
pub(crate) struct ThreadWaiter {
    started: Instant,
}

impl ThreadWaiter {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Waiter for ThreadWaiter {
    fn now_ms(&mut self) -> u128 {
        self.started.elapsed().as_millis()
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// What [`run_spawn`] reads from its process, bundled so `main` hands them over in one piece.
pub(crate) struct SpawnDeps<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub clock: &'a dyn Clock,
    pub waiter: &'a mut dyn Waiter,
}

/// The process environment, as [`run_spawn`] reads it. A value that is not UTF-8 is absent.
pub(crate) fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// True when `--json` appears anywhere in `argv`.
pub(crate) fn wants_json(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--json")
}

const VALUE_FLAGS: [&str; 8] = [
    "--message",
    "--title",
    "--name",
    "--dir",
    "--agent",
    "--model",
    "--request-id",
    "--wait",
];

/// Parse the arguments after `spawn`. `Ok(None)` asks for the spawn help. Only syntax is
/// checked here; the environment and the argument values are checked by [`run_spawn`].
pub(crate) fn parse(argv: &[String]) -> Result<Option<SpawnCli>, UsageError> {
    let usage = |message: String| UsageError {
        message: format!("spawn: {message}"),
        json: wants_json(argv),
    };
    let mut values: Vec<(&str, String)> = Vec::new();
    let mut json = false;
    let mut cancel = false;
    let mut status = false;
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        match flag {
            "-h" | "--help" => return Ok(None),
            "--json" => json = true,
            "--cancel" => cancel = true,
            "--status" => status = true,
            _ if VALUE_FLAGS.contains(&flag) => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| usage(format!("{flag} needs a value")))?;
                if values.iter().any(|(seen, _)| *seen == flag) {
                    return Err(usage(format!("{flag} was given twice")));
                }
                values.push((flag, value.clone()));
                i += 1;
            }
            other => return Err(usage(format!("unknown argument {other:?}"))),
        }
        i += 1;
    }
    let mut take = |flag: &str| {
        values
            .iter()
            .position(|(seen, _)| *seen == flag)
            .map(|at| values.swap_remove(at).1)
    };
    if status {
        // A LISTING TAKES NO ARGUMENTS. It is about every child this session has, so an id or a message
        // on this line would describe something it does not do.
        if cancel {
            return Err(usage(
                "--status and --cancel ask for different things".into(),
            ));
        }
        if let Some((flag, _)) = values.first() {
            return Err(usage(format!("{flag} cannot be used with --status")));
        }
        return Ok(Some(SpawnCli {
            args: SpawnArgs::default(),
            request_id: None,
            wait_s: DEFAULT_WAIT_S,
            json,
            cancel: false,
            status: true,
        }));
    }
    if cancel {
        // A CANCEL NAMES A CHILD; it does not describe one. Taking `--message` here would let a
        // cancel carry arguments nobody reads, which is how a caller ends up believing it re-dispatched.
        let request_id = take("--request-id")
            .ok_or_else(|| usage("--cancel needs the --request-id of the child to stop".into()))?;
        let request_id = parse_request_id(request_id).map_err(usage)?;
        if let Some((flag, _)) = values.first() {
            return Err(usage(format!("{flag} cannot be used with --cancel")));
        }
        return Ok(Some(SpawnCli {
            args: SpawnArgs::default(),
            request_id: Some(request_id),
            wait_s: DEFAULT_WAIT_S,
            json,
            cancel: true,
            status: false,
        }));
    }
    let message = take("--message").ok_or_else(|| usage("--message is required".into()))?;
    if message.trim().is_empty() {
        return Err(usage("--message must not be empty".into()));
    }
    let agent = take("--agent")
        .map(|value| parse_agent(&value))
        .transpose()
        .map_err(usage)?;
    let request_id = take("--request-id")
        .map(parse_request_id)
        .transpose()
        .map_err(usage)?;
    let wait_s = take("--wait")
        .map(|value| parse_wait(&value))
        .transpose()
        .map_err(usage)?
        .unwrap_or(DEFAULT_WAIT_S);
    Ok(Some(SpawnCli {
        args: SpawnArgs {
            message,
            title: take("--title"),
            name: take("--name"),
            dir: take("--dir").map(PathBuf::from),
            agent,
            model: take("--model"),
        },
        request_id,
        wait_s,
        json,
        cancel: false,
        status: false,
    }))
}

fn parse_agent(value: &str) -> Result<Engine, String> {
    Engine::ALL
        .into_iter()
        .find(|engine| engine.label() == value)
        .ok_or_else(|| format!("--agent must be claude or codex, not {value:?}"))
}

fn parse_request_id(value: String) -> Result<String, String> {
    if spawn::is_valid_request_id(&value) {
        Ok(value)
    } else {
        Err(format!(
            "--request-id must be a lowercase UUID, not {value:?}"
        ))
    }
}

fn parse_wait(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= MAX_WAIT_S)
        .ok_or_else(|| {
            format!(
                "--wait must be a whole number of seconds from 0 to {MAX_WAIT_S}, not {value:?}"
            )
        })
}

/// Publish the request and wait for its receipt. Never touches anything but the calling
/// session's `spawn-requests/` directory.
pub(crate) fn run_spawn(
    cli: &SpawnCli,
    env: &dyn Fn(&str) -> Option<String>,
    clock: &dyn Clock,
    waiter: &mut dyn Waiter,
) -> CliOutput {
    let request_id = cli
        .request_id
        .clone()
        .unwrap_or_else(job_engine::mint_uuid_v4);
    let (parent_session, state_dir) = match session_env(env) {
        Ok(found) => found,
        Err(message) => return CliOutput::failed(&request_id, ErrorCode::NotInASession, message),
    };
    if cli.cancel {
        return run_cancel(&spawn::requests_dir(&state_dir), &request_id);
    }
    let args = match checked_args(&cli.args) {
        Ok(args) => args,
        Err(message) => {
            return CliOutput::failed(&request_id, ErrorCode::InvalidArgument, message);
        }
    };
    let request = SpawnRequest {
        schema_version: spawn::SCHEMA_VERSION,
        request_id: request_id.clone(),
        parent_session,
        created_at: clock.now(),
        args,
    };
    // The dashboard refuses to read a request over the cap, so one that big would only ever
    // come back as `invalid_request`. The length is measured as `publish_request` writes it.
    let bytes = serde_json::to_vec_pretty(&request).map_or(0, |json| json.len() + 1);
    if bytes as u64 > spawn::MAX_REQUEST_BYTES {
        return CliOutput::failed(
            &request_id,
            ErrorCode::MessageTooLong,
            format!(
                "the request would be {bytes} bytes; the dashboard reads at most {} — shorten --message",
                spawn::MAX_REQUEST_BYTES
            ),
        );
    }
    let dir = spawn::requests_dir(&state_dir);
    // A receipt outlives its request (the dashboard keeps it as a tombstone), so it answers a
    // replay even after the request file was cleaned up: other arguments under its id are a
    // conflict, and a final answer is returned rather than asked again.
    if let Some(previous) = spawn::read_receipt(&dir, &request_id).ok().flatten() {
        let hash = request.args.args_hash();
        if previous
            .args_hash
            .as_deref()
            .is_some_and(|answered| answered != hash)
        {
            return conflict(&request_id, Some(previous));
        }
        // TWO DIFFERENT QUESTIONS, and conflating them re-dispatched a task. "Was this id already
        // answered?" decides whether to publish, and that stays `is_final()`: a `ready` receipt whose
        // request file was cleaned up means a child WAS created for this id, so publishing again could
        // make a second one. "Does the caller have its answer?" decides whether to return now, and that
        // is `answered()`: a job's `ready` means the run started, so wait for its result instead.
        if previous.state.is_final() && !request_file_present(&dir, &request_id) {
            return if answered(previous.state) {
                waited(&request_id, Some(previous), true)
            } else {
                wait_for_receipt(&dir, &request_id, cli.wait_s, waiter)
            };
        }
    }
    match spawn::publish_request(&dir, &request) {
        Ok(Publish::Published) => {}
        Ok(Publish::AlreadyPresent(existing)) if same_request(&existing, &request) => {}
        Ok(Publish::AlreadyPresent(_)) => {
            return conflict(
                &request_id,
                spawn::read_receipt(&dir, &request_id).ok().flatten(),
            );
        }
        Err(error) => {
            return CliOutput::failed(
                &request_id,
                ErrorCode::RequestUnwritable,
                format!("{error:#}"),
            );
        }
    }
    wait_for_receipt(&dir, &request_id, cli.wait_s, waiter)
}

/// Record a cancel for `request_id` and report where that request stands.
///
/// REPORTED, NEVER INVENTED. An id this session never asked for gets no marker and an honest refusal: a
/// marker for a request that does not exist would sit in the directory forever. A request whose job has
/// already ended is returned as it is — a `done` child cannot be un-finished, and saying `cancelled`
/// would misreport work that was actually delivered. Otherwise the marker is written (idempotently) and
/// the state is `cancel_requested`: the dashboard, not this command, does the stopping, so the caller
/// polls the receipt as it does for everything else.
fn run_cancel(dir: &Path, request_id: &str) -> CliOutput {
    let receipt = spawn::read_receipt(dir, request_id).ok().flatten();
    if receipt.is_none() && !request_file_present(dir, request_id) {
        return CliOutput::failed(
            request_id,
            ErrorCode::InvalidArgument,
            format!(
                "this session never asked for request {request_id}, so there is nothing to cancel"
            ),
        );
    }
    if let Some(found) = receipt.as_ref().filter(|r| r.state.is_job_terminal()) {
        return CliOutput {
            state: CliState::Receipt(found.state),
            request_id: Some(request_id.to_string()),
            receipt: receipt.clone(),
            error: found.error.clone(),
        };
    }
    match spawn::publish_cancel(dir, request_id) {
        Ok(_) => CliOutput {
            state: CliState::CancelRequested,
            request_id: Some(request_id.to_string()),
            receipt,
            error: None,
        },
        Err(error) => CliOutput::failed(
            request_id,
            ErrorCode::RequestUnwritable,
            format!("{error:#}"),
        ),
    }
}

/// `request_conflict`: the id already names a request with other arguments. The output carries
/// that request's receipt, when there is one, so the agent sees what its id already created.
fn conflict(request_id: &str, existing: Option<SpawnReceipt>) -> CliOutput {
    CliOutput {
        receipt: existing,
        ..CliOutput::failed(
            request_id,
            ErrorCode::RequestConflict,
            format!(
                "request {request_id} was already made with different arguments; rerun with the \
                 original arguments to check on it, and use a new --request-id only for a \
                 different task"
            ),
        )
    }
}

/// Whether the request file for `id` is still in `dir` (as any kind of entry).
fn request_file_present(dir: &Path, id: &str) -> bool {
    std::fs::symlink_metadata(spawn::request_path(dir, id)).is_ok()
}

/// The calling session's id and state directory, from the env its terminal was launched with.
pub(crate) fn session_env(
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(String, PathBuf), String> {
    let session = env(ENV_SESSION).filter(|value| !value.is_empty());
    let state_dir = env(ENV_STATE_DIR).filter(|value| !value.is_empty());
    let (Some(session), Some(state_dir)) = (session, state_dir.map(PathBuf::from)) else {
        return Err(format!(
            "{ENV_SESSION} and {ENV_STATE_DIR} are not both set, so this is not a pmtui-managed \
             session; a session started before spawn support gets them at its next launch \
             (Enter after pause, or r)"
        ));
    };
    if !state_dir.is_absolute() || !state_dir.is_dir() {
        return Err(format!(
            "{ENV_STATE_DIR}={} is not an existing absolute directory",
            state_dir.display()
        ));
    }
    Ok((session, state_dir))
}

/// The normalized arguments the request records, or why they cannot be sent. The directory is
/// made absolute against the current directory but not canonicalized: the dashboard resolves
/// and checks it.
fn checked_args(args: &SpawnArgs) -> Result<SpawnArgs, String> {
    let mut args = args.normalized();
    if let Some(title) = &args.title {
        let chars = title.chars().count();
        if chars > spawn::TITLE_MAX_CHARS {
            return Err(format!(
                "--title must be {} characters or fewer, not {chars}",
                spawn::TITLE_MAX_CHARS
            ));
        }
        if title.chars().any(char::is_control) {
            return Err("--title cannot contain control characters".into());
        }
    }
    if let Some(name) = &args.name {
        normalize_display_name(name).map_err(|reason| format!("--name: {reason}"))?;
    }
    if let Some(dir) = &args.dir {
        let absolute = std::path::absolute(dir)
            .map_err(|error| format!("--dir {:?}: {error}", dir.display()))?;
        args.dir = Some(absolute);
    }
    Ok(args)
}

/// An already-published request is this one when it has the same parent, id and arguments.
fn same_request(existing: &SpawnRequest, request: &SpawnRequest) -> bool {
    existing.parent_session == request.parent_session
        && existing.request_id == request.request_id
        && existing.args.args_hash() == request.args.args_hash()
}

/// Poll the receipt every [`POLL_MS`] until it is final or `wait_s` has passed. A receipt that
/// cannot be read counts as none yet: only the dashboard writes it, and it replaces it whole.
fn wait_for_receipt(dir: &Path, id: &str, wait_s: u64, waiter: &mut dyn Waiter) -> CliOutput {
    let deadline = waiter.now_ms() + u128::from(wait_s) * 1000;
    loop {
        let receipt = spawn::read_receipt(dir, id).ok().flatten();
        let now = waiter.now_ms();
        let finished = receipt.as_ref().is_some_and(|found| answered(found.state));
        if finished || now >= deadline {
            return waited(id, receipt, finished);
        }
        let left = u64::try_from(deadline - now).unwrap_or(POLL_MS);
        waiter.sleep_ms(left.min(POLL_MS));
    }
}

/// Whether this state ANSWERS what the caller asked for — the point `--wait` stops at.
///
/// A child is a job, so `ready` is NOT an answer: it means the run STARTED. Stopping there is what sent
/// a parent to read its child's transcript for something the receipt was about to say in one line.
/// `outcome_unknown` is not an answer either — for a job it says readiness could not see the terminal
/// twice, and the retirement sweep replaces it moments later with what the run actually left.
///
/// Everything else that is final IS an answer: the five job outcomes, a launch that failed, and
/// `needs_attention`, which is the dashboard asking for a person rather than reporting on the task.
fn answered(state: ReceiptState) -> bool {
    match state {
        ReceiptState::Ready
        | ReceiptState::OutcomeUnknown
        | ReceiptState::Claimed
        | ReceiptState::Staged
        | ReceiptState::Launching => false,
        ReceiptState::NeedsAttention => true,
        other => other.is_job_terminal(),
    }
}

fn waited(id: &str, receipt: Option<SpawnReceipt>, finished: bool) -> CliOutput {
    let state = match &receipt {
        Some(found) if finished => CliState::Receipt(found.state),
        Some(_) => CliState::InProgress,
        None => CliState::Queued,
    };
    CliOutput {
        state,
        request_id: Some(id.to_string()),
        error: receipt.as_ref().and_then(|found| found.error.clone()),
        receipt,
    }
}

/// `0` ready or done; `3` queued, in progress, needs attention or needs a human (the receipt says
/// what to do); `1` failed, cancelled, ended without a result, or outcome unknown; `2` a usage error.
///
/// A job's `done` joins `ready` at 0 because both mean "the thing you asked for happened"; a parent
/// that branched on `$? == 0` before this change still reads correctly. `needs_human` joins the `3`
/// family for the same reason `needs_attention` is there: the receipt names the next action.
pub(crate) fn exit_code(out: &CliOutput) -> i32 {
    match out.state {
        CliState::Receipt(ReceiptState::Ready | ReceiptState::Done) => 0,
        CliState::Receipt(
            ReceiptState::NeedsAttention
            | ReceiptState::NeedsHuman
            | ReceiptState::Claimed
            | ReceiptState::Staged
            | ReceiptState::Launching,
        )
        | CliState::Queued
        | CliState::InProgress
        | CliState::CancelRequested => 3,
        CliState::Receipt(
            ReceiptState::Failed
            | ReceiptState::OutcomeUnknown
            | ReceiptState::EndedWithoutResult
            | ReceiptState::Cancelled,
        )
        | CliState::Failed => 1,
        CliState::UsageError => 2,
    }
}

/// The printed name of a state. A receipt still claimed, staged or launching is `in_progress`.
fn state_name(state: CliState) -> &'static str {
    match state {
        CliState::Receipt(ReceiptState::Ready) => "ready",
        CliState::Receipt(ReceiptState::NeedsAttention) => "needs_attention",
        CliState::Receipt(ReceiptState::Failed) | CliState::Failed => "failed",
        CliState::Receipt(ReceiptState::OutcomeUnknown) => "outcome_unknown",
        CliState::Receipt(ReceiptState::Done) => "done",
        CliState::Receipt(ReceiptState::NeedsHuman) => "needs_human",
        CliState::Receipt(ReceiptState::EndedWithoutResult) => "ended_without_result",
        CliState::Receipt(ReceiptState::Cancelled) => "cancelled",
        CliState::Receipt(
            ReceiptState::Claimed | ReceiptState::Staged | ReceiptState::Launching,
        )
        | CliState::InProgress => "in_progress",
        CliState::CancelRequested => "cancel_requested",
        CliState::Queued => "queued",
        CliState::UsageError => "usage_error",
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    schema_version: u32,
    request_id: Option<&'a str>,
    state: &'static str,
    receipt: Option<&'a SpawnReceipt>,
    error: Option<&'a SpawnError>,
}

/// One JSON document: `{"schema_version", "request_id", "state", "receipt", "error"}`. The
/// receipt is the dashboard's, verbatim; `error` repeats its error, or carries the command's.
pub(crate) fn render_json(out: &CliOutput) -> String {
    let envelope = Envelope {
        // The ENVELOPE's own version, unchanged: this is the CLI's output contract, and the receipt
        // nested inside it carries its own (`RECEIPT_SCHEMA_VERSION`).
        schema_version: spawn::SCHEMA_VERSION,
        request_id: out.request_id.as_deref(),
        state: state_name(out.state),
        receipt: out.receipt.as_ref(),
        error: out.error.as_ref(),
    };
    // Every field is a string, a number, or a receipt parsed from JSON, so this cannot fail.
    serde_json::to_string_pretty(&envelope).unwrap_or_default()
}

/// One line, such as `spawned service-2 "Fix flaky fork test" (codex, from service): ready`,
/// or `spawn <request_id>: queued — …` before the dashboard has named the child.
pub(crate) fn render_plain(out: &CliOutput) -> String {
    let state = state_name(out.state);
    let session = out
        .receipt
        .as_ref()
        .and_then(|found| found.session.as_ref());
    let mut line = match (session, out.request_id.as_deref()) {
        (Some(child), _) => {
            let title = child
                .title
                .as_deref()
                .map(|title| format!("\"{title}\" "))
                .unwrap_or_default();
            format!(
                "spawned {} {title}({}, from {}): {state}",
                child.id,
                child.agent.label(),
                child.spawned_by
            )
        }
        (None, Some(id)) => format!("spawn {id}: {state}"),
        (None, None) => format!("spawn: {state}"),
    };
    let check_again = || {
        let id = out.request_id.as_deref().unwrap_or("<id>");
        format!("rerun with the same arguments and --request-id {id} to check again")
    };
    let detail = match (&out.error, out.state) {
        (Some(error), _) => Some(format!("{}: {}", code_name(error.code), error.message)),
        (None, CliState::Queued) => Some(format!(
            "no dashboard has claimed it yet; {}",
            check_again()
        )),
        (None, CliState::InProgress) => Some(format!(
            "the dashboard is still creating the child; {}",
            check_again()
        )),
        _ => None,
    };
    if let Some(detail) = detail {
        line.push_str(" — ");
        line.push_str(&detail);
    }
    if let Some(next) = out
        .receipt
        .as_ref()
        .and_then(|found| found.next_action.as_ref())
    {
        line.push_str("; next: ");
        line.push_str(&tmux::launch_command(&next.argv));
    }
    line
}

/// An error code as the JSON spells it.
fn code_name(code: ErrorCode) -> String {
    serde_json::to_string(&code)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}
