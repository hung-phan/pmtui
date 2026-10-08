//! The agent-loop `JobScheduler` driven IN PROCESS at a real pane. Both tests here
//! are bug classes made of a tmux fact rather than of harness logic: a pane whose
//! process has EXITED but whose last frame still classifies Idle, and a supervisor
//! verdict coming back through the real detached-spawn substrate with its control
//! bytes intact.

use std::process::Command;
use std::time::{Duration, Instant};

use agent_manager::clock::{Clock, SystemClock};
use agent_manager::job::WakeState;
use agent_manager::pmstate;
use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, TmuxDriver, session_name};

use crate::keystrokes::send_key;
use crate::probe::TmuxSocket;
use crate::probe::{tmux_available, wait_for_file_contents, wait_for_pane_text, wait_until};
use crate::seed::{CONSULTABLE_MARKER, seed_supervisor_session};
use crate::stubs::{
    REPLY_PICKS_DPRINT, REPLY_WITH_ESCAPE, install_claude_stub, next_step_reporting_claude_stub,
    nudge_counting_claude_stub, write_resume_fallback_stubs, write_stubs,
};

/// ACCEPTANCE (real tmux): the harness must NOT nudge a CORPSE.
///
/// This is the bug class a `FakeDriver` unit test structurally cannot catch. The whole
/// failure lives in a tmux fact: with `remain-on-exit on`, a pane whose process has EXITED
/// keeps both the session (`has-session` succeeds) and the last frame it painted — and if
/// that frame ends at a bare prompt, `classify_pane` reports **Idle**. A fake can be told
/// "this pane is dead"; only real tmux can establish that a dead pane still LOOKS idle,
/// which is the entire reason the harness needed a separate liveness probe. Before the fix
/// the scheduler nudged this pane every cadence, forever, at full token cost, and the human
/// heard nothing.
///
/// It also pins the tmux trap the probe is built around: `display-message -p` exits **0
/// printing NOTHING** for a target it cannot resolve, so `pane_dead` must assert on OUTPUT
/// and never on exit status. A bogus target is probed here to prove it comes back "not
/// dead" rather than `Err` or a false positive.
///
/// Hygiene, mirroring `send_keys_against_real_tmux_reaches_the_pane`: a private per-pid
/// socket, every result recorded into a local, and `kill-server` BEFORE the first assertion
/// — so a failing assertion can never leak a tmux server or session. No `pmd`/`pmtui` is
/// started (the scheduler is driven in-process), so there is no daemon to kill.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// harness_does_not_nudge_a_dead_pane`.
#[test]
#[ignore]
fn harness_does_not_nudge_a_dead_pane() {
    if !tmux_available() {
        eprintln!("skipping dead-pane test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-dead");
    let driver = TmuxDriver::with_socket(socket.name());
    let id = "deadpane";
    let root = dir.path().to_path_buf();
    // The scheduler derives the session name itself, so the corpse must be created under
    // exactly that name for `ensure_session` to find it AlreadyUp (and not relaunch).
    let session = session_name(id, &root);

    // A per-session config + a fresh Idle ledger: the same on-disk shape pmtui's intake
    // writes, which is all `tick` needs to reach the drive path.
    let paths = ProjectPaths::for_session(&root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    state::write_text_atomic(&paths.brief(), "prove a corpse is never nudged\n").unwrap();
    agent_manager::job::save(
        &paths,
        &agent_manager::job::AgentLoopState::fresh(Engine::Claude, Some(1), SystemClock.now()),
    )
    .unwrap();

    // `printf` paints a bare prompt (what `classify_pane` calls Idle), then `cat` holds the
    // pane open so `remain-on-exit` can be set BEFORE the process exits — no sleep race.
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &[
            "sh".to_string(),
            "-c".to_string(),
            "printf '> \\n'; cat".to_string(),
        ],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    // Needle is bare `>`: tmux strips trailing spaces from a captured row, so the painted
    // `"> "` comes back as `">"` — which is exactly what `is_bare_prompt` matches on.
    let prompt_drawn = launched.is_ok() && wait_for_pane_text(&driver, &session, ">");
    // remain-on-exit: keep the pane (as DEAD) when its process exits, instead of tearing
    // the session down. This is the real-world shape — a tmux configured this way, or an
    // engine that exits while the session is kept — and the one the harness misread.
    let set_remain = prompt_drawn
        && Command::new("tmux")
            .args([
                "-L",
                socket.name(),
                "set-option",
                "-t",
                &format!("={session}:"),
                "remain-on-exit",
                "on",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    // EOF to `cat` ⇒ the shell exits ⇒ the pane becomes a corpse.
    let killed = set_remain && send_key(socket.name(), &session, "C-d");
    let became_dead = killed
        && wait_until(Duration::from_secs(10), || {
            driver.pane_dead(&session).unwrap_or(false)
        });

    // ---- the three facts that make the bug, recorded before any assertion ----
    // (1) tmux still says the SESSION is alive, so `ensure_session` will not relaunch it.
    let still_alive = driver.is_alive(&session);
    // (2) the corpse's last frame classifies IDLE — i.e. a capture alone authorises a nudge.
    let corpse_tail = driver.capture_tail(&session, 40).unwrap_or_default();
    let corpse_reads_idle =
        agent_manager::tmux::classify_pane(&corpse_tail) == agent_manager::tmux::PaneActivity::Idle;
    // (3) the probe on a BOGUS target must be "not dead", not an error and not `true`
    //     (`display-message -p` exits 0 printing nothing — assert on OUTPUT, never status).
    let bogus = driver.pane_dead(&format!("no-such-session-{}", std::process::id()));

    // ---- drive the REAL scheduler at the corpse ----
    let mut sched = agent_manager::job_engine::JobScheduler::new(
        id,
        root.clone(),
        id,
        agent_manager::registry::Engine::Claude,
        None,
    );
    let clock = SystemClock;
    let first = sched.tick(&driver, &clock);
    // Keep sweeping for longer than BUSY_RECHECK_S (5s) + the 1s cadence, so the run really
    // does reach the point where a pre-fix harness performs its `send_keys` — a shorter
    // window would leave the no-nudge assertion below asserting about a send that was never
    // even attempted.
    for _ in 0..12 {
        std::thread::sleep(Duration::from_millis(600));
        let _ = sched.tick(&driver, &clock);
    }
    let after = driver.capture_tail(&session, 200).unwrap_or_default();
    let ledger = agent_manager::job::load(&paths).ok().flatten();

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    // ---- assertions (the server is already gone) ----
    launched.expect("launch_interactive should start the scratch session");
    assert!(prompt_drawn, "the stub must paint a bare prompt first");
    assert!(
        set_remain,
        "could not set remain-on-exit — test cannot proceed"
    );
    assert!(
        became_dead,
        "the pane should be DEAD after its process exited; capture was:\n{corpse_tail}"
    );
    assert!(
        still_alive.expect("has-session should query cleanly"),
        "PREMISE: tmux keeps the session for a dead pane — if this ever stops being true, \
         `ensure_session` would relaunch and this whole bug class disappears"
    );
    assert!(
        corpse_reads_idle,
        "PREMISE: a corpse's last frame must classify Idle, or this test proves nothing \
         about the probe. Capture was:\n{corpse_tail}"
    );
    assert_eq!(
        bogus.ok(),
        Some(false),
        "pane_dead on an unresolvable target must be a clean `false` — tmux exits 0 \
         printing NOTHING there, so keying off exit status would silently invert this"
    );
    // Belt and braces, and deliberately NOT sold as the load-bearing check: MEASURED
    // against the pre-fix code, this assertion still passes there, because tmux will not
    // render into a dead pane whatever we send it. So it can only ever catch a nudge that
    // somehow DID land — useful, but it cannot see the bug on its own.
    assert!(
        !after.contains("You are a long-running agent"),
        "the harness typed a nudge into a DEAD pane; pane was:\n{after}"
    );
    // THE BUG, in one assertion (this is the one that fails on pre-fix code, with
    // `Monitoring{…}`): the harness must SURFACE the dead pane at once, instead of
    // re-parking a cadence it will keep driving until the 30-minute stall backstop fires a
    // misleading "busy with no progress".
    let first = first.expect("tick should not error on a dead pane");
    assert!(
        matches!(first, agent_manager::job_engine::JobTick::Stuck(_)),
        "a dead pane must surface at once, got {first:?}"
    );
    let ledger = ledger.expect("the ledger should still be readable");
    assert!(
        matches!(ledger.run, agent_manager::job::JobRun::Blocked { .. }),
        "and it must park on the human, got {:?}",
        ledger.run
    );
    assert_eq!(
        ledger.open_stops.len(),
        1,
        "exactly one stop, surfaced once"
    );
    assert_eq!(
        ledger.open_stops[0].kind,
        pmstate::StopKind::Capability,
        "only a human can close or restart the session"
    );
}

/// ACCEPTANCE (real tmux): the Standard→Autopilot-without-chatting resume regression.
///
/// This is the bug class a `FakeDriver` unit test cannot fully establish, because the whole
/// mechanism is a tmux fact: a human who flips a freshly created Standard session to
/// Autopilot WITHOUT chatting first seeds a `conversation_id` claude never persisted, so the
/// first `claude --resume <seed>` prints "No conversation found" and EXITS, tmux then TEARS
/// THE SESSION DOWN, `ensure_session` sees "not alive" and (pre-fix) relaunches with the SAME
/// bad `--resume` forever. A fake can be told a session died; only real tmux establishes that
/// a resumed-ghost pane exits and takes its session with it, which is what drives the loop.
///
/// The `claude` stub reproduces the empirical engine behaviour (resume-ghost → exit 1;
/// `--session-id` → create + stay alive) and records each launch's mode to a file, so the
/// proof is deterministic and FREE. The fix makes the SECOND launch fall back to
/// `--session-id <seed>` (CREATE) exactly once — after which the agent is UP, not looping.
///
/// Hygiene mirrors the sibling acceptance tests: a private per-pid socket, every result
/// recorded into a local, and `kill-server` BEFORE the first assertion, so a failing
/// assertion can never leak a tmux server or session.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// standard_autopilot_resume_ghost_falls_back_to_create_over_real_tmux`.
#[test]
#[ignore]
fn standard_autopilot_resume_ghost_falls_back_to_create_over_real_tmux() {
    if !tmux_available() {
        eprintln!("skipping resume-fallback acceptance test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let socket = TmuxSocket::new("pm-resume");
    let launches = root.join("launches.log");
    let (_stub_bin, tmuxw) = write_resume_fallback_stubs(&root, &launches);
    let driver = TmuxDriver {
        tmux: tmuxw.clone(),
        socket: Some(socket.name().to_string()),
    };

    let id = "resumefb";
    let seed = "seed-never-persisted";
    let session = session_name(id, &root);

    // A fresh Autopilot session with NO conversation_id — the create-and-flip-without-chat
    // shape: the seed is adopted from the registry, not from a persisted ledger cid.
    let paths = ProjectPaths::for_session(&root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    state::write_text_atomic(&paths.brief(), "prove the resume→create fallback\n").unwrap();
    agent_manager::job::save(
        &paths,
        &agent_manager::job::AgentLoopState::fresh(Engine::Claude, Some(1), SystemClock.now()),
    )
    .unwrap();

    let mut sched =
        agent_manager::job_engine::JobScheduler::new(id, root.clone(), id, Engine::Claude, None);
    // pmtui refreshes the seed every sweep; the scheduler holds it until adopted.
    sched.set_registry_seed(Some(seed));
    // Point the seed-existence probe at an EMPTY claude home (no `projects` dir) so it
    // returns None and this test exercises the optimistic-resume → one-shot-create state
    // machine — the fallback that must hold when existence can't be probed (codex, a
    // machine where claude never ran). The probe's own zero-failure fast path is covered
    // by the unit tests `seed_{with,without}_a_transcript_*`.
    sched.set_claude_home(root.join("claude-home"));

    // Sweep until the fallback CREATE has fired (resume-ghost dies, tmux tears the session
    // down, the relaunch falls back to `--session-id`). LAUNCH_GRACE_S (8s) sits between the
    // dead-resume launch and the due relaunch, so allow generous slack.
    let reached_create = sweep_until(&mut sched, &driver, Duration::from_secs(60), || {
        std::fs::read_to_string(&launches)
            .map(|s| s.lines().any(|l| l.starts_with("create ")))
            .unwrap_or(false)
    });
    // Give the create REPL a beat to settle at its prompt before probing liveness.
    let alive_at_end = reached_create
        && wait_until(Duration::from_secs(10), || {
            driver.is_alive(&session).unwrap_or(false)
        });

    // ---- record everything BEFORE tearing the server down ----
    let launches_txt = std::fs::read_to_string(&launches).unwrap_or_default();
    let ledger = agent_manager::job::load(&paths).ok().flatten();

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    // ---- assertions (the server is already gone) ----
    let resume_lines = launches_txt
        .lines()
        .filter(|l| l.starts_with("resume "))
        .count();
    let create_lines = launches_txt
        .lines()
        .filter(|l| l.starts_with("create "))
        .count();
    assert!(
        reached_create,
        "the fallback never created the conversation; launches were:\n{launches_txt}"
    );
    // The FIRST launch resumes the seed (it MIGHT have been a real chat conversation).
    assert_eq!(
        launches_txt.lines().next(),
        Some(format!("resume {seed}").as_str()),
        "the first attempt must RESUME the seed; launches:\n{launches_txt}"
    );
    // THE FIX, in two counts: the relaunch after the resume-ghost died is a CREATE with the
    // SAME id, and it happens exactly once — no create→create loop, no resume→resume loop.
    assert!(
        launches_txt.contains(&format!("create {seed}")),
        "the relaunch must CREATE the same id via --session-id; launches:\n{launches_txt}"
    );
    assert_eq!(
        create_lines, 1,
        "the create-fallback is ONE-SHOT (no create→create loop); launches:\n{launches_txt}"
    );
    assert_eq!(
        resume_lines, 1,
        "only the first attempt resumes — the ghost is not resumed forever; launches:\n{launches_txt}"
    );
    // The agent is UP after the fallback, which is the whole point: no infinite relaunch.
    assert!(
        alive_at_end,
        "the create-fallback must bring the agent UP, not leave it looping; launches:\n{launches_txt}"
    );
    let ledger = ledger.expect("the ledger should be readable");
    assert_eq!(
        ledger.conversation_id.as_deref(),
        Some(seed),
        "the seed was adopted as the conversation id"
    );
    assert!(
        !ledger.resume_unconfirmed,
        "the create-fallback cleared the gate, so future relaunches RESUME the now-real conversation"
    );
}

/// ACCEPTANCE (real tmux): the harness must NOT nudge a claude that is STREAMING an answer.
///
/// This is the Bug-A fact a `FakeDriver` structurally cannot catch. In Claude Code v2.1.x
/// there is NO on-screen busy marker while answer tokens render — no `esc to interrupt`
/// (absent from the build), no spinner near the composer (it vanishes once tokens flow) —
/// only a growing `●`-led response block above the same bare `❯` a WAITING pane shows. So
/// `classify_pane` returns **Idle** for an actively-working agent, for many consecutive
/// frames, and the two-observation gate cannot save it on frame-count alone. The fix is the
/// content-stability gate: a streaming pane's transcript CHANGES between the two
/// `BUSY_RECHECK_S`-apart captures, so `idle_fingerprint` differs and the gate re-arms
/// instead of nudging. Only real tmux can establish the FACT that the transcript actually
/// changes frame to frame while the pane still classifies Idle — a fake returns a fixed
/// capture, so it is byte-stable and would (correctly) nudge.
///
/// The pane is a shell loop that appends a growing block + a bare `❯` + a ticking footer
/// every ~0.4s: it classifies Idle on every capture, stays ALIVE (never a corpse), and
/// changes content faster than the recheck interval — exactly the streaming shape.
///
/// Hygiene mirrors `harness_does_not_nudge_a_dead_pane`: a private per-pid socket, every
/// result recorded into a local, and `kill-server` BEFORE the first assertion. No
/// `pmd`/`pmtui` is started (the scheduler is driven in-process).
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// harness_does_not_nudge_a_streaming_pane`.
#[test]
#[ignore]
fn harness_does_not_nudge_a_streaming_pane() {
    if !tmux_available() {
        eprintln!("skipping streaming-pane test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-stream");
    let driver = TmuxDriver::with_socket(socket.name());
    let id = "streampane";
    let root = dir.path().to_path_buf();
    let session = session_name(id, &root);

    let paths = ProjectPaths::for_session(&root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    state::write_text_atomic(&paths.brief(), "prove a streaming agent is never nudged\n").unwrap();
    let mut ledger =
        agent_manager::job::AgentLoopState::fresh(Engine::Claude, Some(1), SystemClock.now());
    // A pinned conversation id so `ensure_session` treats the pre-launched pane as AlreadyUp.
    ledger.conversation_id = Some("stream-convo".into());
    agent_manager::job::save(&paths, &ledger).unwrap();

    // A pane that STREAMS: each ~0.4s it prints one more `●` line, redraws the bare `❯`, and
    // ticks a footer. `printf '\342\235\257 '` is `❯ ` (what `is_bare_prompt` matches). The
    // loop never exits, so the pane is alive (not a corpse) and its content keeps changing.
    let script = "i=0; while :; do i=$((i+1)); printf '\\342\\227\\217 streaming token %s\\n' \"$i\"; \
         printf '\\342\\235\\257 \\n'; printf 'Context: %s%%\\n' \"$i\"; sleep 0.4; done";
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &["sh".to_string(), "-c".to_string(), script.to_string()],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    // Wait until the pane has painted a bare `❯` at least once.
    let prompt_drawn = launched.is_ok() && wait_for_pane_text(&driver, &session, "❯");

    // ---- premises, recorded before any assertion ----
    // (1) a mid-stream capture classifies IDLE — i.e. capture alone would authorise a nudge.
    let mid = driver.capture_tail(&session, 40).unwrap_or_default();
    let mid_reads_idle =
        agent_manager::tmux::classify_pane(&mid) == agent_manager::tmux::PaneActivity::Idle;
    // (2) two captures ~1.2s apart DIFFER — the stream is live, so `idle_fingerprint` moves.
    let cap_a = driver.capture_tail(&session, 40).unwrap_or_default();
    std::thread::sleep(Duration::from_millis(1200));
    let cap_b = driver.capture_tail(&session, 40).unwrap_or_default();
    let stream_is_live = !cap_a.is_empty() && cap_a != cap_b;

    // ---- drive the REAL scheduler at the streaming pane for several recheck windows ----
    let mut sched = agent_manager::job_engine::JobScheduler::new(
        id,
        root.clone(),
        id,
        agent_manager::registry::Engine::Claude,
        None,
    );
    let clock = SystemClock;
    // ~16s: well past two BUSY_RECHECK_S (5s) windows, so a count-only gate would have
    // nudged by now. Each iteration also lets the stream advance between drives.
    for _ in 0..30 {
        let _ = sched.tick(&driver, &clock);
        std::thread::sleep(Duration::from_millis(550));
    }
    let after = driver.capture_tail(&session, 200).unwrap_or_default();
    let ledger_after = agent_manager::job::load(&paths).ok().flatten();

    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    // ---- assertions (the server is already gone) ----
    launched.expect("launch_interactive should start the streaming session");
    assert!(prompt_drawn, "the stub must paint a bare prompt");
    assert!(
        mid_reads_idle,
        "PREMISE: a streaming frame must classify Idle, or this proves nothing about the \
         false-Idle window. Capture was:\n{mid}"
    );
    assert!(
        stream_is_live,
        "PREMISE: two captures a beat apart must differ, or the stream is not live and the \
         fingerprint gate is not being exercised.\nA:\n{cap_a}\nB:\n{cap_b}"
    );
    // THE BUG, in one assertion: a nudge sets `nudged_at_seq` on the ledger (nudge.rs). If
    // the harness ever typed into this working pane, it is `Some` here. The content-stability
    // gate keeps it `None`: every recheck sees a changed transcript and re-arms.
    let ledger_after = ledger_after.expect("the ledger should still be readable");
    assert!(
        ledger_after.nudged_at_seq.is_none(),
        "the harness nudged a STREAMING agent (nudged_at_seq={:?}); run={:?}",
        ledger_after.nudged_at_seq,
        ledger_after.run
    );
    // Belt and braces: the nudge prompt's opening line must not have landed in the pane.
    assert!(
        !after.contains("You are a long-running agent"),
        "the nudge prompt reached a working pane:\n{after}"
    );
}

/// Sweep the scheduler like pmd does until `done` or the bound elapses.
///
/// `PM_TEST_TRACE=1` prints each tick's outcome plus the live session list. That hook is
/// not decoration: a real-substrate failure here surfaces as "the supervisor was never
/// consulted", which says nothing at all, and BOTH bugs found while writing this test
/// (a corrupted stub that made the worker pane exit immediately, and its cause) were
/// invisible without exactly these two facts.
fn sweep_until(
    sched: &mut agent_manager::job_engine::JobScheduler,
    driver: &TmuxDriver,
    bound: Duration,
    mut done: impl FnMut() -> bool,
) -> bool {
    let start = Instant::now();
    let clock = SystemClock;
    while start.elapsed() < bound {
        let out = sched.tick(driver, &clock);
        if std::env::var("PM_TEST_TRACE").is_ok() {
            let shown = match &out {
                Ok(t) => format!("{t:?}"),
                Err(e) => format!("{e:#}"),
            };
            let sessions = Command::new(&driver.tmux)
                .args(["-L", driver.socket().unwrap_or("default")])
                .args(["list-sessions", "-F", "#{session_name}"])
                .output()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            eprintln!(
                "[sweep {:?}] tick -> {shown} | sessions=[{sessions}]",
                start.elapsed()
            );
        }
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    done()
}

/// ACCEPTANCE (real tmux): the LLM supervisor replaces the canned auto-approval with
/// GOAL-AWARE content, and a reply carrying an escape sequence escalates with **nothing
/// typed**.
///
/// This is the bug class a `FakeDriver` unit test structurally cannot catch, and there are
/// three distinct tmux facts in it:
///
///  1. **`send_keys` transmits control bytes verbatim.** Asserted directly, against a real
///     pane, by capturing the raw bytes the pane's process receives. That is the ONLY
///     reason Rule 3 (reject all C0/C1) has to live in the harness: nothing between the
///     harness and the agent will clean up after it, so `\x1b[Z` would arrive as
///     **shift+tab, cycling claude's permission mode**, and `\r` would submit a second
///     message. A fake `send_keys` records a string and proves none of this.
///  2. **The real detached-spawn substrate.** The consult goes out through
///     `Driver::spawn_step` (tee'd wrapper + atomic done-signal) into a `pmsup-` session
///     and is reaped on a LATER sweep — so this exercises the real argv
///     `worker::build_supervisor_command` builds, the real `timeout` wrapper, real PATH
///     resolution, and the real `--output-format json` envelope unwrap.
///  3. **Nothing is leaked.** The consult's own session must be killed on reap.
///
/// The supervisor is a STUB emitting a known verdict, so the test is deterministic and
/// FREE: `claude` is stubbed on the child PATH for both roles (see `write_stubs`), and the
/// scheduler is told that its custom driver provides the binary even when the parent process
/// PATH does not. The stub echoes the nonce the harness minted, so a harness that stopped
/// minting or checking one would fail here rather than pass vacuously.
///
/// Hygiene, mirroring `harness_does_not_nudge_a_dead_pane`: a private per-pid socket,
/// every result recorded into a local, and `kill-server` BEFORE the first assertion — so a
/// failing assertion can never leak a tmux server or session. No `pmd`/`pmtui` is started
/// (the scheduler is driven in-process), so there is no daemon to kill.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// supervisor_answers_from_the_goal_over_real_tmux`.
#[test]
#[ignore]
fn supervisor_answers_from_the_goal_over_real_tmux() {
    if !tmux_available() {
        eprintln!("skipping supervisor acceptance test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let socket = TmuxSocket::new("pm-sup");
    let typed = root.join("typed.log");
    let sup_prompt = root.join("consult-prompt.txt");
    let (stub_bin, tmuxw) = write_stubs(&root, &typed, &sup_prompt, REPLY_PICKS_DPRINT);
    // The driver, with `tmux` pointed at the PATH-injecting wrapper (see `write_stubs`).
    let driver = TmuxDriver {
        tmux: tmuxw.clone(),
        socket: Some(socket.name().to_string()),
    };

    // ---- premise 0: the STUB, not the real CLI, is what a pane resolves `claude` to.
    // Asserted on OUTPUT (never exit status) and BEFORE anything is driven, because if the
    // real `claude` won this test would spend real tokens instead of failing.
    let probe_done = root.join("steps/probe.done");
    let probe_log = root.join("steps/probe.log");
    let probe = driver.spawn_step(
        "pmsup-probe",
        &root,
        &["sh".into(), "-c".into(), "command -v claude".into()],
        &probe_done,
        &probe_log,
    );
    let probe_resolved = probe.is_ok()
        && wait_until(Duration::from_secs(10), || {
            std::fs::read_to_string(&probe_log)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        });
    let resolved_claude = std::fs::read_to_string(&probe_log).unwrap_or_default();
    // Checked HERE rather than with the other assertions, and deliberately breaking the
    // "kill-server before assertions" ordering: everything below DRIVES the scheduler, so a
    // pane that resolved the REAL `claude` would spend real tokens before the tail
    // assertions could complain. The server is torn down first, so nothing leaks either way.
    if resolved_claude.trim() != stub_bin.join("claude").display().to_string() {
        let _ = Command::new("tmux")
            .arg("-L")
            .arg(socket.name())
            .arg("kill-server")
            .status();
        panic!(
            "a pane must resolve `claude` to the STUB at {} before anything is driven, or \
             this test would spend real tokens; `command -v claude` said {resolved_claude:?} \
             (probe produced output: {probe_resolved})",
            stub_bin.join("claude").display()
        );
    }

    // ---- premise 1: `send_keys` puts control bytes on the wire UNCHANGED.
    // The pane's process writes the raw bytes it received to a file, so the assertion is
    // about bytes rather than about how a terminal chose to render them.
    let bytes_out = root.join("raw-bytes.txt");
    let sink = format!(
        "IFS= read -r l; printf '%s' \"$l\" | od -An -tx1 > '{}'",
        bytes_out.display()
    );
    let sink_up = driver
        .launch_interactive(
            "pmsup-bytes",
            &root,
            &["sh".into(), "-c".into(), sink],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .is_ok();
    let esc_sent = sink_up && driver.send_keys("pmsup-bytes", "ESC\u{1b}[Z END").is_ok();
    let esc_arrived = esc_sent
        && wait_until(Duration::from_secs(10), || {
            std::fs::read_to_string(&bytes_out)
                .map(|s| s.contains("1b"))
                .unwrap_or(false)
        });
    let raw_bytes = std::fs::read_to_string(&bytes_out).unwrap_or_default();

    // ---- part A: a goal-aware decision reaches the worker.
    let paths_a = seed_supervisor_session(
        &root,
        "supa",
        "Keep the changelog tooling consistent. dprint is already vendored in this repo; \
         do not add a second formatter.\n",
    );
    std::fs::write(paths_a.needs_you(), CONSULTABLE_MARKER).unwrap();
    let mut sched_a = agent_manager::job_engine::JobScheduler::new(
        "supa",
        root.clone(),
        "supa",
        agent_manager::registry::Engine::Claude,
        None,
    );
    sched_a.set_decider_binary_available(true);
    let advised = sweep_until(&mut sched_a, &driver, Duration::from_secs(90), || {
        std::fs::read_to_string(&typed)
            .map(|s| s.contains("session supervisor"))
            .unwrap_or(false)
    });
    let typed_a = std::fs::read_to_string(&typed).unwrap_or_default();
    let consult_prompt_a = std::fs::read_to_string(&sup_prompt).unwrap_or_default();
    // The consult's own tee'd log — the first place to look when the stub misbehaved (a
    // shell syntax error in it shows up here and nowhere else).
    let consult_log_a = std::fs::read_to_string(paths_a.advice_log(1)).unwrap_or_default();
    let ledger_a = agent_manager::job::load(&paths_a).ok().flatten();
    // No `pmsup-` session may survive its reap.
    let leaked_a = Command::new(&tmuxw)
        .args([
            "-L",
            socket.name(),
            "list-sessions",
            "-F",
            "#{session_name}",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    // ---- part B: a reply carrying an escape sequence escalates, typing NOTHING.
    // A second stub dir for a second session, so part A's evidence is untouched.
    let dir_b = tempfile::tempdir().unwrap();
    let root_b = dir_b.path().to_path_buf();
    let typed_b_path = root_b.join("typed.log");
    let (_bin_b, tmuxw_b) = write_stubs(
        &root_b,
        &typed_b_path,
        &root_b.join("prompt.txt"),
        REPLY_WITH_ESCAPE,
    );
    let driver_b = TmuxDriver {
        tmux: tmuxw_b,
        socket: Some(socket.name().to_string()),
    };
    let paths_b = seed_supervisor_session(&root_b, "supb", "Keep the changelog consistent.\n");
    std::fs::write(paths_b.needs_you(), CONSULTABLE_MARKER).unwrap();
    let mut sched_b = agent_manager::job_engine::JobScheduler::new(
        "supb",
        root_b.clone(),
        "supb",
        agent_manager::registry::Engine::Claude,
        None,
    );
    sched_b.set_decider_binary_available(true);
    let escalated_b = sweep_until(&mut sched_b, &driver_b, Duration::from_secs(90), || {
        agent_manager::job::load(&paths_b)
            .ok()
            .flatten()
            .is_some_and(|l| matches!(l.run, agent_manager::job::JobRun::Blocked { .. }))
    });
    let typed_b = std::fs::read_to_string(&typed_b_path).unwrap_or_default();
    let ledger_b = agent_manager::job::load(&paths_b).ok().flatten();

    // ---- teardown: both servers gone BEFORE the first assertion.
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    // ================= assertions (the server is already gone) =================
    assert!(
        probe_resolved,
        "the `command -v claude` probe never produced output"
    );
    assert_eq!(
        resolved_claude.trim(),
        stub_bin.join("claude").display().to_string(),
        "a pane must resolve `claude` to the STUB, or this test spends real tokens"
    );
    // PREMISE for Rule 3: nothing downstream sanitizes, so the harness must.
    assert!(sink_up, "could not launch the raw-byte sink pane");
    assert!(
        esc_arrived,
        "PREMISE: `send_keys` must transmit ESC (0x1b) to the pane's process VERBATIM — if \
         it were filtered somewhere, Rule 3 would be unnecessary. Bytes were:\n{raw_bytes}"
    );

    // ---- part A
    assert!(
        !consult_prompt_a.is_empty(),
        "the supervisor was never consulted at all; the consult's log said:\n{consult_log_a}"
    );
    assert!(
        consult_prompt_a.contains("[0] prettier") && consult_prompt_a.contains("[1] dprint"),
        "the consult must enumerate the harness's options BY INDEX:\n{consult_prompt_a}"
    );
    assert!(
        consult_prompt_a.contains("dprint is already vendored"),
        "the consult must carry the human's GOAL:\n{consult_prompt_a}"
    );
    assert!(
        advised,
        "the worker never received goal-aware advice. Typed log was:\n{typed_a}"
    );
    // THE FEATURE, in one assertion: the option the goal implies, named by the HARNESS.
    assert!(
        typed_a.contains("option 2 — dprint"),
        "the worker must be told WHAT to do, not merely that it was approved:\n{typed_a}"
    );
    assert!(
        typed_a.contains("the goal already vendors dprint"),
        "the supervisor's reason must reach the worker:\n{typed_a}"
    );
    assert!(
        !typed_a.contains("Auto-approved"),
        "the canned string must be GONE when a supervisor answered:\n{typed_a}"
    );
    let ledger_a = ledger_a.expect("part A ledger should be readable");
    assert!(
        !matches!(ledger_a.run, agent_manager::job::JobRun::Blocked { .. }),
        "a usable verdict must not escalate, got {:?}",
        ledger_a.run
    );
    assert!(
        !leaked_a.lines().any(|s| s.starts_with("pmsup-supa")),
        "a reaped consult must leave no `pmsup-` session behind; live sessions were:\n{leaked_a}"
    );

    // ---- part B
    assert!(
        escalated_b,
        "an escape-sequence reply must escalate to the human. Ledger was {:?}",
        ledger_b.as_ref().map(|l| l.run.clone())
    );
    let ledger_b = ledger_b.expect("part B ledger should be readable");
    assert_eq!(
        ledger_b.open_stops.len(),
        1,
        "exactly one stop, surfaced once"
    );
    assert_eq!(
        ledger_b.open_stops[0].kind,
        pmstate::StopKind::Capability,
        "Rule 2: `Capability` is forced Hard, so BOTH tiers escalate. `WorkerStuck` floors \
         to Medium and (Autopilot, Medium) => AutoFlow — it would auto-approve the very \
         reply being refused"
    );
    assert_ne!(ledger_b.open_stops[0].kind, pmstate::StopKind::WorkerStuck);
    // THE BUG this prevents: not one byte of the tainted advice may reach the pane.
    assert!(
        !typed_b.contains("pwned") && !typed_b.contains('\u{1b}'),
        "an escape-sequence reply reached the worker's pane:\n{typed_b:?}"
    );
    assert!(
        ledger_b.pending_context.is_none(),
        "and nothing may be queued for delivery either: {:?}",
        ledger_b.pending_context
    );
    drop(dir);
    drop(dir_b);
}

#[test]
#[ignore]
fn supervisor_selects_a_live_multi_choice_end_to_end_over_real_tmux() {
    if !tmux_available() {
        eprintln!("skipping live dialog-supervisor test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let socket = TmuxSocket::new("pm-dialog-supervisor");
    let selected = root.join("selected.txt");
    let prompt = root.join("consult-prompt.txt");
    let typed = root.join("unused-typed.txt");
    let (stub_bin, tmuxw) = write_stubs(&root, &typed, &prompt, REPLY_PICKS_DPRINT);
    let worker = format!(
        r#"#!/bin/sh
mode=worker
for a in "$@"; do
  case "$a" in -p) mode=sup ;; esac
  last="$a"
done
if [ "$mode" = sup ]; then
  nonce=`printf '%s\n' "$last" | sed -n 's/^NONCE: //p' | head -1`
  printf '%s\n' "$last" > '{}'
  printf '{{"type":"result","subtype":"success","result":"{{\\"nonce\\":\\"%s\\",\\"action\\":\\"select_option\\",\\"option_index\\":5,\\"reason\\":\\"run the two remaining layers\\"}}"}}\n' "$nonce"
  exit 0
fi
stty -echo -icanon min 1 time 0
cursor=1
c1=0
c2=0
c3=0
c4=0
draw() {{
  printf '\033[2J\033[H'
  printf '←  ☐ Test layers  ✔ Submit  →\n'
  printf ' Which test layers should I run?\n'
  [ "$cursor" = 1 ] && p='❯' || p=' '
  [ "$c1" = 1 ] && b='✔' || b=' '
  printf ' %s 1. [%s] unit\n' "$p" "$b"
  [ "$cursor" = 2 ] && p='❯' || p=' '
  [ "$c2" = 1 ] && b='✔' || b=' '
  printf ' %s 2. [%s] integration\n' "$p" "$b"
  [ "$cursor" = 3 ] && p='❯' || p=' '
  [ "$c3" = 1 ] && b='✔' || b=' '
  printf ' %s 3. [%s] real tmux\n' "$p" "$b"
  [ "$cursor" = 4 ] && p='❯' || p=' '
  [ "$c4" = 1 ] && b='✔' || b=' '
  printf ' %s 4. [%s] Type something\n' "$p" "$b"
  [ "$cursor" = 5 ] && p='❯' || p=' '
  printf ' %s    Submit\n' "$p"
  printf ' Enter to select · ↑/↓ to navigate · Esc to cancel\n'
}}
draw
while :; do
  c=$(dd bs=1 count=1 2>/dev/null)
  if [ -z "$c" ]; then
    # Claude advances one row after toggling a checkbox.
    case "$cursor" in
      1) c1=$((1-c1)); cursor=2; draw ;;
      2) c2=$((1-c2)); cursor=3; draw ;;
      3) c3=$((1-c3)); cursor=4; draw ;;
      4) c4=$((1-c4)); cursor=5; draw ;;
      5)
        printf '%s%s%s%s\n' \
          "$([ "$c1" = 1 ] && printf 1)" \
          "$([ "$c2" = 1 ] && printf 2)" \
          "$([ "$c3" = 1 ] && printf 3)" \
          "$([ "$c4" = 1 ] && printf 4)" > '{}'
        sleep 5
        ;;
    esac
    continue
  fi
  if [ "$c" = "$(printf '\033')" ]; then
    dd bs=1 count=1 2>/dev/null >/dev/null
    key=$(dd bs=1 count=1 2>/dev/null)
    [ "$key" = B ] && [ "$cursor" -lt 5 ] && cursor=$((cursor+1))
    [ "$key" = A ] && [ "$cursor" -gt 1 ] && cursor=$((cursor-1))
    draw
  fi
done
"#,
        prompt.display(),
        selected.display()
    );
    std::fs::write(stub_bin.join("claude"), worker).unwrap();

    let driver = TmuxDriver {
        tmux: tmuxw,
        socket: Some(socket.name().to_string()),
    };
    let probe_done = root.join("steps/dialog-probe.done");
    let probe_log = root.join("steps/dialog-probe.log");
    let probe = driver.spawn_step(
        "pmsup-dialog-probe",
        &root,
        &["sh".into(), "-c".into(), "command -v claude".into()],
        &probe_done,
        &probe_log,
    );
    let probe_resolved = probe.is_ok()
        && wait_until(Duration::from_secs(10), || {
            std::fs::read_to_string(&probe_log).is_ok_and(|value| !value.trim().is_empty())
        });
    let resolved_claude = std::fs::read_to_string(&probe_log).unwrap_or_default();
    if resolved_claude.trim() != stub_bin.join("claude").display().to_string() {
        let _ = Command::new("tmux")
            .arg("-L")
            .arg(socket.name())
            .arg("kill-server")
            .status();
        panic!(
            "dialog test must resolve the stub at {}, got {resolved_claude:?} \
             (probe produced output: {probe_resolved})",
            stub_bin.join("claude").display()
        );
    }
    let id = "dialog-supervisor";
    let paths = seed_supervisor_session(
        &root,
        id,
        "Run integration and real-tmux coverage; unit coverage already passed.\n",
    );
    let session = session_name(id, &root);
    let launched = driver.launch_interactive(
        &session,
        &root,
        &[
            "env".into(),
            "-u".into(),
            "CLAUDECODE".into(),
            "claude".into(),
        ],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let menu_ready = launched.is_ok() && wait_for_pane_text(&driver, &session, "Which test layers");
    let mut scheduler =
        agent_manager::job_engine::JobScheduler::new(id, &root, id, Engine::Claude, None);
    scheduler.set_decider_binary_available(true);
    let selected_live = menu_ready
        && sweep_until(&mut scheduler, &driver, Duration::from_secs(20), || {
            std::fs::read_to_string(&selected).is_ok_and(|value| value.trim() == "23")
        });
    let ledger = agent_manager::job::load(&paths).ok().flatten();
    let consult_prompt = std::fs::read_to_string(&prompt).unwrap_or_default();

    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    launched.expect("launch live choice fixture");
    assert!(
        probe_resolved,
        "stub resolution probe should produce output"
    );
    assert!(menu_ready, "worker menu should render");
    assert!(
        consult_prompt.contains("[5] integration + real tmux"),
        "decider sees bounded checkbox combinations: {consult_prompt}"
    );
    assert!(
        selected_live,
        "active probe + validated choice should check integration and real tmux, then submit"
    );
    let ledger = ledger.expect("dialog selection should leave a ledger");
    assert!(
        ledger
            .last_status
            .as_deref()
            .is_some_and(|status| status.contains("options 2, 3")),
        "selection audit missing: {:?}",
        ledger.last_status
    );
    assert!(ledger.advice_inflight.is_none());
}

/// ACCEPTANCE (real tmux, real `pmd`): after the agent reports a `next_step`, the NEXT nudge
/// echoes it VERBATIM. This is the end-to-end proof of the worker-lane mirror feature on real
/// substrate — a real `pmd` on a private socket driving a session whose stubbed `claude`
/// reports a unique `next_step`, and the subsequent nudge (as RECEIVED by the stub) must carry
/// that exact text.
///
/// Everything below `pmd`'s CLI is the production path: the daemon reconciles an Autopilot
/// `Mode::AgentLoop` row, `ensure_session` launches the persistent `claude` (the stub, resolved
/// off `pmd`'s own PATH — a tmux pane inherits its creating client's PATH, and every tmux `pmd`
/// spawns is a child of `pmd`), the marker disposer mirrors `next_step` into `last_plan`, and
/// `loop_nudge_prompt` quotes it back. Nothing here is faked but the agent.
///
/// The assertion reads the FILE the stub tees received prompts into — never `status.success()`.
/// (`pmd` never exits on its own here: an agent-loop session has no "done", a human closes it —
/// so a clean exit-code is not even available; a crash backstop `try_wait` guards against the
/// daemon dying before it could drive, which would make the wait vacuously time out.)
///
/// NON-VACUITY: `marker_text` is UNIQUE per run and the stub writes it ONLY into `needs-you.json`,
/// never into `@TYPED@`. So a hit in `@TYPED@` can have exactly one cause — the nudge quoted the
/// agent's reported `next_step` back. Were the mirror broken, `last_plan` would stay empty, the
/// second nudge would omit the "Your next step" line, and the wait would time out and FAIL. And
/// the second nudge cannot even fire until the `awaiting_report` gate clears, which requires this
/// marker to have been disposed (its `seq` bumps `last_marker_seq`) — the very act that sets
/// `last_plan` — so the echoing nudge deterministically follows the report.
///
/// Hygiene: a private per-pid socket (RAII [`TmuxSocket`] kills the server + removes its socket
/// file on drop, even on a panicking assertion), and the spawned `pmd` is SIGTERM'd (its reaper
/// clears the `pmloop-` it owns) then hard-killed BEFORE the assertions — so a failure can leak
/// neither a daemon nor a server.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// the_next_nudge_echoes_the_agents_reported_next_step`.
#[test]
#[ignore = "real tmux: run with --ignored --test-threads=1"]
fn the_next_nudge_echoes_the_agents_reported_next_step() {
    if !tmux_available() {
        eprintln!("skipping next_step-echo acceptance test: no tmux");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let id = "replay";

    // A UNIQUE marker (pid + nanos, only `[A-Za-z0-9-]` so no shell/JSON escaping) so a hit in
    // `@TYPED@` can ONLY have come from THIS run's reported next_step — never incidental prompt
    // text, and never a stale file left by another run.
    let marker_text = format!(
        "REPLAY-MARKER-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    // The per-session on-disk state pmtui's intake writes: an Autopilot config (what
    // `pmd_drives_row` requires for an agent-loop row), the goal, and a fresh 1s-cadence ledger.
    // `seed_supervisor_session` writes it under `ProjectPaths::for_session(root, id)`, exactly the
    // paths `daemon::entry_paths` derives for a `Mode::AgentLoop` row.
    let paths = seed_supervisor_session(
        &root,
        id,
        "prove the nudge echoes the agent's reported next_step\n",
    );

    // The stub `claude`: presents idle, tees every received line to `@TYPED@`, and on each nudge
    // writes a WakeReport carrying our unique next_step (into `needs-you.json`, NOT `@TYPED@`).
    let typed = root.join("typed.log");
    let stub = next_step_reporting_claude_stub(&typed, &paths.needs_you(), &marker_text);
    let stub_bin = install_claude_stub(dir.path(), &stub);

    // The AgentLoop registry row `pmd` drives. Enabled + the Autopilot per-session config above
    // is exactly what `pmd_drives_row` requires; empty `coordinator_cmd` + `Mode::AgentLoop`
    // routes `Runner::build` to the `JobScheduler`.
    let reg_path = root.join("registry.json");
    let mut reg = Registry::default();
    reg.projects.push(ProjectEntry {
        id: id.to_string(),
        display_name: None,
        root: root.clone(),
        enabled: true,
        mode: Mode::AgentLoop,
        engine: Some(Engine::Claude),
        worker_model: None,
        initial_prompt: None,
        task_title: None,
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s: Some(1),
    });
    reg.save(&reg_path).unwrap();

    // Start a REAL `pmd` on a private per-pid socket. Its PATH carries the stub dir first, so
    // every tmux client `pmd` spawns creates panes that resolve `claude` to the stub. Spawned
    // (not `timeout`-wrapped): an agent-loop session never reaches "done", so `pmd` runs until we
    // kill it. `PM_TURN_HOOK=off` keeps the daemon on the content-stability idle gate (the stub
    // ignores the injected `--settings` turn hook, so no turn-signal file would ever appear).
    let socket = TmuxSocket::new("pm-replay");
    let path_env = format!(
        "{}:{}",
        stub_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_pmd"))
        .arg("--registry")
        .arg(&reg_path)
        .arg("--socket")
        .arg(socket.name())
        .arg("--tick-ms")
        .arg("200")
        .env("PATH", path_env)
        .env("ECC_GATEGUARD", "off")
        .env("PM_TURN_HOOK", "off")
        .spawn()
        .expect("spawn pmd");

    // THE PROOF: poll `@TYPED@` (the bytes the stub RECEIVED) until it contains our unique marker
    // — i.e. the nudge AFTER the agent reported its next_step quoted that next_step back verbatim.
    // Bounded wait, never a fixed sleep. The budget covers cold-start grace (8s) + two
    // idle-confirmation windows (~5s each) for two nudges, with generous slack for a loaded box.
    let typed_contents = wait_for_file_contents(&typed, &marker_text, Duration::from_secs(60));

    // Crash backstop, read before teardown: did `pmd` die before it could drive? If so the wait
    // above timed out for the wrong reason and the test proves nothing — surface that distinctly.
    let pmd_died = matches!(child.try_wait(), Ok(Some(_)));

    // ---- teardown BEFORE the assertions ----
    // SIGTERM first so `pmd`'s signal reaper tears down the `pmloop-` session it owns; then
    // hard-kill/reap the child. The RAII `TmuxSocket` kills the server + removes the lingering
    // socket file on drop (runs even if an assertion below panics).
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    std::thread::sleep(Duration::from_millis(300));
    let _ = child.kill();
    let _ = child.wait();
    drop(socket);

    // ---- assertions (server already gone) ----
    assert!(
        !pmd_died,
        "pmd exited before it could drive the session — the assertion below would be vacuous. \
         @TYPED@ held:\n{typed_contents}"
    );
    assert!(
        typed_contents.contains(&marker_text),
        "the nudge after the agent reported its next_step must echo it VERBATIM.\n\
         Looked for {marker_text:?} in @TYPED@, which held:\n{typed_contents}"
    );
    drop(dir);
}

// ======================================================================================
// LAYER 2 — real-tmux acceptance for the context-sync eval harness (Task 7).
//
// L2-1 pins the Milestone-B decider seam firing through the REAL detached substrate: a
// stubbed worker on a live pane writes a `working` marker, the in-process scheduler
// disposes it, and the on-disk `digest`/`situation`/`raw.jsonl` carry the result. The
// unit scenario S1 (`src/job_engine/tests/scenarios.rs`) proves the LOGIC over a
// `FakeDriver`; L2-1 proves the PLUMBING — a real `tmux` pane, a real capture, a real
// atomic marker write observed off disk. L2-2/L2-3 encode the DESIRED Milestone-E nudge
// shape (skill trigger + "Since last wake" block; the non-negotiable rule surviving the
// skill-absent degrade); they are `#[ignore = "acceptance: Milestone E — real tmux"]`
// and WILL NOT pass until E lands — that flip is E's exit gate.
// ======================================================================================

/// How many nudges a teeing stub has RECEIVED, counted by the harness nudge's stable
/// opening line. The whole ~4 KB prompt is teed line-by-line into `@TYPED@`, and this
/// first line appears exactly once per nudge, so it is the deterministic nudge counter
/// (a substring of the wrapped body could straddle a line boundary — this cannot).
fn nudges_in(typed: &str) -> usize {
    typed
        .lines()
        .filter(|l| l.starts_with("You are a long-running agent"))
        .count()
}

/// L2-1 (BASELINE, real tmux): a disposed `working` decision lands in
/// `raw.jsonl`/`situation`/`digest` on a LIVE pane.
///
/// This is the plumbing twin of unit scenario S1. A `FakeDriver` proves the disposer's
/// logic; only a real detached pane proves the seam survives the real substrate — a real
/// `claude`-shaped REPL that classifies Idle, receives the harness nudge over
/// `send-keys`, writes its `needs-you.json` marker atomically (tmp+rename), and has that
/// marker observed and disposed by a later in-process sweep. After the sweep the test
/// reads the ledger + `raw.jsonl` OFF DISK (never an exit code) and asserts B's seam
/// fired: `digest.disposed >= 1`, at least one `raw.jsonl` line, `situation.state ==
/// Working`.
///
/// Hygiene mirrors the sibling acceptance tests: a private per-pid socket (RAII
/// [`TmuxSocket`] kills the server + removes the socket file on drop, even on a panicking
/// assertion). The scheduler runs in-process, so there is no daemon to kill.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// a_disposed_decision_lands_on_disk_over_real_tmux`.
#[test]
#[ignore]
fn a_disposed_decision_lands_on_disk_over_real_tmux() {
    if !tmux_available() {
        eprintln!("skipping L2-1 disposed-decision test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-l2-dispose");
    let driver = TmuxDriver::with_socket(socket.name());
    let id = "l2dispose";
    let root = dir.path().to_path_buf();
    let session = session_name(id, &root);

    // The on-disk shape pmtui's intake writes: an Autopilot config + a goal + a fresh
    // 1s-cadence ledger. `conversation_id` is pinned so a live pre-launched pane is
    // adopted `AlreadyUp` rather than relaunched (`ensure_session` keys off `is_alive`).
    let paths = ProjectPaths::for_session(&root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    state::write_text_atomic(&paths.brief(), "prove the B seam fires on a live pane\n").unwrap();
    let mut ledger =
        agent_manager::job::AgentLoopState::fresh(Engine::Claude, Some(1), SystemClock.now());
    ledger.conversation_id = Some("l2-dispose-convo".into());
    agent_manager::job::save(&paths, &ledger).unwrap();

    // The stubbed worker: a bare-prompt REPL (classifies Idle) that, on each arriving
    // nudge, OVERWRITES `needs-you.json` with a `{"state":"working"}` marker (seq = Unix
    // time, atomic tmp+rename). So the flow the seam needs is real: nudge -> marker ->
    // observe -> dispose.
    let count_file = root.join("nudges.log");
    let stub = nudge_counting_claude_stub(&count_file, &paths.needs_you());
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &["sh".into(), "-c".into(), stub],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let prompt_drawn = launched.is_ok() && wait_for_pane_text(&driver, &session, ">");

    // Drive the REAL scheduler until the marker has been disposed (poll the ledger off
    // disk), bounded well past the two idle-confirmation windows (~5s each) + a nudge +
    // one more sweep to observe the marker.
    let mut sched = agent_manager::job_engine::JobScheduler::new(
        id,
        root.clone(),
        id,
        agent_manager::registry::Engine::Claude,
        None,
    );
    let disposed = sweep_until(&mut sched, &driver, Duration::from_secs(45), || {
        agent_manager::job::load(&paths)
            .ok()
            .flatten()
            .is_some_and(|l| l.digest.disposed >= 1)
    });

    // Read the observables OFF DISK before teardown.
    let ledger_after = agent_manager::job::load(&paths).ok().flatten();
    let raw_jsonl = std::fs::read_to_string(paths.raw_jsonl()).unwrap_or_default();

    let _ = driver.terminate(&session);
    drop(socket);

    // ---- assertions (the server is already gone) ----
    launched.expect("launch_interactive should start the stub session");
    assert!(prompt_drawn, "the stub must paint a bare prompt first");
    let ledger_after = ledger_after.expect("the ledger should be readable after the sweep");
    assert!(
        disposed,
        "the worker's `working` marker was never disposed; digest={:?}, run={:?}",
        ledger_after.digest, ledger_after.run
    );
    // THE SEAM, off disk: the per-kind counter bumped through the real substrate.
    assert!(
        ledger_after.digest.disposed >= 1,
        "digest.disposed must be >= 1 after a live dispose, was {}",
        ledger_after.digest.disposed
    );
    // The machine full-fidelity side-file got its line.
    let raw_lines: Vec<&str> = raw_jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        !raw_lines.is_empty(),
        "raw.jsonl must carry at least one disposed-decision line; file held:\n{raw_jsonl}"
    );
    // The projected situation snapshot reflects the worker's reported state.
    let situation = ledger_after
        .situation
        .expect("a disposed decision must leave a situation snapshot");
    assert_eq!(
        situation.state,
        WakeState::Working,
        "situation.state must mirror the worker's `working` report"
    );
    drop(dir);
}

/// Seed an Autopilot agent-loop session, pre-launch the teeing next-step stub as its live
/// pane, and drive the in-process scheduler until at least `want_nudges` nudges have been
/// teed into `@TYPED@` (or the bound elapses). Returns the `@TYPED@` contents after tearing
/// the server down. The stub disposes its own marker on the first nudge (clearing the
/// awaiting-report gate), so the SECOND nudge is the one that carries the echo / signal
/// block — the shape L2-2/L2-3 assert about.
fn typed_after_nudges(prefix: &str, id: &str, goal: &str, want_nudges: usize) -> (String, bool) {
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new(prefix);
    let driver = TmuxDriver::with_socket(socket.name());
    let root = dir.path().to_path_buf();
    let session = session_name(id, &root);

    let paths = ProjectPaths::for_session(&root, id);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy: Tier::Autopilot,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .unwrap();
    state::write_text_atomic(&paths.brief(), goal).unwrap();
    let mut ledger =
        agent_manager::job::AgentLoopState::fresh(Engine::Claude, Some(1), SystemClock.now());
    ledger.conversation_id = Some(format!("{id}-convo"));
    agent_manager::job::save(&paths, &ledger).unwrap();

    // A UNIQUE next_step so the echo it drives is non-incidental (and never itself the
    // skill trigger / "Since last wake" text L2-2 looks for).
    let marker_text = format!("L2E-STEP-{}", std::process::id());
    let typed = root.join("typed.log");
    let stub = next_step_reporting_claude_stub(&typed, &paths.needs_you(), &marker_text);
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &["sh".into(), "-c".into(), stub],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let _ = launched.is_ok() && wait_for_pane_text(&driver, &session, ">");

    // The worker-skill name is pmd-owned. Adoption replaces any old per-skill directory with the
    // canonical Claude alias without touching sibling project skills.
    std::fs::create_dir_all(paths.claude_project_skill_dir()).unwrap();
    std::fs::write(paths.claude_project_skill_file(), "old worker skill").unwrap();
    std::fs::write(paths.claude_project_skill_dir().join("old-note"), "old").unwrap();

    let mut sched = agent_manager::job_engine::JobScheduler::new(
        id,
        root.clone(),
        id,
        agent_manager::registry::Engine::Claude,
        None,
    );
    let _ = sweep_until(&mut sched, &driver, Duration::from_secs(60), || {
        std::fs::read_to_string(&typed)
            .map(|s| nudges_in(&s) >= want_nudges)
            .unwrap_or(false)
    });
    let typed_contents = std::fs::read_to_string(&typed).unwrap_or_default();
    // Milestone E: whether the NATIVE worker SKILL.md landed in the tempdir work_dir. Captured
    // BEFORE `drop(dir)` removes the tempdir. `ensure_session` installs it on first ADOPTION
    // (`alive && !installed`) of the pre-launched stub pane, so the FIRST sweep writes it — the
    // on-disk install does not depend on the (stub, not real claude) engine binary.
    let skill_landed = paths.canonical_worker_skill_file().exists()
        && std::fs::symlink_metadata(paths.claude_project_skill_dir())
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        && paths.claude_project_skill_file().exists()
        && !paths.claude_project_skill_dir().join("old-note").exists();

    let _ = driver.terminate(&session);
    drop(socket);
    drop(dir);
    (typed_contents, skill_landed)
}

/// L2-2 (ACC:E, real tmux): the Milestone-E signal-flag nudge + skill trigger reach the
/// pane. After E lands, the big static protocol (the full WakeReport schema, the four
/// operating bullets) MOVES OUT into the `agent-manager-worker` skill and the nudge instead
/// carries a skill trigger. This asserts that DESIRED shape over real substrate: `@TYPED@` (the
/// bytes the stub RECEIVED) must carry the skill trigger and must NOT still inline the full
/// schema. (A steady wake carries NO "Since last wake" block — it appears only when a signal
/// fires; that conditional is unit-pinned by s_e1/s_e2/s_e3.)
///
/// It WILL NOT pass today — today's nudge inlines the schema and has no skill trigger — which
/// is the point: un-ignoring it (and seeing it pass) is Milestone E's real-tmux exit gate.
///
/// (Was `#[ignore = "acceptance: Milestone E — real tmux"]` — un-ignored as E's real-tmux gate;
/// still gated on tmux PRESENCE, not on `#[ignore]`.)
#[test]
fn the_signal_flag_nudge_and_skill_trigger_reach_the_pane() {
    if !tmux_available() {
        eprintln!("skipping L2-2 signal-flag test: tmux not available");
        return;
    }
    let (typed, skill_landed) = typed_after_nudges(
        "pm-l2-signal",
        "l2signal",
        "prove the E signal-flag nudge reaches the pane\n",
        2,
    );
    // The canonical worker skill lands under .agents and Claude discovers it through .claude.
    assert!(
        skill_landed,
        "the worker skill must land under .agents with a Claude compatibility symlink"
    );
    // E's spec: the nudge points at the worker skill instead of inlining the protocol.
    assert!(
        typed.contains("Continue the goal per your /agent-manager-worker skill"),
        "the Claude nudge must carry the inline slash-prefixed skill trigger; @TYPED@ held:\n{typed}"
    );
    assert!(
        typed.contains("working toward the goal below on a heartbeat"),
        "the delivered nudge must frame the whole goal brief; @TYPED@ held:\n{typed}"
    );
    assert!(
        !typed.contains("working ONE goal"),
        "the delivered nudge must not imply one atomic objective; @TYPED@ held:\n{typed}"
    );
    assert!(
        typed.contains("the whole goal is met"),
        "the delivered completion floor must cover the whole brief; @TYPED@ held:\n{typed}"
    );
    assert!(
        typed.contains("checkpoint.json"),
        "the delivered nudge must name the agent-owned checkpoint; @TYPED@ held:\n{typed}"
    );
    assert!(
        typed.contains("does not replace your final decision marker"),
        "checkpoint continuity must not replace the marker; @TYPED@ held:\n{typed}"
    );
    // E's spec: the full inlined machine schema is GONE from the nudge (it lives in the skill).
    assert!(
        !typed.contains("## Signal a decision point"),
        "the E nudge must NOT still inline the full WakeReport schema; @TYPED@ held:\n{typed}"
    );
}

/// L2-3 (ACC:E, real tmux): the non-negotiable "no messages for you" floor reaches a live nudged
/// pane on WHATEVER nudge branch runs.
///
/// One line can never be dropped on ANY nudge path: *it sends no messages on your behalf*. This
/// drives a real nudge to a live pane and asserts that rule reaches `@TYPED@`. It does NOT force
/// the write-failed / skill-absent degrade branch — it ignores the skill-landed bool, and in real
/// tmux the native install normally succeeds, so it exercises whatever branch happened to run.
/// The GENUINE skill-absent nudge (write-failed / codex) is unit-covered by `s_e5`. It belongs to
/// E because it pins the invariant EVERY branch must preserve — a worker must still be told it
/// owns its own comms and does not decide when the project is done.
///
/// (Gated on tmux PRESENCE at runtime, not on `#[ignore]`.)
#[test]
fn the_no_messages_floor_reaches_a_live_nudged_pane() {
    if !tmux_available() {
        eprintln!("skipping L2-3 floor test: tmux not available");
        return;
    }
    // The floor is UNCONDITIONAL on both branches, so it holds whether or not the skill installed;
    // a GENUINE skill-absent nudge (write-failed / codex) is unit-covered by `s_e5`. Ignore the
    // landing bool here.
    let (typed, _skill_landed) = typed_after_nudges(
        "pm-l2-degrade",
        "l2degrade",
        "prove the E degrade keeps the non-negotiable rule\n",
        1,
    );
    assert!(
        typed.contains("sends no messages on your behalf"),
        "the non-negotiable rule must survive every degrade path; @TYPED@ held:\n{typed}"
    );
    // ONE wake types the nudge EXACTLY once — over real tmux, from the bytes the pane received
    // (not a terminal render). This pins that the harness does not double-type a nudge: if a
    // driven pane ever SHOWS the floor block twice, it is a terminal reflow/scrollback artifact,
    // not a send-side bug. Guards the whole nudge path (compose + the single `send_keys`).
    assert_eq!(
        nudges_in(&typed),
        1,
        "one wake must type exactly ONE nudge:\n{typed}"
    );
    assert_eq!(
        typed.matches("sends no messages on your behalf").count(),
        1,
        "the floor must be typed exactly once per nudge (no double-type):\n{typed}"
    );
}
