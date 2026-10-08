//! Unit tests for the agent-loop scheduler, split to mirror the engine's modules: a
//! test lives in the file named after the module whose behaviour it pins. The fixtures
//! every one of those files builds on live here — a session on disk, a `FakeDriver`, a
//! `FakeClock` and the marker/consult writers — because sharing them is what keeps the
//! tests short enough to read as statements about behaviour.

use super::*;
use crate::clock::test_support::FakeClock;
use crate::pmstate::StopKind;
use crate::policy::{self, Decision};
use crate::state::Answer;
use crate::state::Tier;
use crate::tmux::PaneActivity;
use crate::tmux::fake::FakeDriver;
use crate::worker;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

// The engine's own modules, globbed so each test file's `use super::*;` reaches the
// items it exercises exactly as the single test module used to.
use super::drive::*;
use super::fallback::*;
// `nudge`'s items the tests use (`loop_nudge_prompt`, the CADENCE_* consts, `human_cadence`)
// now all arrive via `use super::*;` — the parent module re-exports them (Layer 3 needed
// `loop_nudge_prompt` reachable), so a `use super::nudge::*;` glob here would be redundant.
use super::progress::*;
use super::session::*;
use super::stops::*;
use super::supervisor::*;

mod awaiting_report_exit;
mod budget;
mod dialog_advice;
mod drive;
mod fallback;
mod lifecycle;
mod marker;
mod nudge;
mod progress;
mod scenarios;
mod session;
mod stops;
mod supervisor;

struct Fx {
    _dir: TempDir,
    paths: ProjectPaths,
    driver: FakeDriver,
    clock: FakeClock,
    sched: JobScheduler,
    /// Isolated claude config dir the seed-existence probe reads (never the real
    /// `~/.claude`). No `projects` subdir by default ⇒ the probe returns `None` ⇒ tests
    /// exercise the optimistic-resume state machine; a test that wants the probe to answer
    /// creates `claude_home/projects/<slug>/<id>.jsonl` (present) or an empty `projects`
    /// dir (confident-absent).
    claude_home: PathBuf,
}

const SESSION_ID: &str = "bot.one";
const START: Epoch = 1000;
/// A pane snapshot that classifies Idle (bare prompt) / Busy (interrupt hint).
const IDLE_PANE: &str = "did some work\n❯ ";
const BUSY_PANE: &str = "✻ Working… (esc to interrupt)";

/// A session dir with a valid per-session config + a fresh Idle ledger.
fn setup(tier: Tier, engine: Engine, cadence_s: Option<u64>) -> Fx {
    setup_with(tier, engine, cadence_s, |_| {})
}

/// As [`setup`], but lets the test mutate the fresh ledger before it is written.
fn setup_with(
    tier: Tier,
    engine: Engine,
    cadence_s: Option<u64>,
    edit: impl FnOnce(&mut AgentLoopState),
) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::for_session(dir.path(), SESSION_ID);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: tier,
            step_timeout_s: 100,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    let mut ledger = AgentLoopState::fresh(engine, cadence_s, START);
    edit(&mut ledger);
    job::save(&paths, &ledger).unwrap();
    let root = dir.path().to_path_buf();
    let mut sched = JobScheduler::new(SESSION_ID, &root, SESSION_ID, engine, None);
    // FakeDriver owns consult spawning, so these tests must not depend on whether the host
    // happens to have the selected decider CLI installed. The missing-binary test overrides
    // this cache to false explicitly.
    sched.set_decider_binary_available(true);
    // The no-progress backstop (slice 1) is OPT-IN per test, like `max_wakes` overrides: the
    // fixture work_dir is a plain tempdir (not a git repo), so `tree_fingerprint` would return
    // None and the detector is inert anyway — but forcing the threshold to 0 here also keeps
    // unrelated scenarios from spawning `git` on every nudge. The `progress` tests set it back on.
    sched.no_progress_threshold = 0;
    // Isolate the seed-existence probe from the machine's real ~/.claude: point it at an
    // (un-created) dir under the temp root, so by default the probe finds no `projects`
    // dir and returns None (the optimistic-resume state machine every prior test asserts).
    let claude_home = root.join("claude-home");
    sched.set_claude_home(&claude_home);
    Fx {
        _dir: dir,
        paths,
        driver: FakeDriver::new(),
        clock: FakeClock::new(START),
        sched,
        claude_home,
    }
}

fn ledger(fx: &Fx) -> AgentLoopState {
    job::load(&fx.paths).unwrap().unwrap()
}

/// Drive ONE confirmed heartbeat nudge on an idle pane, from a DUE tick at the
/// current clock. The two-observation confirmation gate
/// ([`IDLE_CONFIRMATIONS_REQUIRED`]) means the first due tick only ARMS the gate (a
/// short `BUSY_RECHECK_S` re-park, asserted here), and the tick `BUSY_RECHECK_S`
/// later actually nudges. Leaves the clock at the nudging instant and returns that
/// tick's outcome.
fn tick_confirmed(fx: &mut Fx) -> JobTick {
    let armed_at = fx.clock.now();
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: armed_at + BUSY_RECHECK_S
        },
        "the first Idle observation only arms the confirmation gate"
    );
    fx.clock.set(armed_at + BUSY_RECHECK_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap()
}

fn loop_session(fx: &Fx) -> String {
    tmux::session_name(&fx.sched.project_id, &fx.sched.work_dir)
}

fn chat_session(fx: &Fx) -> String {
    tmux::session_name(&fx.sched.project_id, &fx.sched.work_dir)
}

/// The argv the persistent session was launched with (last launch of `session`).
fn launched_argv(fx: &Fx, session: &str) -> Option<Vec<String>> {
    fx.driver
        .launched()
        .into_iter()
        .rev()
        .find(|(s, _)| s == session)
        .map(|(_, argv)| argv)
}

/// The value following `flag` in the loop's launch argv (e.g. `--session-id` for a
/// CREATE, `--resume` for a RESUME), from the last launch of `session`.
fn launched_flag(fx: &Fx, session: &str, flag: &str) -> Option<String> {
    let argv = launched_argv(fx, session)?;
    argv.windows(2).find_map(|w| {
        if w[0] == flag {
            Some(w[1].clone())
        } else {
            None
        }
    })
}

// --- marker-disposer test helpers (Slice 2) ------------------------------

/// An AlreadyUp claude session on a DUE `Monitoring{START}` run, idle pane — the
/// common start state for a marker-disposer test (one `tick` reaches step 2.5). The
/// `edit` closure mutates the fresh ledger (e.g. to seed `last_marker_seq`) before
/// it is persisted.
fn marker_fx(tier: Tier, edit: impl FnOnce(&mut AgentLoopState)) -> (Fx, String) {
    let fx = setup_with(tier, Engine::Claude, Some(300), |l| {
        l.conversation_id = Some("convo-1".into());
        l.run = JobRun::Monitoring { until: START };
        edit(l);
    });
    let sess = tmux::session_name(&fx.sched.project_id, &fx.sched.work_dir);
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    (fx, sess)
}

/// Overwrite the session's `needs-you.json` with `body` (the agent is its sole
/// writer; the harness only reads it). Fresh real mtime.
fn write_marker(fx: &Fx, body: &str) {
    std::fs::write(fx.paths.needs_you(), body).unwrap();
}

/// The agent REPORTING PROGRESS — what m38 requires between two nudges.
///
/// Since m38 the harness will not type into a pane again until the agent has written a marker
/// with a higher `seq` than the one outstanding (`idle_observed`'s awaiting-report gate), so a
/// test that wants a SECOND nudge has to let the agent answer the first. `working` is the state
/// a mid-goal agent reports and the one that must not escalate; `dispose_report` advances
/// `last_marker_seq` for every state, which is what opens the gate.
///
/// Backdated by `age_s` so the mid-write grace treats it as settled; callers pass a DECREASING
/// age so successive markers have strictly increasing mtimes (the mtime pre-check short-circuits
/// on an unchanged one).
fn report_progress(fx: &Fx, seq: u64, age_s: u64) {
    write_marker(
        fx,
        &format!(r#"{{"seq":{seq},"state":"working","status":"still going"}}"#),
    );
    backdate_marker(fx, age_s);
}

/// Age the marker's mtime `secs_ago` seconds into the REAL-wall-clock past, so the
/// OQ3 mid-write grace treats it as "settled" (not a mid-write read). Distinct
/// `secs_ago` values also make successive rewrites' mtimes distinct (so the mtime
/// pre-check sees each as advanced).
fn backdate_marker(fx: &Fx, secs_ago: u64) {
    let t = SystemTime::now() - Duration::from_secs(secs_ago);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(fx.paths.needs_you())
        .unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(t))
        .unwrap();
}

fn blocked_fx() -> Fx {
    setup_with(Tier::Standard, Engine::Claude, Some(300), |l| {
        l.conversation_id = Some("convo-1".into());
        l.run = JobRun::Blocked {
            stop_ids: vec!["stop-x".into()],
            since: START,
        };
        l.open_stops = vec![open_stop(
            "stop-x".into(),
            StopKind::Ambiguity,
            None,
            "",
            &[],
            START,
        )];
    })
}

const BLOCKED_HARD: &str = r#"{"seq":100,"state":"blocked","stops":[{"kind":"publish","effect":{"scope":"external","reversibility":"irreversible","authority":"ordinary"},"risk_class":"low","question":"ship it?"}]}"#;

/// A consultable marker decision: Autopilot + a low-risk ambiguity that actually asks
/// something and enumerates options.
const AUTOFLOW_ASKS: &str = r#"{"seq":9,"state":"blocked","stops":[{"kind":"ambiguity","effect":{"scope":"local","reversibility":"reversible","authority":"ordinary"},"risk_class":"low","question":"Which formatter for the changelog?","options":["prettier","dprint"]}]}"#;

const CHOICE_DIALOG_PANE: &str = concat!(
    " Which formatter should I use for the changelog?\n",
    " ❯ 1. prettier\n",
    "   2. dprint\n",
    "   3. Type something.\n",
    "\n",
    " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
);

const CHANGED_CHOICE_DIALOG_PANE: &str = concat!(
    " Which test command should I run?\n",
    " ❯ 1. cargo test\n",
    "   2. cargo test --all\n",
    "\n",
    " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
);

const MULTI_CHOICE_DIALOG_PANE: &str = concat!(
    "←  ☐ Test layers  ✔ Submit  →\n",
    " Which test layers should I run?\n",
    " ❯ 1. [ ] unit\n",
    "   2. [ ] integration\n",
    "   3. [ ] real tmux\n",
    "   4. [ ] Type something\n",
    "      Submit\n",
    "────────────────────────────────────────────────────────────\n",
    "  5. Chat about this\n",
    " Enter to select · ↑/↓ to navigate · Esc to cancel\n",
);

/// The `pmsup-` session name for consult `seq` of the fixture's session.
fn sup_session(fx: &Fx, seq: u64) -> String {
    tmux::supervisor_session_name(&fx.sched.project_id, &fx.sched.work_dir, seq)
}

fn advice_wrapper(fx: &Fx, seq: u64) -> PathBuf {
    fx.paths
        .steps_dir()
        .join(format!("{}.run.sh", sup_session(fx, seq)))
}

/// Put a goal on disk — the thing the supervisor is asked to decide AGAINST.
fn write_goal(fx: &Fx, goal: &str) {
    state::write_text_atomic(&fx.paths.brief(), goal).unwrap();
}

/// Flip the on-disk per-session `decider_engine` (the field `tick` re-reads each sweep to
/// dispatch the consult), leaving every other config field as `setup_with` wrote it. Used
/// by the codex-consult test to select the codex builder without touching the claude twin.
fn set_decider_engine(fx: &Fx, engine: Engine) {
    let mut config: Config = state::read_json(&fx.paths.config()).unwrap();
    config.decider_engine = engine;
    state::write_json_atomic(&fx.paths.config(), &config).unwrap();
}

/// Put a per-session `decider_model` on disk (Task 3): `spawn_advice` re-reads config each
/// sweep and pins WHICH model the selected engine's consult argv carries, taking precedence
/// over the env/const fallback. Leaves every other config field as `setup_with` wrote it.
fn set_decider_model(fx: &Fx, model: Option<&str>) {
    let mut config: Config = state::read_json(&fx.paths.config()).unwrap();
    config.decider_model = model.map(str::to_string);
    state::write_json_atomic(&fx.paths.config(), &config).unwrap();
}

/// The argv the harness actually spawned for consult `seq`.
fn consult_argv(fx: &Fx, seq: u64) -> Vec<String> {
    fx.driver
        .command_for(&sup_session(fx, seq))
        .unwrap_or_else(|| panic!("no consult {seq} was spawned"))
}

/// The nonce the harness minted for consult `seq`, recovered from the prompt it built.
/// The test has to echo THIS value, exactly as a real supervisor would — which is what
/// makes the wrong-nonce tests below meaningful rather than tautological.
fn consult_nonce(fx: &Fx, seq: u64) -> String {
    let argv = consult_argv(fx, seq);
    argv.last()
        .expect("the prompt is the trailing positional")
        .lines()
        .find_map(|l| l.strip_prefix("NONCE: "))
        .expect("the consult prompt carries a nonce")
        .trim()
        .to_string()
}

/// Finish consult `seq` the way the tee'd wrapper does: the reply text in the LOG, the
/// exit code in the done-signal.
fn finish_consult(fx: &Fx, seq: u64, reply: &str, exit_code: i32) {
    std::fs::create_dir_all(fx.paths.steps_dir()).unwrap();
    std::fs::write(advice_wrapper(fx, seq), "generated wrapper").unwrap();
    std::fs::write(fx.paths.advice_log(seq), reply).unwrap();
    std::fs::write(fx.paths.advice_done_signal(seq), format!("{exit_code}\n")).unwrap();
}

fn seed_inflight_advice_artifacts(fx: &Fx, seq: u64) {
    std::fs::create_dir_all(fx.paths.steps_dir()).unwrap();
    std::fs::write(advice_wrapper(fx, seq), "generated wrapper").unwrap();
    std::fs::write(fx.paths.advice_log(seq), "partial reply").unwrap();
    std::fs::write(fx.paths.advice_last_message(seq), "partial verdict").unwrap();
}

/// A `claude --output-format json` envelope holding `inner` as its `result` string.
fn consult_reply(inner: &str) -> String {
    format!(
        "{{\"type\":\"result\",\"subtype\":\"success\",\"result\":{}}}",
        serde_json::to_string(inner).unwrap()
    )
}

/// Spawn a consult for `AUTOFLOW_ASKS` and assert the spawning tick consumed nothing.
/// Returns the consult seq (always 1 — the first consult of a fresh scheduler).
fn start_consult(fx: &mut Fx) -> u64 {
    write_goal(
        fx,
        "Keep the changelog tooling consistent; dprint is already vendored.",
    );
    write_marker(fx, AUTOFLOW_ASKS);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        },
        "the spawning tick parks a SHORT recheck and yields"
    );
    assert!(
        fx.driver.sent_keys().is_empty(),
        "nothing may be typed while the harness has nothing to say"
    );
    let l = ledger(fx);
    assert!(
        l.pending_context.is_none(),
        "no approval is written until the supervisor has answered: {:?}",
        l.pending_context
    );
    assert_eq!(l.last_marker_seq, 9, "the bump is consumed exactly once");
    // The bump is consumed, so the harness now OWES the worker an answer — and that debt
    // has to survive a daemon restart. Asserted in the shared helper so every consult
    // test enforces it (m23 bug 1).
    assert!(
        l.advice_inflight.is_some(),
        "the spawning tick must record the consult debt on the ledger"
    );
    1
}

/// Detect [`CHOICE_DIALOG_PANE`], spawn its consult, and assert that no key was
/// selected before a validated verdict exists.
fn start_dialog_consult(fx: &mut Fx, session: &str) -> u64 {
    write_goal(
        fx,
        "Keep the changelog tooling consistent; dprint is already vendored.",
    );
    fx.driver.set_tail(session, CHOICE_DIALOG_PANE);
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + SUPERVISOR_POLL_S
        }
    );
    assert!(
        fx.driver.selected_dialog_options().is_empty(),
        "detecting a choice must not select before the decider answers"
    );
    assert!(
        ledger(fx)
            .advice_inflight
            .as_ref()
            .is_some_and(|a| a.pane_dialog),
        "dialog consults need a distinct durable target"
    );
    let prompt = consult_argv(fx, 1).last().cloned().unwrap_or_default();
    assert!(prompt.contains("[0] prettier") && prompt.contains("[1] dprint"));
    assert!(
        !prompt.contains("Type something"),
        "a meta option that opens another input is not a decidable answer: {prompt}"
    );
    1
}

/// Historical blanket-approval text. Negative assertions prove it never reaches the worker.
const BLANKET: &str = "Auto-approved";

// --- scenario-runner shared helpers (context-sync eval, Layer 1 / Task 1) ------
//
// The scenario runner (`mod scenarios`) needs three primitives no existing fixture
// provides: a `Vec<Answer>` writer for `answers.json` (the answer-arrives / resume
// scenarios), a `turn_signal` byte writer (the marker-less-finish FO-2 scenarios), and
// readers for the two decider side-files (`decisions.md` / `raw.jsonl`). They live here,
// beside the other fixtures, so every test file reaches them via `use super::*;`.

/// Append a human [`Answer`] to `answers.json`, reading the current inbox first so
/// successive calls accumulate. `answered_at` is stamped to `now` (a caller passes the
/// current clock, which is `>= since` for a live park, so the answer counts). Field names
/// verified against `state::Answer` (`records.rs:243`): `stop_id`, `answer`, `note`,
/// `answered_by` (defaults "user"), `answered_at`.
fn push_answer(fx: &Fx, stop_id: &str, note: Option<&str>, now: Epoch) {
    let mut answers: Vec<Answer> = state::read_json_or(&fx.paths.answers(), Vec::new()).unwrap();
    answers.push(Answer {
        stop_id: stop_id.to_string(),
        answer: "answered".into(),
        note: note.map(Into::into),
        answered_by: "user".into(),
        answered_at: now,
    });
    state::write_json_atomic(&fx.paths.answers(), &answers).unwrap();
}

/// Grow the per-session turn-complete signal to exactly `bytes` bytes, so
/// `JobScheduler::turn_count()` (`mod.rs:521`) reports `bytes` completed turns. `write`
/// truncates, so a later call SETS the size rather than only ever appending.
fn grow_turn_signal(fx: &Fx, bytes: usize) {
    std::fs::create_dir_all(fx.paths.daemon_dir()).unwrap();
    std::fs::write(fx.paths.turn_signal(), vec![b'x'; bytes]).unwrap();
}

/// Read `decisions.md` (`paths.rs:76`), or `None` when it does not exist yet — the sparse
/// notable-only human log (Escalated/Stalled), absent for a Working/Monitoring/AutoFlow
/// disposition.
fn read_decisions_md(fx: &Fx) -> Option<String> {
    std::fs::read_to_string(fx.paths.decisions()).ok()
}

/// The non-empty lines of `raw.jsonl` (`paths.rs:82`), or `[]` when the file is absent —
/// the full-fidelity machine JSONL, one line per accepted marker bump.
fn read_raw_jsonl(fx: &Fx) -> Vec<String> {
    std::fs::read_to_string(fx.paths.raw_jsonl())
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn fixture_helpers_round_trip_answers_turn_signal_and_side_files() {
    let (fx, _sess) = marker_fx(Tier::Standard, |_| {});
    // A fresh session has neither side-file yet.
    assert!(
        read_raw_jsonl(&fx).is_empty(),
        "no raw.jsonl before any dispose"
    );
    assert_eq!(
        read_decisions_md(&fx),
        None,
        "no decisions.md before any dispose"
    );

    // push_answer round-trips through answers.json (verifying the real field names).
    push_answer(&fx, "stop-x", Some("go ahead"), START + 5);
    push_answer(&fx, "stop-y", None, START + 9);
    let answers: Vec<Answer> = state::read_json_or(&fx.paths.answers(), Vec::new()).unwrap();
    assert_eq!(answers.len(), 2, "answers accumulate");
    assert_eq!(answers[0].stop_id, "stop-x");
    assert_eq!(answers[0].answer, "answered");
    assert_eq!(answers[0].note.as_deref(), Some("go ahead"));
    assert_eq!(answers[0].answered_by, "user");
    assert_eq!(answers[0].answered_at, START + 5);
    assert_eq!(answers[1].note, None);

    // grow_turn_signal sets the byte length turn_count() reads (and re-sets it, not appends).
    grow_turn_signal(&fx, 3);
    assert_eq!(std::fs::metadata(fx.paths.turn_signal()).unwrap().len(), 3);
    grow_turn_signal(&fx, 5);
    assert_eq!(
        std::fs::metadata(fx.paths.turn_signal()).unwrap().len(),
        5,
        "grow SETS the size (write truncates), so it models a monotonic turn count"
    );
}
