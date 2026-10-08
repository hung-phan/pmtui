use std::path::Path;

use crate::registry::Engine;
use crate::worker::build_fork_command;

#[test]
fn claude_fork_resumes_the_source_and_requests_session_start_identity() {
    let argv = build_fork_command(
        Engine::Claude,
        "source-session",
        Some(Path::new("/tmp/turn-complete")),
        Some(Path::new("/tmp/fork-id")),
        Some("sonnet"),
    );

    assert_eq!(&argv[..4], ["env", "-u", "CLAUDECODE", "claude"]);
    assert!(argv.windows(2).any(|pair| pair == ["--model", "sonnet"]));
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--resume", "source-session"])
    );
    assert!(argv.iter().any(|arg| arg == "--fork-session"));
    let settings = argv
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .map(|pair| &pair[1])
        .expect("fork settings");
    assert!(settings.contains("SessionStart") && settings.contains("CLAUDE_CODE_SESSION_ID"));
    assert!(!argv.iter().any(|arg| arg == "--session-id"));
    assert!(!argv.iter().any(|arg| arg == "-p"));
}

#[test]
fn codex_fork_keeps_global_options_before_the_subcommand() {
    let argv = build_fork_command(
        Engine::Codex,
        "11111111-2222-4333-8444-555555555555",
        Some(Path::new("/tmp/turn-complete")),
        None,
        Some("gpt-5.4"),
    );

    let fork = argv.iter().position(|arg| arg == "fork").expect("fork arg");
    assert_eq!(argv[0], "codex");
    assert_eq!(argv[fork + 1], "11111111-2222-4333-8444-555555555555");
    assert!(
        argv[..fork]
            .windows(2)
            .any(|pair| pair == ["-m", "gpt-5.4"])
    );
    assert!(argv[..fork].windows(2).any(|pair| pair[0] == "-c"));
    assert!(!argv.iter().any(|arg| arg == "resume"));
    assert!(!argv.iter().any(|arg| arg == "--session-id"));
}
