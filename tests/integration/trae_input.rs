use std::path::PathBuf;
use std::time::Duration;

use agent_manager::registry::{Engine, Registry};
use agent_manager::tmux::{
    Driver, ManagedEnv, PaneActivity, TmuxDriver, classify_dialog, classify_pane, has_composer,
    session_name,
};

use crate::keystrokes::{send_key, send_literal};
use crate::probe::{
    TmuxSocket, tmux_available, wait_for_file_contents, wait_for_newest_status, wait_for_pane_text,
    wait_until,
};
use crate::seed::seed_standard_loop_session;

const COMPOSER: &str = concat!(
    "\u{2500}\u{2500}\u{2500}\u{2500}\n",
    "\u{276f} Explain this codebase\n",
    "\u{2500}\u{2500}\u{2500}\u{2500}\n",
    "  ? for shortcuts\n",
);

struct Fixture {
    host: TmuxDriver,
    agent: TmuxDriver,
    host_socket: TmuxSocket,
    _agent_socket: TmuxSocket,
    session: String,
    registry: PathBuf,
    received: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new(surface: &str) -> Self {
        assert!(tmux_available(), "this acceptance test requires tmux");
        let dir = tempfile::tempdir().unwrap();
        let host_socket = TmuxSocket::new("am-trae-input-host");
        let agent_socket = TmuxSocket::new("am-trae-input-agent");
        let host = TmuxDriver::with_socket(host_socket.name());
        let agent = TmuxDriver::with_socket(agent_socket.name());
        let registry = dir.path().join("registry.json");
        let received = dir.path().join("received.txt");
        let id = "trae-input";
        seed_standard_loop_session(&registry, dir.path(), id);
        let mut rows = Registry::load(&registry).unwrap();
        rows.projects[0].engine = Some(Engine::Trae);
        rows.save(&registry).unwrap();
        let session = session_name(id, dir.path());
        let screen = dir.path().join("screen.txt");
        std::fs::write(&screen, surface).unwrap();
        // A process receipt proves Enter submitted bytes; terminal echo alone cannot.
        agent
            .launch_interactive(
                &session,
                dir.path(),
                &[
                    "sh".into(),
                    "-c".into(),
                    "stty -echo; cat \"$1\"; while IFS= read -r line; do printf '%s\\n' \"$line\" >> \"$2\"; printf 'RECEIVED %s\\n' \"$line\"; done".into(),
                    "trae-input".into(),
                    screen.display().to_string(),
                    received.display().to_string(),
                ],
                &ManagedEnv::default(),
            )
            .unwrap();
        assert!(wait_for_pane_text(
            &agent,
            &session,
            "Explain this codebase"
        ));
        // No sibling pmd means this test cannot start a daemon or a real worker.
        let binary = dir.path().join("pmtui");
        std::fs::copy(env!("CARGO_BIN_EXE_pmtui"), &binary).unwrap();
        host.launch_interactive(
            "dashboard",
            dir.path(),
            &[
                "env".into(),
                "-u".into(),
                "PMTUI_SESSION".into(),
                "-u".into(),
                "PMTUI_STATE_DIR".into(),
                "-u".into(),
                "PMTUI_BIN".into(),
                binary.display().to_string(),
                "--socket".into(),
                agent_socket.name().into(),
                "--registry".into(),
                registry.display().to_string(),
            ],
            &ManagedEnv::default(),
        )
        .unwrap();
        assert!(wait_for_pane_text(&host, "dashboard", id));
        Self {
            host,
            agent,
            host_socket,
            _agent_socket: agent_socket,
            session,
            registry,
            received,
            _dir: dir,
        }
    }

    fn submit(&self, text: &str) {
        assert!(send_literal(self.host_socket.name(), "dashboard", "s"));
        assert!(wait_for_pane_text(&self.host, "dashboard", "Send"));
        eprintln!(
            "composer:\n{}",
            self.host.capture_tail("dashboard", 60).unwrap()
        );
        assert!(send_literal(self.host_socket.name(), "dashboard", text));
        assert!(send_key(self.host_socket.name(), "dashboard", "Enter"));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = send_key(self.host_socket.name(), "dashboard", "Escape");
        let _ = send_literal(self.host_socket.name(), "dashboard", "q");
        wait_until(Duration::from_secs(3), || {
            !self.host.is_alive("dashboard").unwrap_or(false)
        });
    }
}

#[test]
#[ignore = "requires real tmux"]
fn trae_input_dashboard_submits_once_at_idle_and_busy_composers() {
    for (prefix, expected) in [
        ("Finished response\n", PaneActivity::Idle),
        ("Working (esc to interrupt)\n", PaneActivity::Busy),
    ] {
        let fx = Fixture::new(&format!("{prefix}{COMPOSER}"));
        let before = fx.agent.capture_tail(&fx.session, 60).unwrap();
        eprintln!("agent before:\n{before}");
        assert!(has_composer(&before));
        assert_eq!(classify_pane(&before), expected);
        fx.submit("TRAE_MESSAGE_ACCEPTED");
        assert!(wait_for_newest_status(&fx.registry, "sent"));
        let receipt = wait_for_file_contents(
            &fx.received,
            "TRAE_MESSAGE_ACCEPTED",
            Duration::from_secs(5),
        );
        // An extra Enter can confirm a later dialog, so absence needs an observation window.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(receipt, "TRAE_MESSAGE_ACCEPTED\n");
        assert_eq!(std::fs::read_to_string(&fx.received).unwrap(), receipt);
        let after = fx.agent.capture_tail(&fx.session, 60).unwrap();
        eprintln!("agent after:\n{after}");
        assert!(after.contains("RECEIVED TRAE_MESSAGE_ACCEPTED"));
    }
}

#[test]
#[ignore = "requires real tmux"]
fn trae_input_dashboard_refuses_dialogs_even_with_a_visible_composer() {
    for dialog in [
        "Hooks need review\n1 hook is new or changed.\nHooks can run outside the sandbox after you trust them.\n\u{276f} 1. Review hooks\n  2. Trust all and continue\n  3. Continue without trusting (hooks won't run)\nenter confirm | esc go back\n",
        "Sign in to use TRAE CLI\n> 1. ByteDance Employee\n  2. Enterprise User\nPress Enter to continue\n",
        "Do you want to create hello.txt?\n\u{276f} 1. Yes\n  2. No\nEsc to cancel\n",
        "Which path?\n\u{276f} 1. First\n  2. Second\nEnter to select | Esc to cancel\n",
    ] {
        let fx = Fixture::new(&format!("{COMPOSER}{dialog}"));
        let before = fx.agent.capture_tail(&fx.session, 60).unwrap();
        eprintln!("blocked agent:\n{before}");
        assert!(classify_dialog(&before).is_some());
        assert!(!has_composer(&before));
        assert_eq!(classify_pane(&before), PaneActivity::Busy);
        fx.submit("MUST_NOT_REACH_DIALOG");
        assert!(wait_for_newest_status(&fx.registry, "dialog up"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(!fx.received.exists(), "a dialog received input");
        let after = fx.agent.capture_tail(&fx.session, 60).unwrap();
        eprintln!("blocked agent after:\n{after}");
        assert_eq!(after, before);
    }
}
