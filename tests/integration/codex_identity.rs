//! THE GATE: a real codex turn must actually write the conversation id.
//!
//! Everything else about this feature is checked against a fixture, and a fixture is exactly what
//! made the first version of it ship broken. The hook was written to match `"thread_id"` — the
//! spelling of the INTERNAL `HookEventAfterAgent` — while the legacy notify payload a `notify`
//! program really receives is re-serialized as `UserNotification` with `rename_all = "kebab-case"`,
//! so the wire key is `"thread-id"`. Unit tests built from the wrong struct agreed with the bug.
//!
//! Only a live engine settles that. This test launches the argv the LAUNCHER builds — never a
//! hand-copy of it — drives one turn, and reads the file.
//!
//! Opt-in, because it spends real tokens: skips cleanly unless `PM_CODEX_IDENTITY` is set AND a
//! real `codex` is on `PATH` AND tmux is available.
//!
//! Run: `ECC_GATEGUARD=off PM_CODEX_IDENTITY=1 cargo test --test integration codex_identity \
//! -- --ignored --nocapture`

use agent_manager::registry::Engine;
use agent_manager::state::{ProjectPaths, codex_identity};
use agent_manager::worker::{Resume, build_standard_command};
use std::time::{Duration, Instant};

const SOCKET: &str = "am-codex-identity";
const SESSION: &str = "pm-codex-identity-probe";
/// Codex has to start, trust the directory, take a turn and run the hook. Generous because the
/// failure this test exists to catch is "the file never appears", and a short timeout would report
/// that same symptom for a merely slow model.
const DEADLINE: Duration = Duration::from_secs(180);

fn enabled() -> bool {
    std::env::var("PM_CODEX_IDENTITY").is_ok() && which("codex") && which("tmux")
}

fn which(bin: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// POSIX single-quoting, local because the crate's own `shq` is private and widening production
/// visibility for a test is the wrong trade. This is a hand-written copy of SHELL QUOTING, not of
/// the launcher's argv — the argv, which is the thing a hand-copy would falsify, still comes from
/// `build_standard_command`. A quoting mistake here cannot pass silently either: codex would fail
/// to start and the assertions below would say so.
fn shq(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn tmux(args: &[&str]) -> std::process::Output {
    std::process::Command::new("tmux")
        .args(["-L", SOCKET])
        .args(args)
        .output()
        .expect("run tmux")
}

#[test]
#[ignore]
fn a_real_codex_turn_writes_its_conversation_id() {
    if !enabled() {
        eprintln!(
            "skipping real-codex identity gate: set PM_CODEX_IDENTITY=1 with codex and tmux on PATH"
        );
        return;
    }
    let dir = tempfile::tempdir().expect("scratch project");
    let root = dir.path();
    let paths = ProjectPaths::for_session(root, "probe");
    std::fs::create_dir_all(paths.state_dir()).expect("state dir");

    // THE LAUNCHER'S OWN ARGV. Rebuilding the `-c notify=…` override here by hand would repeat the
    // very mistake this test exists to catch: it would encode what I believe the launcher emits.
    let argv = build_standard_command(
        Engine::Codex,
        &Resume::Fresh { session_id: None },
        Some(&paths.turn_signal()),
        None,
    );
    let quoted: Vec<String> = argv.iter().map(|a| shq(a)).collect();

    let _ = tmux(&["kill-session", "-t", SESSION]);
    let launch = tmux(&[
        "new-session",
        "-d",
        "-s",
        SESSION,
        "-x",
        "120",
        "-y",
        "40",
        "-c",
        &root.to_string_lossy(),
        &quoted.join(" "),
    ]);
    assert!(
        launch.status.success(),
        "launch codex: {}",
        String::from_utf8_lossy(&launch.stderr)
    );

    // One trivial turn. What matters is that a turn ENDS, not what it says, so the prompt needs no
    // tools, no approvals and almost no tokens.
    std::thread::sleep(Duration::from_secs(12));
    let _ = tmux(&[
        "send-keys",
        "-t",
        SESSION,
        "-l",
        "--",
        "reply with the single word ok",
    ]);
    std::thread::sleep(Duration::from_millis(600));
    let _ = tmux(&["send-keys", "-t", SESSION, "Enter"]);

    let start = Instant::now();
    let mut captured = None;
    while start.elapsed() < DEADLINE {
        if let Some(id) = codex_identity::read(&paths) {
            captured = Some(id);
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let pane =
        String::from_utf8_lossy(&tmux(&["capture-pane", "-p", "-t", SESSION, "-S", "-60"]).stdout)
            .to_string();
    let turns = std::fs::metadata(paths.turn_signal()).map(|m| m.len()).ok();
    let _ = tmux(&["kill-session", "-t", SESSION]);
    let _ = tmux(&["kill-server"]);

    let id = captured.unwrap_or_else(|| {
        panic!(
            "codex ran but wrote no conversation id.\n\
             turn-complete bytes: {turns:?}\n\
             Check the payload's key spelling against codex's legacy notify serialization.\n\
             pane:\n{pane}"
        )
    });
    // Re-validated through the same reader pmd and pmtui use, so "the hook wrote something" and
    // "the readers accept it" cannot diverge.
    assert_eq!(
        codex_identity::parse(&id),
        Some(id.as_str()),
        "the captured id must satisfy the contract its readers enforce: {id:?}"
    );
    assert!(
        turns.unwrap_or(0) >= 1,
        "the same hook must still count the turn: {turns:?}"
    );
    eprintln!("real codex turn captured conversation id {id}");
}
