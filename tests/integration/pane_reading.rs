//! What the harness READS off a live pane, where only real tmux can establish the
//! shape that arrives: a vendor's startup dialog in whatever wording it ships today,
//! and the colour path — `capture-pane -e` re-serialises tmux's CELL GRID rather
//! than replaying the bytes `printf` wrote, so hand-written fixture bytes cannot
//! stand in for it.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier};

use agent_manager::ansi;
use agent_manager::tmux::{Driver, TmuxDriver, session_name};

use crate::probe::TmuxSocket;
use crate::probe::tmux_available;

/// Real-CLI: launch an ACTUAL `codex` in a directory it has never seen and prove the
/// harness recognises the STARTUP TRUST DIALOG it blocks on.
///
/// Why this one earns a live test when the unit fixture already covers the parsing: the
/// trust branch of `classify_dialog` is the only place in the codebase that matches a
/// vendor's PROSE rather than its chrome, so the fixture can only ever prove we parse
/// yesterday's wording. This proves the wording is still today's — and it does it at the
/// pane width the harness really uses, since it goes through the harness's own
/// `launch_interactive` (detached `new-session`, no `-x`, i.e. 80 columns), which is
/// where a reflowed question would split the anchor across two lines.
///
/// SAFETY. It DETECTS and never ANSWERS: a trust decision is exactly the class the
/// escalation floor exists for, and answering it would write
/// `[projects."<dir>"] trust_level = "trusted"` into the real `~/.codex/config.toml`. So
/// no key is ever sent to the pane — the asserts below include the proof, checking the
/// scratch path never appears in that file. Nor is any prompt submitted, so codex runs
/// nothing at all: it cannot touch the network, a repo, or anything outside the scratch
/// tempdir, because it never gets past the dialog. Private `-L` socket keyed on this
/// pid, `kill-server` before the assertions, verdicts captured into locals first.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored real_codex_trust_dialog_is_detected`.
#[test]
#[ignore]
fn real_codex_trust_dialog_is_detected_on_an_untrusted_directory() {
    use agent_manager::tmux::{PaneActivity, classify_dialog, classify_pane};

    if !tmux_available()
        || Command::new("codex")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
    {
        eprintln!("skipping real-codex trust-dialog test: tmux/codex unavailable");
        return;
    }
    // A FRESH tempdir is the whole premise: codex keys trust on the directory path, so a
    // path it has never seen is the only way to make the dialog appear at all.
    let dir = tempfile::tempdir().unwrap();
    // AND IT MUST BE A GIT REPOSITORY. codex 0.160 only asks about trust for a repo; in a plain
    // directory it skips the question and goes straight to its composer, which is what made this test
    // fail while the recogniser was perfectly fine. Measured both ways on the same binary: a bare
    // tempdir produced no prompt at all, and `git init` in the same place produced "Trust this folder?"
    // within 10 seconds.
    assert!(
        Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .map(|status| status.success())
            .unwrap_or(false),
        "the scratch dir must be a git repo or codex never asks about trust"
    );
    let socket = TmuxSocket::new("pm-codex-trust");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = session_name("trust", dir.path());
    let launched = driver
        .launch_interactive(
            &session,
            dir.path(),
            &["codex".into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .is_ok();

    // Poll for the dialog rather than sleeping a fixed time: codex paints its banner
    // first, and the pane is empty for the first few hundred ms.
    let mut dialog = None;
    let mut pane = String::new();
    let start = Instant::now();
    while launched && start.elapsed() < Duration::from_secs(30) {
        pane = driver.capture_tail(&session, 40).unwrap_or_default();
        dialog = classify_dialog(&pane);
        if dialog.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let activity = classify_pane(&pane);
    // Teardown BEFORE the assertions, so a failing assert cannot leak a codex process or
    // a tmux server. The dialog is still unanswered when codex dies with the pane.
    let _ = driver.terminate(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();
    // Read the real config AFTER teardown: answering the dialog is what would have added
    // the scratch path here, so its absence is the machine-checkable proof we only looked.
    let codex_config = std::env::var("HOME")
        .map(|h| Path::new(&h).join(".codex/config.toml"))
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let scratch = dir.path().display().to_string();

    // ---- assertions ----
    assert!(launched, "codex should launch in the scratch dir");
    let d = dialog.unwrap_or_else(|| {
        panic!(
            "the harness must recognise codex's startup trust dialog — if the pane below \
             shows it but this failed, codex has REWORDED the anchor in \
             `tmux::CODEX_TRUST_QUESTION`, and every first codex session in a new \
             directory is silently stalling again. Pane was:\n{pane}"
        )
    });
    assert!(
        {
            let question = d.question.to_ascii_lowercase();
            question.contains("do you trust the contents of this directory?")
                || question.contains("trust this folder?")
        },
        "the human must be shown the real question, got {:?}",
        d.question
    );
    assert!(
        d.options.len() >= 2,
        "the human must be shown the real choices, got {:?}",
        d.options
    );
    // The dialog pane reads Busy, which is why `drive` consults `classify_dialog` first.
    // Worth asserting on the LIVE pane specifically: the real screen carries a
    // `> You are in <dir>` row that the trimmed unit fixture drops, and it is the closest
    // thing on it to claude's bare `>` prompt. Busy here proves it does not read as one.
    assert_eq!(
        activity,
        PaneActivity::Busy,
        "a dialog draws no bare prompt and no busy marker; pane was:\n{pane}"
    );
    assert!(
        !codex_config.contains(&scratch),
        "the test must DETECT the trust dialog, never answer it — {scratch} must not have \
         been trusted in ~/.codex/config.toml"
    );
}

/// ACCEPTANCE (real substrate): the COLOUR path end to end. Paint a known bold +
/// indexed-colour string into a REAL tmux pane, read it back with `capture_tail_styled`
/// (`capture-pane -e`), parse it with `ansi::styled_lines`, and assert the spans carry the
/// colour and the attribute the pane was painted with.
///
/// Why it has to be real tmux: `-e` re-serialises tmux's CELL GRID rather than replaying
/// the bytes `printf` wrote, so only tmux can say what shape the parser actually receives
/// (attributes and colours may come back as separate escapes, in either order). A unit
/// test over hand-written bytes cannot establish that.
///
/// It also pins the SIBLING SPLIT that keeps this feature safe: plain `capture_tail` must
/// still come back escape-FREE, because `classify_pane`, `classify_dialog` and the stall
/// detector all match on its literal text.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored capture_tail_styled_round_trips_real_colour`.
#[test]
#[ignore]
fn capture_tail_styled_round_trips_real_colour() {
    if !tmux_available() {
        eprintln!("skipping styled-capture test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("pm-ansi");
    let driver = TmuxDriver::with_socket(socket.name());
    let session = "pmi-ansi";
    // Bold + indexed-153 fg closed by a full reset (the exact pair claude uses), then an
    // OSC 8 hyperlink around a filename (the shape that wraps filenames in claude's tool
    // rows). `\033` is POSIX `printf`, so this holds whatever `/bin/sh` is; `sleep` keeps
    // the pane — and so the session — alive while we poll. Raw strings keep the escapes
    // readable; `shq` re-quotes the whole thing for `sh -c`.
    let painted = concat!(
        r"printf '\033[1;38;5;153mANSI_MARKER_42\033[0m ",
        r"\033]8;;file:///p.txt\033\\LINK_TEXT_7\033]8;;\033\\\n'",
        "; sleep 300",
    );
    driver
        .launch_interactive(
            session,
            dir.path(),
            &["sh".into(), "-c".into(), painted.into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch");

    let mut styled = String::new();
    for _ in 0..50 {
        styled = driver.capture_tail_styled(session, 20).unwrap_or_default();
        if styled.contains("ANSI_MARKER_42") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // The escape-free sibling, read from the SAME live pane so the comparison is fair.
    let plain = driver.capture_tail(session, 20).unwrap_or_default();

    // Every result into a local, and the server dies BEFORE the first assertion.
    let lines = ansi::styled_lines(&styled);
    let marker = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content.contains("ANSI_MARKER_42"))
        .map(|s| (s.style.add_modifier, s.style.fg));
    let rendered: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    let styled_has_escapes = styled.contains('\u{1b}');
    let plain_has_escapes = plain.contains('\u{1b}');
    // The regression guard that matters most: the human LIKES this pane, so the parsed
    // text must be identical to what the escape-free capture already showed — colour is
    // the ONLY thing this feature adds. Compared row by row with trailing padding
    // trimmed, because that is the granularity the Log section renders at.
    let plain_rows: Vec<&str> = plain.lines().map(str::trim_end).collect();
    let styled_rows: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let _ = driver.terminate(session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    assert!(
        styled_has_escapes,
        "capture_tail_styled must pass tmux's escapes through (is `-e` there?); got {styled:?}"
    );
    assert!(
        !plain_has_escapes,
        "capture_tail must stay escape-FREE — the pane classifiers parse it; got {plain:?}"
    );
    let (mods, fg) = marker.unwrap_or_else(|| {
        panic!("the painted marker never reached a parsed span; capture was {styled:?}")
    });
    assert!(
        mods.contains(Modifier::BOLD),
        "the pane's bold attribute was lost; capture was {styled:?}"
    );
    assert_eq!(
        fg,
        Some(Color::Indexed(153)),
        "the pane's indexed colour was lost; capture was {styled:?}"
    );
    assert!(
        !rendered.contains('\u{1b}'),
        "the parser left escape bytes in the rendered text: {rendered:?}"
    );
    assert!(
        rendered.contains("ANSI_MARKER_42"),
        "the marker text was dropped: {rendered:?}"
    );
    // OSC 8: the link TEXT survives, the URL never becomes visible text. Asserted on the
    // parsed output rather than on the raw capture, so it holds whether or not this tmux
    // build chose to re-emit the hyperlink into the grid.
    assert!(
        rendered.contains("LINK_TEXT_7"),
        "the OSC 8 link text was dropped: {rendered:?}"
    );
    assert!(
        !rendered.contains("file://") && !rendered.contains("]8;"),
        "an OSC 8 wrapper leaked into the rendered text: {rendered:?}"
    );
    assert_eq!(
        styled_rows, plain_rows,
        "the styled path must render the SAME TEXT as the escape-free capture — colour is \
         the only thing this feature adds, and a text change here is a visual regression"
    );
}
