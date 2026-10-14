//! Opt-in wire check: real TraeX must report a new fork identity through the
//! launcher's hooks. Hook trust stays with the person; this test never answers it.

use std::time::{Duration, Instant};

use agent_manager::lease;
use agent_manager::registry::Engine;
use agent_manager::state::{ProjectPaths, codex_identity};
use agent_manager::tmux::{
    Driver, ManagedEnv, PaneActivity, TmuxDriver, classify_dialog, classify_pane,
};
use agent_manager::worker::{Resume, build_fork_command, build_standard_command};

use crate::probe::{TmuxSocket, tmux_available};

fn wait_for(driver: &TmuxDriver, session: &str, description: &str, ready: impl Fn() -> bool) {
    let start = Instant::now();
    loop {
        let pane = driver.capture_tail(session, 60).unwrap_or_default();
        assert!(
            classify_dialog(&pane).is_none(),
            "{description}: engine needs human input; review its hooks in an ordinary TRAE session first.\n{pane}"
        );
        if ready() {
            eprintln!("{description}:\n{pane}");
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(180),
            "{description}: timed out.\n{pane}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "requires authenticated TraeX and human-trusted hooks"]
fn trae_identity_real_fork_reports_a_new_thread_without_a_completed_turn() {
    if std::env::var_os("PM_TRAE_IDENTITY").is_none() {
        eprintln!("skipping real-TRAE identity gate: set PM_TRAE_IDENTITY=1");
        return;
    }
    assert!(tmux_available(), "tmux is required");
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-trae-identity");
    let driver = TmuxDriver::with_socket(socket.name());
    let source = ProjectPaths::for_session(dir.path(), "source");
    let child = ProjectPaths::for_session(dir.path(), "child");
    let managed = |id: &str, paths: &ProjectPaths| {
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        ManagedEnv {
            session_id: id.into(),
            state_dir: paths.state_dir(),
            pmtui_bin: Some(env!("CARGO_BIN_EXE_pmtui").into()),
        }
    };
    driver
        .launch_interactive(
            "source",
            dir.path(),
            &build_standard_command(
                Engine::Trae,
                &Resume::Fresh { session_id: None },
                Some(&source.turn_signal()),
                None,
            ),
            &managed("source", &source),
        )
        .unwrap();
    wait_for(&driver, "source", "source startup", || {
        codex_identity::read(&source).is_some()
            && driver
                .capture_tail("source", 60)
                .is_ok_and(|pane| classify_pane(&pane) == PaneActivity::Idle)
    });
    assert!(!source.turn_signal().exists());
    {
        let _input = lease::try_acquire(&source.input_lock()).unwrap().unwrap();
        driver
            .send_keys("source", "Reply with the single word ok.")
            .unwrap();
    }
    wait_for(&driver, "source", "source completed turn", || {
        std::fs::metadata(source.turn_signal()).is_ok_and(|meta| meta.len() > 0)
    });
    let source_id = codex_identity::read(&source).unwrap();
    let source_turns = std::fs::read(source.turn_signal()).unwrap();
    driver
        .launch_interactive(
            "child",
            dir.path(),
            &build_fork_command(
                Engine::Trae,
                &source_id,
                Some(&child.turn_signal()),
                None,
                None,
            ),
            &managed("child", &child),
        )
        .unwrap();
    wait_for(&driver, "child", "fork startup", || {
        codex_identity::read(&child).is_some()
    });
    assert_ne!(codex_identity::read(&child).unwrap(), source_id);
    assert!(!child.turn_signal().exists());
    assert_eq!(std::fs::read(source.turn_signal()).unwrap(), source_turns);
    assert_eq!(codex_identity::read(&source).unwrap(), source_id);
    assert!(driver.is_alive("source").unwrap());
}
