//! Real-tmux acceptance for finite worker wakes and checkpoint reconciliation.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use agent_manager::clock::{Clock, SystemClock};
use agent_manager::job::{AgentLoopState, WakeState};
use agent_manager::job_engine::JobScheduler;
use agent_manager::registry::Engine;
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, TmuxDriver, session_name};

use crate::probe::{TmuxSocket, tmux_available, wait_for_pane_text};

struct ManualClock(AtomicI64);

impl ManualClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn finite_wake_stub(
    marker: &std::path::Path,
    checkpoint: &std::path::Path,
    output: &std::path::Path,
    phase: &std::path::Path,
) -> String {
    format!(
        r#"printf '> \n'
wake=0
while IFS= read -r line; do
  case "$line" in
    'You are a long-running agent'*)
      wake=$((wake + 1))
      printf '%s\n' "$wake" > '{phase}'
      if [ "$wake" -eq 1 ]; then
        started=$(date +%s)
        deadline=$(( started + 1 ))
        ( timeout 1 sh -c 'sleep 30'
          status=$?
          if [ "$status" -eq 124 ]; then
            printf deadline-enforced > '{output}'
          else
            printf 'unexpected-timeout-status:%s' "$status" > '{output}'
          fi
        ) &
        pid=$!
        start=$(awk '{{print $22}}' "/proc/$pid/stat")
        printf '{{"version":1,"seq":1,"in_progress":["bounded work"],"activities":[{{"id":"bounded-work","status":"running","handle":"pid:%s@start:%s","output_ref":"{output}","started_unix_s":%s,"deadline_unix_s":%s}}],"next":["reconcile bounded work"]}}' "$pid" "$start" "$started" "$deadline" > '{checkpoint}.tmp'
        mv '{checkpoint}.tmp' '{checkpoint}'
        printf '{{"seq":101,"state":"monitoring","status":"bounded work detached","next_step":"reconcile bounded work","next_check_s":60}}' > '{marker}.tmp'
        mv '{marker}.tmp' '{marker}'
      elif grep -q '^deadline-enforced$' '{output}' 2>/dev/null; then
        printf '{{"version":1,"seq":2,"done":["runaway stopped at hard deadline"],"next":["continue goal"]}}' > '{checkpoint}.tmp'
        mv '{checkpoint}.tmp' '{checkpoint}'
        printf '{{"seq":102,"state":"working","status":"reconciled detached result","next_step":"continue goal"}}' > '{marker}.tmp'
        mv '{marker}.tmp' '{marker}'
      fi
      ;;
  esac
  printf '> \n'
done
"#,
        marker = marker.display(),
        checkpoint = checkpoint.display(),
        output = output.display(),
        phase = phase.display(),
    )
}

fn drive_until(
    scheduler: &mut JobScheduler,
    driver: &TmuxDriver,
    clock: &ManualClock,
    timeout: Duration,
    mut ready: impl FnMut() -> bool,
) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        let _ = scheduler.tick(driver, clock);
        if ready() {
            return true;
        }
        clock.advance(1);
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn run_finite_wake_case(engine: Engine) {
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new(match engine {
        Engine::Claude => "pm-finite-claude",
        Engine::Codex => "pm-finite-codex",
    });
    let driver = TmuxDriver::with_socket(socket.name());
    let id = match engine {
        Engine::Claude => "finite-claude",
        Engine::Codex => "finite-codex",
    };
    let root = dir.path().to_path_buf();
    let paths = ProjectPaths::for_session(&root, id);
    let session = session_name(id, &root);
    let output = root.join("bounded.out");
    let phase = root.join("wake.phase");
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
    state::write_text_atomic(&paths.brief(), "complete bounded background work").unwrap();
    let now = SystemClock.now();
    let mut ledger = AgentLoopState::fresh(engine, Some(1), now);
    ledger.conversation_id = Some(format!("{id}-conversation"));
    agent_manager::job::save(&paths, &ledger).unwrap();

    let stub = finite_wake_stub(&paths.needs_you(), &paths.checkpoint(), &output, &phase);
    let launched = driver.launch_interactive(
        &session,
        &root,
        &["sh".into(), "-c".into(), stub],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let prompt_drawn = launched.is_ok() && wait_for_pane_text(&driver, &session, ">");
    let clock = ManualClock::new(now);
    let mut scheduler = JobScheduler::new(id, root.clone(), id, engine, None);

    let monitoring_disposed = prompt_drawn
        && drive_until(
            &mut scheduler,
            &driver,
            &clock,
            Duration::from_secs(20),
            || {
                agent_manager::job::load(&paths)
                    .ok()
                    .flatten()
                    .and_then(|state| state.situation)
                    .is_some_and(|situation| situation.state == WakeState::Monitoring)
            },
        );
    let first_checkpoint = state::read_checkpoint(&paths.checkpoint()).ok().flatten();
    let output_contents =
        crate::probe::wait_for_file_contents(&output, "deadline-enforced", Duration::from_secs(6));
    let output_finished = output_contents.trim() == "deadline-enforced";

    clock.advance(61);
    let reconciled = output_finished
        && drive_until(
            &mut scheduler,
            &driver,
            &clock,
            Duration::from_secs(20),
            || {
                agent_manager::job::load(&paths)
                    .ok()
                    .flatten()
                    .is_some_and(|state| {
                        state.last_marker_seq == 102
                            && state
                                .situation
                                .as_ref()
                                .is_some_and(|s| s.state == WakeState::Working)
                    })
            },
        );
    let final_checkpoint = state::read_checkpoint(&paths.checkpoint()).ok().flatten();
    let phase_text = std::fs::read_to_string(&phase).unwrap_or_default();
    let skill_landed = paths.native_worker_skill_file(engine).exists();

    let _ = driver.terminate(&session);
    drop(socket);

    launched.expect("launch the finite-wake stub");
    assert!(prompt_drawn, "{engine:?}: stub did not become idle");
    assert!(
        monitoring_disposed,
        "{engine:?}: monitoring marker was not disposed"
    );
    let first_checkpoint = first_checkpoint.expect("first checkpoint should be readable");
    assert_eq!(first_checkpoint.activities.len(), 1);
    assert!(first_checkpoint.activities[0].deadline_unix_s.is_some());
    assert!(
        output_finished,
        "{engine:?}: hard timeout was not enforced; output={output_contents:?}"
    );
    assert!(reconciled, "{engine:?}: next wake did not reconcile");
    let final_checkpoint = final_checkpoint.expect("final checkpoint should be readable");
    assert!(final_checkpoint.activities.is_empty());
    assert_eq!(
        final_checkpoint.done,
        ["runaway stopped at hard deadline".to_string()]
    );
    assert_eq!(phase_text.trim(), "2", "{engine:?}: expected two wakes");
    assert!(
        skill_landed,
        "{engine:?}: native worker skill was not installed"
    );
}

#[test]
#[ignore]
fn finite_wakes_reconcile_bounded_work_for_claude_and_codex() {
    if !tmux_available() {
        eprintln!("skipping finite-wake test: tmux not available");
        return;
    }
    for engine in [Engine::Claude, Engine::Codex] {
        run_finite_wake_case(engine);
    }
}
