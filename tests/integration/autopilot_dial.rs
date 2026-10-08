//! ACCEPTANCE for the keys that START and STOP the driving — `m` (the autopilot
//! dial), `p` (pause) and `r` (restart) — every one of them measured by COUNTING the
//! nudges that actually land in the agent's pane. One module because each test's
//! negative ("no further nudge") is only non-vacuous next to a cadence proven to
//! repeat, and that proof is the same fixture in all three.

use std::process::Command;
use std::time::Duration;

use agent_manager::registry::{Engine, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, session_name};

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{PmdSibling, enter_fixture, turn_autopilot_on};
use crate::probe::{
    pid_alive, probe_pane_pid, sessions_column, tmux_available, wait_for_pane_text_within,
    wait_until,
};
use crate::seed::seed_standard_loop_session_at;
use crate::stubs::{
    nudge_count, nudge_counting_claude_stub, nudge_counting_claude_with_turn_hook_stub,
};

// ---------------------------------------------------------------------------
// "I switch autopilot off with t and it STILL runs and sends prompts" (m15)
//
// Reported verbatim. `config.autonomy` was read in the whole engine at exactly ONE
// production line (`policy::decide_kind`), so the tier only decided whether a stop the
// worker REPORTED got auto-approved or escalated. The heartbeat was tier-blind, so `m`
// could START the driving (`cycle_tier` -> `ensure_daemon`) and could never stop it.
//
// No `FakeDriver` unit test can see the whole of this: "no further keystroke reaches a
// live claude after a config write, and the claude is still alive" is composed of a real
// tmux pane, a real 500ms pmd sweep, a real cadence timer and real file ordering. So this
// drives a real `pmtui` with real keystrokes over a real `pmd` and COUNTS the nudges that
// actually land in the agent's pane.
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn r_restart_standard_codex_resumes_the_exact_live_user_rollout() {
    if !tmux_available() {
        eprintln!("skipping Codex restart test: tmux not available");
        return;
    }
    let fx = enter_fixture("cxr", PmdSibling::Missing);
    let (sock, host) = (fx.host_socket.clone(), fx.host_session.clone());
    let launches = fx.dir.path().join("codex-launches.log");
    let session_dir = fx.dir.path().join("codex/sessions/2026/08/25");
    std::fs::create_dir_all(&session_dir).unwrap();
    let user_id = "11111111-2222-4333-8444-555555555555";
    let subagent_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let user_rollout = session_dir.join(format!("rollout-old-{user_id}.jsonl"));
    let subagent_rollout = session_dir.join(format!("rollout-new-{subagent_id}.jsonl"));
    for (path, id, session_id, source) in [
        (&user_rollout, user_id, user_id, "user"),
        (&subagent_rollout, subagent_id, user_id, "subagent"),
    ] {
        let meta = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": id,
                "session_id": session_id,
                "cwd": fx.proj,
                "thread_source": source
            }
        });
        std::fs::write(path, format!("{meta}\n")).unwrap();
    }
    let codex = fx.dir.path().join("bin/codex");
    std::fs::write(
        &codex,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> '{}'\n\
             exec 8< '{}'\n\
             exec 9< '{}'\n\
             printf 'THREAD original\\n'\n\
             while IFS= read -r line; do :; done\n",
            launches.display(),
            user_rollout.display(),
            subagent_rollout.display(),
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "bot", 300);
    let mut registry = Registry::load(&fx.reg_path).unwrap();
    registry.projects[0].engine = Some(Engine::Codex);
    registry.save(&fx.reg_path).unwrap();
    let session = session_name("bot", &fx.proj);
    let initial = fx.agent.launch_interactive(
        &session,
        &fx.proj,
        &[codex.to_string_lossy().into_owned()],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let row_up =
        fx.up && wait_for_pane_text_within(&fx.host, &host, "bot", Duration::from_secs(15));
    let thread_up = initial.is_ok()
        && wait_for_pane_text_within(
            &fx.agent,
            &session,
            "THREAD original",
            Duration::from_secs(10),
        );
    let old_pid = probe_pane_pid(&fx.agent_socket, &session);

    let restarted = row_up
        && thread_up
        && send_literal(&sock, &host, "r")
        && wait_for_pane_text_within(&fx.host, &host, "Confirm restart", Duration::from_secs(10))
        && send_literal(&sock, &host, "y")
        && wait_for_pane_text_within(&fx.host, &host, "restarted", Duration::from_secs(15));
    let old_died = restarted
        && old_pid
            .as_deref()
            .is_some_and(|pid| wait_until(Duration::from_secs(10), || !pid_alive(pid)));
    let back_up = restarted
        && wait_until(Duration::from_secs(20), || {
            fx.agent.is_alive(&session).unwrap_or(false)
        });
    let new_pid = back_up
        .then(|| probe_pane_pid(&fx.agent_socket, &session))
        .flatten();
    let launch_log = std::fs::read_to_string(&launches).unwrap_or_default();
    let replacement = launch_log.lines().find(|line| {
        let mut args = line.split_whitespace().rev();
        args.next() == Some(user_id) && args.next() == Some("resume")
    });
    let exact_resume = replacement.is_some();
    let turn_hooked = replacement.is_some_and(|line| line.contains("notify="));
    let registry_id = Registry::load(&fx.reg_path)
        .ok()
        .and_then(|registry| registry.projects.into_iter().next())
        .and_then(|entry| entry.conversation_id);
    let token_restored = back_up
        && wait_for_pane_text_within(
            &fx.agent,
            &session,
            "THREAD original",
            Duration::from_secs(10),
        );
    let pane = fx.host.capture_tail(&host, 120).unwrap_or_default();
    let leaked = fx.teardown();

    initial.expect("launch initial Codex stub");
    assert!(row_up && thread_up, "precondition failed:\n{pane}");
    assert!(restarted, "`r` then `y` should restart Codex:\n{pane}");
    assert!(old_died, "old Codex process survived: {old_pid:?}");
    assert!(
        back_up && token_restored,
        "resumed Codex did not come back:\n{pane}"
    );
    assert_ne!(new_pid, old_pid, "restart must replace the process");
    assert!(exact_resume, "replacement argv never resumed {user_id}");
    assert!(
        turn_hooked,
        "replacement argv omitted the turn-completion hook"
    );
    assert_eq!(registry_id.as_deref(), Some(user_id));
    assert!(leaked.is_empty(), "leaked pmd: {leaked:?}");
}

/// ACCEPTANCE: `r` really ENDS the agent's process — pane, chat REPL and all.
///
/// User: *"the restart needs to stop the claude session too, i see it still running."*
///
/// `restart_agent` calls `tmux kill-session` on the `pmloop-` name and nothing else, which leaves
/// two ways for a claude to survive a "restart":
///   1. the `pmchat-` REPL, which is a SECOND live agent on the same conversation — and which parks
///      pmd's poll while it lives, so the freshly restarted session would sit there doing nothing;
///   2. the pane's own process, if `kill-session` does not take its children down.
///
/// Only a real tmux server and real processes can tell those apart, so this measures the PIDs: it
/// records the pane's process before the restart and asserts it is gone afterwards, then asserts
/// the chat session is gone too, then that pmd brings a NEW agent up (a restart that only kills is
/// a `p`).
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// r_restart_ends_the_agent_and_its_chat`.
#[test]
#[ignore]
fn r_restart_replaces_the_one_terminal_process() {
    if !tmux_available() {
        eprintln!("skipping restart test: tmux not available");
        return;
    }
    let fx = enter_fixture("rst", PmdSibling::DelayedReal);
    let (sock, sess) = (fx.host_socket.clone(), fx.host_session.clone());
    let counts = fx.dir.path().join("nudges.log");
    // The stub reports into the DRIVEN session's marker, so the awaiting-report gate opens between
    // nudges exactly as it does for a real agent.
    let marker = ProjectPaths::for_session(&fx.proj, "bot").needs_you();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(
        fx.dir.path().join("bin/claude"),
        nudge_counting_claude_stub(&counts, &marker),
    )
    .unwrap();
    const CADENCE_S: u64 = 5;
    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "bot", CADENCE_S);
    let loop_s = session_name("bot", &fx.proj);

    // Drive it for real, so there is a live agent to restart.
    let row_up =
        fx.up && wait_for_pane_text_within(&fx.host, &sess, "bot", Duration::from_secs(15));
    let driving = row_up
        && turn_autopilot_on(&fx, &sock, &sess)
        && wait_for_pane_text_within(&fx.host, &sess, "bot → Autopilot", Duration::from_secs(15));
    let loop_up = driving
        && wait_until(Duration::from_secs(30), || {
            fx.agent.is_alive(&loop_s).unwrap_or(false)
        });
    let nudged = loop_up && wait_until(Duration::from_secs(45), || nudge_count(&counts) >= 1);

    // THE PANE'S PROCESS, before the restart. This is the thing the user can see running.
    let pane_pid = probe_pane_pid(&fx.agent_socket, &loop_s);
    // RESTART: `r` then `y` (it always confirms — it throws away the turn in flight).
    let confirmed = nudged
        && send_literal(&sock, &sess, "r")
        && wait_for_pane_text_within(&fx.host, &sess, "Confirm restart", Duration::from_secs(10))
        && send_literal(&sock, &sess, "y")
        && wait_for_pane_text_within(&fx.host, &sess, "restarting", Duration::from_secs(15));

    // The OLD process must be gone — not just the tmux session name.
    let pane_died = confirmed
        && pane_pid.is_some()
        && wait_until(Duration::from_secs(20), || {
            pane_pid.as_deref().is_some_and(|p| !pid_alive(p))
        });
    // …and a NEW agent comes up, or this was a pause rather than a restart.
    let count_at_restart = nudge_count(&counts);
    let back_up = confirmed
        && wait_until(Duration::from_secs(45), || {
            fx.agent.is_alive(&loop_s).unwrap_or(false)
        });
    // `pane_pid` the LOCAL holds the pid from before the restart, so the fresh probe reaches the
    // free function under its unshadowed name.
    let new_pane_pid = if back_up {
        probe_pane_pid(&fx.agent_socket, &loop_s)
    } else {
        None
    };
    let nudging_again = back_up
        && wait_until(Duration::from_secs(60), || {
            nudge_count(&counts) > count_at_restart
        });
    let pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    assert!(fx.up, "pmtui should paint its dashboard:\n{pane}");
    assert!(driving, "`m` should turn autopilot ON:\n{pane}");
    assert!(loop_up, "autopilot ON must launch {loop_s}:\n{pane}");
    assert!(
        nudged,
        "…and drive it, so there is a live turn to throw away"
    );
    assert!(
        pane_pid.is_some(),
        "the fixture must find the agent's pane pid"
    );
    assert!(confirmed, "`r` then `y` should report a restart:\n{pane}");
    // THE USER'S REPORT.
    assert!(
        pane_died,
        "the agent's process {pane_pid:?} SURVIVED the restart — `kill-session` ended the tmux \
         session but not the claude inside it:\n{pane}"
    );
    assert!(back_up, "a restart must bring the agent back:\n{pane}");
    assert_ne!(
        new_pane_pid, pane_pid,
        "the agent must be a NEW process, not the old one still running"
    );
    assert!(
        nudging_again,
        "…and pmd must drive it again (still {count_at_restart} nudges):\n{pane}"
    );
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}

/// ACCEPTANCE: `p` really STOPS a session; `Enter` brings it back UNDRIVEN; `m` starts it again.
///
/// The user's spec, in two messages: *"Pause can stop the current session or autopilot, then kill
/// claude/codex so it is not running. When i press enter on pause session, it will resume for me."*
/// then *"when i press pause, i want to stop autopilot too, only when i press m again, then it start
/// again."* Almost none of that is visible to a unit test — `pause_session`'s pane kill is a no-op
/// against the test socket, and both "pmd does not relaunch it" and "pmd DOES relaunch it after `m`"
/// are statements about a live 500ms sweep racing file writes. The race is the bug this test exists
/// for: kill the pane BEFORE the row is disabled and pmd relaunches the agent within half a second,
/// leaving a row labelled "paused" next to a running claude.
///
/// Asserted in one run, over one real pmd, so no negative can be vacuous:
///   1. autopilot ON drives the row — the agent is up and nudges land, REPEATEDLY;
///   2. `p` KILLS the agent's session (the "not running" the user asked for) and turns the DIAL off;
///   3. it STAYS dead across several sweeps and cadences, and the nudge count does not move;
///   4. the row says so — the label reads `paused`;
///   5. `Enter` brings the SESSION back but NOT the driving: still no agent, still no nudges,
///      because the dial is off — this is the leg the first version of this test got wrong;
///   6. `m` starts it again, and one press is enough: pmd relaunches the agent and the nudges come
///      back, which also proves `m` LIFTED THE PAUSE rather than only writing a tier the sweep
///      never reads on a disabled row.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// p_pauses_a_session_and_m_starts_it_again`.
#[test]
#[ignore]
fn p_pauses_a_session_and_m_starts_it_again() {
    if !tmux_available() {
        eprintln!("skipping pause test: tmux not available");
        return;
    }
    let fx = enter_fixture("pse", PmdSibling::DelayedReal);
    let (sock, sess) = (fx.host_socket.clone(), fx.host_session.clone());
    let counts = fx.dir.path().join("nudges.log");
    // The stub reports into the DRIVEN session's marker, so the awaiting-report gate opens between
    // nudges exactly as it does for a real agent.
    let marker = ProjectPaths::for_session(&fx.proj, "bot").needs_you();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(
        fx.dir.path().join("bin/claude"),
        nudge_counting_claude_stub(&counts, &marker),
    )
    .unwrap();

    // Seeded on disk rather than through the create form, whose cadence floor is 60s — this
    // whole test is about what happens ACROSS cadences, so it needs a short one.
    const CADENCE_S: u64 = 5;
    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "bot", CADENCE_S);
    let loop_s = session_name("bot", &fx.proj);

    // (1) Drive it for real first. `m` flips to Autopilot and ensures the daemon.
    let row_up =
        fx.up && wait_for_pane_text_within(&fx.host, &sess, "bot", Duration::from_secs(15));
    let driving = row_up
        && turn_autopilot_on(&fx, &sock, &sess)
        && wait_for_pane_text_within(&fx.host, &sess, "bot → Autopilot", Duration::from_secs(15));
    let loop_up = driving
        && wait_until(Duration::from_secs(30), || {
            fx.agent.is_alive(&loop_s).unwrap_or(false)
        });
    // TWO nudges: without a repeating cadence, "the nudges stopped" below proves nothing.
    let nudged_twice = loop_up && wait_until(Duration::from_secs(60), || nudge_count(&counts) >= 2);

    // (2) PAUSE — "stop the current session or autopilot", both halves.
    let paused = nudged_twice
        && send_literal(&sock, &sess, "p")
        && wait_for_pane_text_within(&fx.host, &sess, "bot paused", Duration::from_secs(15));
    let cfg_path = ProjectPaths::for_session(&fx.proj, "bot").config();
    let tier_after_pause = state::read_json::<Config>(&cfg_path)
        .map(|c| c.autonomy)
        .ok();
    let killed = paused
        && wait_until(Duration::from_secs(15), || {
            !fx.agent.is_alive(&loop_s).unwrap_or(false)
        });

    // (3) …and it stays dead. Several pmd sweeps AND several cadences, which is the window the
    //     kill-then-disable ordering bug lives in.
    let count_at_pause = nudge_count(&counts);
    let waited_s = CADENCE_S * 4 + 8;
    std::thread::sleep(Duration::from_secs(waited_s));
    let relaunched = fx.agent.is_alive(&loop_s).unwrap_or(false);
    let count_after_pause = nudge_count(&counts);
    // (4) The row must SAY it is paused. Captured while paused, because resume clears it.
    //     This is the assertion that earned its keep: the first version of the marker was a
    //     trailing " (paused)" badge, which no human would ever have seen — at this pane's
    //     sessions-column width the row ends at the tier tag. It lives in the label field now.
    let paused_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let badged = sessions_column(&paused_pane).contains("paused");

    // (5) RESUME with Enter. The session comes back; the DRIVING must not. Waited out over several
    //     sweeps and a cadence, because "pmd did not do X" needs time to be a real claim.
    let resumed = paused
        && send_key(&sock, &sess, "Enter")
        && wait_for_pane_text_within(&fx.host, &sess, "bot resumed", Duration::from_secs(15));
    std::thread::sleep(Duration::from_secs(CADENCE_S * 2 + 5));
    let terminal_after_resume = fx.agent.is_alive(&loop_s).unwrap_or(false);
    let count_after_resume = nudge_count(&counts);
    let resumed_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();

    // (6) `m` — the one key that starts it again, and one press must be enough.
    let restarted = resumed
        && turn_autopilot_on(&fx, &sock, &sess)
        && wait_for_pane_text_within(&fx.host, &sess, "bot → Autopilot", Duration::from_secs(15));
    let back_up = restarted
        && wait_until(Duration::from_secs(45), || {
            fx.agent.is_alive(&loop_s).unwrap_or(false)
        });
    let nudging_again = back_up
        && wait_until(Duration::from_secs(60), || {
            nudge_count(&counts) > count_after_resume
        });
    let final_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let tier_after = state::read_json::<Config>(&cfg_path)
        .map(|c| c.autonomy)
        .ok();
    let pmd_left = fx.teardown();

    // ---- assertions (both servers and the daemon are already gone) ----
    assert!(fx.up, "pmtui should paint its dashboard:\n{paused_pane}");
    assert!(row_up, "the seeded row should appear:\n{paused_pane}");
    assert!(driving, "`m` should turn autopilot ON:\n{paused_pane}");
    assert!(loop_up, "autopilot ON must launch {loop_s}:\n{paused_pane}");
    assert!(
        nudged_twice,
        "the cadence must REPEAT before we pause it — otherwise the stopped-nudges assertion \
         below is vacuous (count was {count_at_pause})"
    );
    assert!(paused, "`p` should pause and say so:\n{paused_pane}");
    // THE USER'S WORDS: "kill claude/codex so it is not running".
    assert!(
        killed,
        "pause must KILL the agent — {loop_s} was still alive:\n{paused_pane}"
    );
    // THE RACE, in one assertion.
    assert!(
        !relaunched,
        "pmd RELAUNCHED the agent {waited_s}s after the pause — the row is disabled but \
         {loop_s} is running again, which is the kill-before-disable ordering bug"
    );
    assert_eq!(
        count_after_pause, count_at_pause,
        "a paused session must receive nothing: {count_at_pause} nudges at the pause, \
         {count_after_pause} after {waited_s}s"
    );
    assert!(
        badged,
        "the paused row must say so on screen — at THIS width, not just a wide one:\n{paused_pane}"
    );
    // *"when i press pause, i want to stop autopilot too"* — the dial, on disk.
    assert_eq!(
        tier_after_pause,
        Some(Tier::Standard),
        "pause must turn autopilot OFF:\n{paused_pane}"
    );
    assert!(resumed, "Enter should resume a paused row:\n{resumed_pane}");
    // Enter relaunches the Standard terminal for the human, but pmd must not drive it.
    assert!(
        terminal_after_resume,
        "Enter must relaunch the Standard terminal for the human:\n{resumed_pane}"
    );
    assert_eq!(
        count_after_resume, count_after_pause,
        "a resumed-but-undriven session must receive nothing: {count_after_pause} nudges at the \
         pause, {count_after_resume} after the resume:\n{resumed_pane}"
    );
    assert!(
        restarted,
        "`m` should turn autopilot back on:\n{final_pane}"
    );
    assert!(
        back_up,
        "`m` must start it again — pmd never relaunched {loop_s}, which is what happens if `m` \
         writes the tier without lifting the pause:\n{final_pane}"
    );
    assert!(
        nudging_again,
        "`m` must restore the DRIVING (still {count_after_resume} nudges):\n{final_pane}"
    );
    assert_eq!(
        tier_after,
        Some(Tier::Autopilot),
        "…and the dial the human just turned on must be what is on disk"
    );
    assert!(pmd_left.is_empty(), "leaked pmd: {pmd_left:?}");
}

/// ACCEPTANCE: `m` is a REAL off switch. With autopilot OFF, pmd stops typing into the
/// session — and does not kill it — and `m` again brings the nudges back, with no restart.
///
/// Everything is asserted against real substrate, in ONE run so the negatives cannot be
/// vacuous (the same `pmd`, in the same sweep loop, is demonstrably driving the Autopilot
/// row while it leaves the Standard one alone):
///   1. autopilot ON really nudges, REPEATEDLY (two nudges, so the cadence is proven live);
///   2. `m` OFF stops the nudges dead — the count does not move across several cadences;
///   3. `m` OFF does NOT kill the `pmloop-` session: the human keeps the agent and drives
///      it by hand (Enter attaches it), which is the whole point of "off";
///   4. `m` ON resumes nudging, with no pmd restart — so the gate is a live read of
///      `config.autonomy`, not a latch decided when the runner was built;
///
/// …plus a control row seeded on Standard that pmd must never launch AT ALL.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// autopilot_off_stops_the_nudges_without_killing_the_session`.
#[test]
#[ignore]
fn autopilot_off_stops_the_nudges_without_killing_the_session() {
    if !tmux_available() {
        eprintln!("skipping autopilot-off test: tmux not available");
        return;
    }
    let fx = enter_fixture("off", PmdSibling::DelayedReal);
    let (sock, sess) = (fx.host_socket.clone(), fx.host_session.clone());
    let counts = fx.dir.path().join("nudges.log");
    // The stub reports into the DRIVEN session's marker, so the awaiting-report gate opens between
    // nudges exactly as it does for a real agent.
    let marker = ProjectPaths::for_session(&fx.proj, "a-loop").needs_you();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(
        fx.dir.path().join("bin/claude"),
        nudge_counting_claude_stub(&counts, &marker),
    )
    .unwrap();

    // Two rows, seeded on disk (NOT through the create form, whose cadence floor is 60s).
    // `a-loop` sorts FIRST at every posture rank the driven row can reach, and `z-ctl`
    // stays Fresh (rank 2) because nothing ever drives it — so the selection, which starts
    // at 0 and is never moved, is always `a-loop`. Each `m` below also waits for a status
    // naming `a-loop`, so a mis-selection fails rather than silently retargets.
    const CADENCE_S: u64 = 5;
    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "a-loop", CADENCE_S);
    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "z-ctl", CADENCE_S);
    let loop_s = session_name("a-loop", &fx.proj);
    let ctl_s = session_name("z-ctl", &fx.proj);

    // (1) Both rows on screen, then `m` on `a-loop` = "let pmd drive this" = ensure a
    //     daemon. The fixture's pmd sleeps PMD_BOOT_DELAY_S before exec'ing the real one.
    let rows_up = fx.up
        && wait_for_pane_text_within(&fx.host, &sess, "a-loop", Duration::from_secs(15))
        && wait_for_pane_text_within(&fx.host, &sess, "z-ctl", Duration::from_secs(15));
    let turned_on = rows_up
        && turn_autopilot_on(&fx, &sock, &sess)
        && wait_for_pane_text_within(
            &fx.host,
            &sess,
            "a-loop → Autopilot",
            Duration::from_secs(15),
        );
    let loop_up = turned_on
        && wait_until(Duration::from_secs(30), || {
            fx.agent.is_alive(&loop_s).unwrap_or(false)
        });

    // (2) TWO nudges, so the repeating cadence is proven before we try to stop it.
    let nudged_once = loop_up && wait_until(Duration::from_secs(45), || nudge_count(&counts) >= 1);
    let nudged_twice =
        nudged_once && wait_until(Duration::from_secs(45), || nudge_count(&counts) >= 2);

    // (3) `m` again = autopilot OFF. Nothing else changes: same pmd, same registry, same
    //     tmux server — only the per-session `config.json`.
    let turned_off = nudged_twice
        && send_literal(&sock, &sess, "m")
        && wait_for_pane_text_within(
            &fx.host,
            &sess,
            "a-loop: autopilot off",
            Duration::from_secs(15),
        );
    // The marker: whatever has landed by now. Then wait out several cadences (plus the
    // confirmation gate's extra recheck) and see whether it moves.
    let count_at_off = nudge_count(&counts);
    let waited_s = CADENCE_S * 5 + 10;
    std::thread::sleep(Duration::from_secs(waited_s));
    let count_after_off = nudge_count(&counts);
    // The session must have SURVIVED being switched off.
    let alive_after_off = fx.agent.is_alive(&loop_s).unwrap_or(false);
    let off_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();

    // (4) `m` once more = back ON. "already running" (not "started") also proves no second
    //     daemon was spawned — the SAME pmd resumes driving.
    let turned_back_on = turned_off
        && turn_autopilot_on(&fx, &sock, &sess)
        && wait_for_pane_text_within(
            &fx.host,
            &sess,
            "a-loop → Autopilot; daemon already running",
            Duration::from_secs(15),
        );
    let resumed = turned_back_on
        && wait_until(Duration::from_secs(45), || {
            nudge_count(&counts) > count_after_off
        });

    // The CONTROL: a Standard row in the same registry, swept by the same live pmd for the
    // whole run, must never have been launched.
    let ctl_launched = fx.agent.is_alive(&ctl_s).unwrap_or(false);
    let ctl_ledger = agent_manager::job::load(&ProjectPaths::for_session(&fx.proj, "z-ctl"))
        .ok()
        .flatten();
    let final_pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    // ---- assertions (both servers and the daemon are already gone) ----
    assert!(
        fx.up,
        "pmtui should paint its dashboard; pane was:\n{off_pane}"
    );
    assert!(
        rows_up,
        "both seeded rows should appear; pane was:\n{off_pane}"
    );
    assert!(
        turned_on,
        "`m` should turn autopilot ON for a-loop; pane was:\n{off_pane}"
    );
    assert!(
        loop_up,
        "autopilot ON must launch {loop_s}; pane was:\n{off_pane}"
    );
    assert!(
        nudged_once,
        "autopilot ON must NUDGE the live session (nothing reached the pane at all)"
    );
    assert!(
        nudged_twice,
        "the cadence must REPEAT while autopilot is on — without a second nudge the \
         no-further-nudge assertion below would be vacuous (count was {count_at_off})"
    );
    assert!(
        turned_off,
        "`m` should turn autopilot OFF and say so; pane was:\n{off_pane}"
    );
    // THE BUG, in one assertion.
    assert_eq!(
        count_after_off, count_at_off,
        "autopilot is OFF and pmd STILL sent prompts to the agent: {count_at_off} nudges \
         at the flip, {count_after_off} after {waited_s}s of waiting. Pane was:\n{off_pane}"
    );
    assert!(
        alive_after_off,
        "turning autopilot off must NOT kill {loop_s} — the human keeps the agent and \
         drives it by hand"
    );
    assert!(
        turned_back_on,
        "`m` should turn autopilot back ON, reusing the SAME daemon; pane was:\n{final_pane}"
    );
    assert!(
        resumed,
        "flipping autopilot back on must resume nudging with no pmd restart (still \
         {count_after_off} nudges); pane was:\n{final_pane}"
    );
    assert!(
        !ctl_launched,
        "a fresh STANDARD row must never be launched by pmd: {ctl_s} exists"
    );
    assert_eq!(
        ctl_ledger.map(|l| l.run),
        Some(agent_manager::job::JobRun::Idle),
        "and its ledger must be untouched — pmd skips the whole tick, so it never even \
         parks a cadence it is not keeping"
    );
    assert!(
        pmd_left.is_empty(),
        "teardown must leave no pmd behind, still running: {pmd_left:?}"
    );
    drop(fx); // whole fixture: reaps pmd + both tmux servers, then the tempdir
}

// ---------------------------------------------------------------------------
// "I switch a standard to autopilot ... that never happens. next displays idle" (m44)
//
// Reported one commit after m43 shipped, and m43 CAUSED it: a Standard create now starts the agent
// in the row's own `pmchat-` REPL, and `chat_lock::is_active` defers pmd on a merely-ALIVE chat
// session ("ALIVE => defer, full stop. No age cap, no reap"). `JobScheduler::drive` returns from
// that gate before it touches `run`, so the ledger stayed `Idle` and the dial changed nothing.
//
// The existing tests could not see it: `p_pauses_a_session_and_m_starts_it_again` and its siblings
// SEED the session on disk, so no chat pane ever exists in them. This one launches the chat first,
// which is the state every real create leaves behind.
// ---------------------------------------------------------------------------

/// ACCEPTANCE: a live (detached) chat must not make `m` a dial that does nothing.
///
/// Measured the only way that cannot be vacuous — by COUNTING nudges that land in the agent's pane
/// after the flip, over a real pmd, with a real chat session alive beforehand.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored --test-threads=1 \
/// m_starts_autopilot_even_when_a_chat_holds_the_conversation`.
#[test]
#[ignore]
fn m_starts_autopilot_even_when_a_chat_holds_the_conversation() {
    if !tmux_available() {
        eprintln!("skipping chat-handover test: tmux not available");
        return;
    }
    let fx = enter_fixture("cho", PmdSibling::DelayedReal);
    let (sock, sess) = (fx.host_socket.clone(), fx.host_session.clone());
    let counts = fx.dir.path().join("nudges.log");
    let session_paths = ProjectPaths::for_session(&fx.proj, "bot");
    let marker = session_paths.needs_you();
    let turn_signal = session_paths.turn_signal();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(
        fx.dir.path().join("bin/claude"),
        nudge_counting_claude_with_turn_hook_stub(&counts, &marker, &turn_signal),
    )
    .unwrap();

    // Short cadence, because the whole assertion is "nudges arrive"; the create form floors at 60s.
    const CADENCE_S: u64 = 5;
    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "bot", CADENCE_S);
    let session = session_name("bot", &fx.proj);

    // THE PRECONDITION m43 introduced: a live, DETACHED chat holding this conversation. Any
    // long-running command will do — `chat_lock::is_active` reads liveness, not what is inside.
    let mut standard_argv = agent_manager::worker::build_standard_command(
        Engine::Claude,
        &agent_manager::worker::Resume::Continue("stub-session".into()),
        Some(&turn_signal),
        None,
    );
    let claude_at = standard_argv
        .iter()
        .position(|arg| arg == "claude")
        .expect("Standard Claude argv");
    standard_argv[claude_at] = fx.dir.path().join("bin/claude").display().to_string();
    let terminal_launched = fx
        .agent
        .launch_interactive(
            &session,
            &fx.proj,
            &standard_argv,
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .is_ok();
    let terminal_alive_first = terminal_launched
        && wait_until(Duration::from_secs(10), || {
            fx.agent.is_alive(&session).unwrap_or(false)
        });
    let pid_before = probe_pane_pid(&fx.agent_socket, &session);

    // THE FLIP.
    let row_up =
        fx.up && wait_for_pane_text_within(&fx.host, &sess, "bot", Duration::from_secs(15));
    let flipped = row_up
        && terminal_alive_first
        && turn_autopilot_on(&fx, &sock, &sess)
        && wait_for_pane_text_within(&fx.host, &sess, "bot → Autopilot", Duration::from_secs(15));

    let terminal_survived = flipped
        && wait_until(Duration::from_secs(15), || {
            fx.agent.is_alive(&session).unwrap_or(false)
        });
    let pid_after = probe_pane_pid(&fx.agent_socket, &session);
    let nudged =
        terminal_survived && wait_until(Duration::from_secs(60), || nudge_count(&counts) >= 1);
    let hook_fired = nudged
        && wait_until(Duration::from_secs(10), || {
            std::fs::metadata(&turn_signal)
                .map(|metadata| metadata.len() >= 1)
                .unwrap_or(false)
        });
    let completed_turns = std::fs::metadata(&turn_signal)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let tier = state::read_json::<Config>(&ProjectPaths::for_session(&fx.proj, "bot").config())
        .map(|c| c.autonomy)
        .ok();

    // Kill the server BEFORE asserting, so a failure cannot leak a running claude stub.
    let _ = Command::new("tmux")
        .args(["-L", &fx.agent_socket, "kill-server"])
        .status();
    let _ = Command::new("tmux")
        .args(["-L", &sock, "kill-server"])
        .status();

    assert!(
        terminal_launched && terminal_alive_first,
        "fixture: the terminal never came up"
    );
    assert!(row_up, "fixture: the dashboard never painted the row");
    assert!(flipped, "the m flip never reported Autopilot");
    assert_eq!(tier, Some(Tier::Autopilot), "the dial did not land on disk");
    assert!(
        terminal_survived,
        "the tier change killed the project terminal"
    );
    assert_eq!(pid_before, pid_after, "tier change restarted the process");
    assert!(nudged, "autopilot never drove the row");
    assert!(
        hook_fired && completed_turns >= 1,
        "the Standard-born pane's turn hook did not survive Autopilot adoption \
         (observed {completed_turns} completed turns)"
    );
}
