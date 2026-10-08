//! `TmuxDriver::send_keys` against a real pane — the one thing `FakeDriver` cannot
//! check by construction, since it records into a `Vec` and never runs tmux. Both
//! delivery paths (literal and paste-buffer) and the copy-mode pane a human leaves
//! behind, each asserted on what the pane's PROCESS received rather than on what a
//! terminal chose to render.

use std::process::Command;
use std::time::Duration;

use agent_manager::tmux::{Driver, TmuxDriver};

use crate::probe::TmuxSocket;
use crate::probe::{
    tmux_available, wait_for_file_contents, wait_for_pane_text, wait_for_pane_text_within,
};

/// Raw option selection is a different terminal operation from sending prose:
/// it must emit only the required arrow keys and one Enter, with no literal option
/// text and no second submit.
#[test]
#[ignore]
fn dialog_selection_against_real_tmux_sends_only_navigation_and_enter() {
    if !tmux_available() {
        eprintln!("skipping live dialog-selection test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-dialog-select");
    let session = format!("amds-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("keys.bin");
    let command = format!("stty -echo; cat > {}", received.display());
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &["sh".to_string(), "-c".to_string(), command],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let first = launched
        .as_ref()
        .ok()
        .map(|_| driver.select_dialog_option(&session, 0, 2));
    let second = first
        .as_ref()
        .is_some_and(|r| r.is_ok())
        .then(|| driver.select_dialog_option(&session, 2, 1));
    let multi = agent_manager::tmux::classify_dialog(concat!(
        " Which layers?\n",
        " ❯ 1. [✔] unit\n",
        "   2. [ ] integration\n",
        "   3. [ ] Type something\n",
        "      Submit\n",
        " Enter to select · Esc to cancel\n",
    ))
    .expect("multi-select fixture");
    let multi_sent = second
        .as_ref()
        .is_some_and(|r| r.is_ok())
        .then(|| driver.select_dialog_options(&session, &multi, &[1]));

    let expected = b"\x1b[B\x1b[B\n\x1b[A\n\n\n\x1b[B\n";
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    while std::time::Instant::now() < deadline {
        bytes = std::fs::read(&received).unwrap_or_default();
        if bytes.len() >= expected.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let _ = driver.terminate(&session);

    launched.expect("launch dialog-key sink");
    first
        .expect("downward selection should run")
        .expect("downward selection should reach the pane");
    second
        .expect("upward selection should run")
        .expect("upward selection should reach the pane");
    multi_sent
        .expect("multi selection should run")
        .expect("checkbox toggles and Submit should reach the pane");
    assert_eq!(
        bytes, expected,
        "single navigation, then multi checkbox toggles + Submit; no prose or extra keys"
    );
}

#[test]
#[ignore]
fn dialog_interactivity_probe_and_selection_work_over_real_tmux() {
    if !tmux_available() {
        eprintln!("skipping live dialog-probe test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-dialog-probe");
    let session = format!("amdp-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let selected = dir.path().join("selected.txt");
    let script = dir.path().join("menu.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
stty -echo -icanon min 1 time 0
sel=1
draw() {{
  printf '\033[2J\033[H'
  printf ' Which path should I take?\n'
  if [ "$sel" = 1 ]; then
    printf ' ❯ 1. Path A\n   2. Path B\n'
  else
    printf '   1. Path A\n ❯ 2. Path B\n'
  fi
  printf ' Enter to select · ↑/↓ to navigate · Esc to cancel\n'
}}
draw
while :; do
  c=$(dd bs=1 count=1 2>/dev/null)
  if [ -z "$c" ]; then
    printf '%s\n' "$sel" > '{}'
    sleep 5
    continue
  fi
  if [ "$c" = "$(printf '\033')" ]; then
    dd bs=1 count=1 2>/dev/null >/dev/null
    key=$(dd bs=1 count=1 2>/dev/null)
    [ "$key" = B ] && sel=2
    [ "$key" = A ] && sel=1
    draw
  fi
done
"#,
            selected.display()
        ),
    )
    .unwrap();
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &["sh".to_string(), script.to_string_lossy().into_owned()],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let ready = launched
        .as_ref()
        .is_ok_and(|_| wait_for_pane_text(&driver, &session, "Which path"));
    let initial = ready
        .then(|| driver.capture_tail(&session, 40))
        .transpose()
        .ok()
        .flatten()
        .and_then(|pane| agent_manager::tmux::classify_dialog(&pane));
    let moved = initial
        .as_ref()
        .map(|dialog| driver.verify_dialog_interactive(&session, dialog));
    let applied = moved
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(|dialog| driver.select_dialog_options(&session, dialog, &[0]));
    let result = wait_for_file_contents(&selected, "1", Duration::from_secs(2));

    let _ = driver.terminate(&session);

    launched.expect("launch redrawable dialog fixture");
    assert!(ready, "fixture should draw its menu");
    let initial = initial.expect("initial pane should classify as a dialog");
    assert_eq!(initial.selected_index, Some(0));
    let moved = moved
        .expect("probe should run")
        .expect("one Down must visibly move the live highlight");
    assert_eq!(moved.selected_index, Some(1));
    applied
        .expect("selection should run")
        .expect("one Up plus Enter should select Path A");
    assert_eq!(result.trim(), "1");
}

/// LIVE-TMUX coverage of `TmuxDriver::send_keys` — the one thing `FakeDriver`
/// cannot check by construction (it records into a `Vec` and never runs tmux,
/// which is exactly why a completely broken `send_keys` sailed past 450 green
/// tests). Drives the REAL driver against a REAL throwaway tmux server on its
/// own socket: literal path, paste-buffer path, `has_clients`,
/// `session_created`, and `terminate`.
///
/// Regression: all three targeted invocations inside `send_keys` used to address
/// the pane as `=<name>`, which tmux rejects (`can't find pane: =<name>`), so
/// every call returned `Err`, `JobScheduler::nudge` treated that as transient and
/// re-parked without consuming a continuation, and autopilot never typed anything
/// into an idle agent. This test fails on that pre-fix code and passes with the
/// `=<name>:` pane form.
///
/// `#[ignore]` (like the other real-substrate tests here) because it needs a tmux
/// binary and spends a couple of seconds of wall clock; run with
/// `cargo test --test integration -- --ignored send_keys_against_real_tmux`.
#[test]
#[ignore]
fn send_keys_against_real_tmux_reaches_the_pane() {
    if !tmux_available() {
        eprintln!("skipping live send_keys test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Private socket + session names that cannot collide with the daemon's
    // (`pmd`/`amloop`/`amtest`/`diagtest`) sockets or any `pmloop-`/`pmchat-`/
    // `agentdeck_` session; both are created and destroyed by this test alone.
    let socket = TmuxSocket::new("am-sendkeys");
    let session = format!("amsk-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());

    // `cat` echoes whatever is typed at it, so the pane text proves the keys
    // actually landed in the pane (a shell would try to EXECUTE them).
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &["cat".to_string()],
        &agent_manager::tmux::ManagedEnv::default(),
    );

    // Everything below records into locals; the server is killed before any
    // assertion runs, so a failure can never leak a tmux server or session.
    let short = format!("amsk-short-marker-{}", std::process::id());
    let short_sent = launched
        .as_ref()
        .ok()
        .map(|_| driver.send_keys(&session, &short));
    let short_seen = short_sent
        .as_ref()
        .is_some_and(|r| r.is_ok())
        .then(|| wait_for_pane_text(&driver, &session, &short));

    // > 800 bytes with newlines ⇒ the load-buffer/paste-buffer path. Short lines
    // so an 80-column pane doesn't wrap them, with unique first/last markers.
    let long_first = format!("amsk-long-first-{}", std::process::id());
    let long_last = format!("amsk-long-last-{}", std::process::id());
    let mut long = String::new();
    long.push_str(&long_first);
    for i in 0..40 {
        long.push_str(&format!("\namsk-filler-line-{i:02}"));
    }
    long.push('\n');
    long.push_str(&long_last);
    let long_len = long.len();
    let long_sent = short_seen
        .is_some()
        .then(|| driver.send_keys(&session, &long));
    let long_seen = long_sent
        .as_ref()
        .is_some_and(|result| result.is_ok())
        .then(|| {
            (
                wait_for_pane_text(&driver, &session, &long_first),
                wait_for_pane_text(&driver, &session, &long_last),
            )
        });

    // A session we never attached to has no clients, and tmux reports when it was
    // created (a plausible recent epoch, not 0).
    let clients = driver.has_clients(&session);
    let created = driver.session_created(&session);

    let terminated = driver.terminate(&session);
    let alive_after = driver.is_alive(&session);
    let _ = Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .status();

    // ---- assertions (server is already gone) ----
    launched.expect("launch_interactive should start the scratch session");
    assert!(
        long_len > 800,
        "payload must exceed the literal threshold, got {long_len}"
    );
    short_sent
        .expect("short send_keys should have been attempted")
        .expect("send_keys (literal path) must succeed against a live pane");
    assert_eq!(
        short_seen,
        Some(true),
        "the literal text must actually appear in the pane"
    );
    long_sent
        .expect("long send_keys should have been attempted")
        .expect("send_keys (paste-buffer path) must succeed against a live pane");
    assert_eq!(
        long_seen,
        Some((true, true)),
        "both ends of the multiline payload must appear in the pane"
    );
    assert!(
        !clients.expect("has_clients should query cleanly"),
        "a detached session has no clients"
    );
    let created = created
        .expect("session_created should query cleanly")
        .expect("tmux should report a creation epoch for a live session");
    assert!(
        created > 1_600_000_000,
        "creation epoch should be a plausible recent time, got {created}"
    );
    terminated.expect("terminate should not error");
    assert!(
        !alive_after.expect("is_alive should query cleanly"),
        "session must be gone after terminate"
    );
}

/// Codex collapses large pastes into a `[Pasted Content N chars]` composer item.
/// Its first Enter expands/accepts that item; a second Enter submits it. Model that
/// state machine in a real tmux pane so `send_keys` cannot report success after only
/// the first, non-submitting Enter.
#[test]
#[ignore]
fn codex_style_collapsed_paste_is_submitted() {
    if !tmux_available() {
        eprintln!("skipping collapsed-paste submit test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-collapsed-paste");
    let session = format!("amcp-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("received.txt");
    let codex = dir.path().join("codex");
    std::fs::copy("/bin/sh", &codex).expect("copy shell as codex fixture");
    let command = format!(
        "IFS= read -r first; \
         printf '\\n› [Pasted Content 1024 chars]\\n'; \
         IFS= read -r ignored; \
         printf '\\n› Ask Codex to do anything\\n'; \
         printf '%s\\n' \"$first\" > {}; \
         sleep 5",
        received.display()
    );
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &[codex.display().to_string(), "-c".to_string(), command],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let payload = format!(
        "AM_COLLAPSED_SUBMIT_{}_{}",
        std::process::id(),
        "x".repeat(1000)
    );
    let sent = launched
        .as_ref()
        .ok()
        .map(|_| driver.send_keys(&session, &payload));
    let placeholder_seen = sent
        .as_ref()
        .is_some_and(|result| result.is_ok())
        .then(|| wait_for_pane_text(&driver, &session, "[Pasted Content"));
    let output = wait_for_file_contents(&received, "AM_COLLAPSED_SUBMIT_", Duration::from_secs(2));

    let _ = driver.terminate(&session);

    launched.expect("launch collapsed-paste fixture");
    assert!(payload.len() > 800 && !payload.contains('\n'));
    sent.expect("collapsed-paste send should run")
        .expect("send_keys should succeed");
    assert_eq!(
        placeholder_seen,
        Some(true),
        "fixture must reach the state where Codex consumed only the first Enter"
    );
    assert!(
        output.contains("AM_COLLAPSED_SUBMIT_"),
        "a second Enter must submit the accepted paste; received {output:?}"
    );
}

/// A production-sized Codex paste can remain in the composer after BOTH fixed-delay Enter
/// presses. This is the live failure shape: pmd recorded the nudge at 06:37:54, but Codex did not
/// record the user turn until a human pressed Enter at 07:28:24. Keep the draft visible after each
/// consumed Enter so the driver must observe the composer and retry instead of assuming two key
/// presses always mean submission.
#[test]
#[ignore]
fn codex_visible_draft_is_retried_until_submitted() {
    if !tmux_available() {
        eprintln!("skipping visible-draft submit test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-codex-visible-draft");
    let session = format!("amcvd-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("received.txt");
    let codex = dir.path().join("codex");
    std::fs::copy("/bin/sh", &codex).expect("copy shell as codex fixture");
    let command = format!(
        "IFS= read -r first; \
         printf '\n› [Pasted Content 2400 chars]\n'; \
         IFS= read -r ignored_one; \
         printf '\n› [Pasted Content 2400 chars]\n'; \
         IFS= read -r ignored_two; \
         printf '\n› Ask Codex to do anything\n'; \
         printf '%s\n' \"$first\" > {}; \
         sleep 5",
        received.display()
    );
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &[codex.display().to_string(), "-c".to_string(), command],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let payload = format!(
        "AM_VISIBLE_DRAFT_SUBMIT_{}_{}",
        std::process::id(),
        "x".repeat(2400)
    );
    let sent = launched
        .as_ref()
        .ok()
        .map(|_| driver.send_keys(&session, &payload));
    let output = wait_for_file_contents(
        &received,
        "AM_VISIBLE_DRAFT_SUBMIT_",
        Duration::from_secs(2),
    );

    let _ = driver.terminate(&session);

    launched.expect("launch visible-draft fixture");
    assert!(payload.len() > 2_000 && !payload.contains('\n'));
    sent.expect("visible-draft send should run")
        .expect("send_keys should keep submitting while the Codex draft remains visible");
    assert!(
        output.contains("AM_VISIBLE_DRAFT_SUBMIT_"),
        "the driver must not return while the pasted draft still needs Enter; received {output:?}"
    );
}

#[test]
#[ignore]
fn codex_visible_literal_message_is_retried_until_submitted() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-codex-literal-draft");
    let session = format!("amcld-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("received.txt");
    let codex = dir.path().join("codex");
    std::fs::copy("/bin/sh", &codex).expect("copy shell as codex fixture");
    let command = format!(
        "IFS= read -r first; printf '\n› %s\n' \"$first\"; IFS= read -r ignored; printf '\n› Ask Codex to do anything\n'; printf '%s\n' \"$first\" > {}; sleep 5",
        received.display()
    );
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &[codex.display().to_string(), "-c".into(), command],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch literal fixture");
    let payload = "AM_LITERAL_MESSAGE_SUBMIT_OK";
    driver
        .send_keys(&session, payload)
        .expect("literal Codex message should retry Enter while draft remains");
    let output = wait_for_file_contents(&received, payload, Duration::from_secs(2));
    let _ = driver.terminate(&session);
    assert!(
        output.contains(payload),
        "literal draft was not submitted: {output:?}"
    );
}

/// Codex refills its composer with a rotating placeholder after a submit, and a short literal
/// message can be a prefix of one. That must read as SUBMITTED: pressing Enter again would hit an
/// empty composer and report a delivery failure that invites the human to resend.
#[test]
#[ignore]
fn codex_literal_message_prefixing_the_placeholder_is_submitted_once() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-codex-placeholder-prefix");
    let session = format!("amcpp-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("received.txt");
    let extra = dir.path().join("extra.txt");
    let codex = dir.path().join("codex");
    std::fs::copy("/bin/sh", &codex).expect("copy shell as codex fixture");
    let command = format!(
        "IFS= read -r first; printf '\n› Write tests for @filename\n'; printf '%s\n' \"$first\" > {}; IFS= read -r again; echo EXTRA > {}; sleep 5",
        received.display(),
        extra.display()
    );
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &[codex.display().to_string(), "-c".into(), command],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch placeholder fixture");
    let sent = driver.send_keys(&session, "Write tests");
    let output = wait_for_file_contents(&received, "Write tests", Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(500));
    let extra_enter = extra.exists();
    let _ = driver.terminate(&session);

    sent.expect("an accepted message must not be reported as a failed delivery");
    assert!(
        output.contains("Write tests"),
        "message not submitted: {output:?}"
    );
    assert!(
        !extra_enter,
        "the driver pressed Enter again on the placeholder composer"
    );
}

/// A literal message wider than the Codex composer wraps onto an indented continuation row, so the
/// `›` row holds only its first part. A swallowed Enter must still be retried, not reported as a
/// delivery while the message sits unsent in the composer.
#[test]
#[ignore]
fn codex_wrapped_literal_message_is_retried_until_submitted() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-codex-wrapped-literal");
    let session = format!("amcwl-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("received.txt");
    let codex = dir.path().join("codex");
    std::fs::copy("/bin/sh", &codex).expect("copy shell as codex fixture");
    // Draw the swallowed draft as Codex does: 60 columns on the `›` row, the rest indented below,
    // then the blank row and footer. Only the second Enter submits.
    let command = format!(
        "IFS= read -r first; rest=$(printf '%s' \"$first\" | cut -c61-); \
         printf '\\n› %.60s\\n  %s\\n\\n  footer\\n' \"$first\" \"$rest\"; \
         IFS= read -r ignored; \
         printf '\\n\\033[1m›\\033[0m \\033[2mAsk Codex to do anything\\033[0m\\n'; \
         printf '%s\\n' \"$first\" > {}; sleep 5",
        received.display()
    );
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &[codex.display().to_string(), "-c".into(), command],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch wrapped-literal fixture");
    let payload = format!(
        "AM_WRAPPED_LITERAL_{} please review the recent commits and summarize the three riskiest changes END",
        std::process::id()
    );
    let sent = driver.send_keys(&session, &payload);
    let output = wait_for_file_contents(&received, "AM_WRAPPED_LITERAL_", Duration::from_secs(2));
    let _ = driver.terminate(&session);

    assert!(payload.len() > 60 && payload.len() <= 800 && !payload.contains('\n'));
    sent.expect("a wrapped literal Codex message should retry Enter while its draft remains");
    assert!(
        output.contains("END"),
        "the wrapped draft was reported delivered but never submitted: {output:?}"
    );
}

/// Codex draws its rotating placeholder dim. A message whose words equal a placeholder must read
/// as submitted once Codex took it, not as a draft still waiting for another Enter.
#[test]
#[ignore]
fn codex_literal_message_equal_to_the_dim_placeholder_is_submitted_once() {
    if !tmux_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-codex-placeholder-equal");
    let session = format!("amcpe-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let received = dir.path().join("received.txt");
    let extra = dir.path().join("extra.txt");
    let codex = dir.path().join("codex");
    std::fs::copy("/bin/sh", &codex).expect("copy shell as codex fixture");
    let command = format!(
        "IFS= read -r first; \
         printf '\\n\\033[1m›\\033[0m \\033[2mSummarize recent commits\\033[0m\\n\\n  footer\\n'; \
         printf '%s\\n' \"$first\" > {}; IFS= read -r again; echo EXTRA > {}; sleep 5",
        received.display(),
        extra.display()
    );
    driver
        .launch_interactive(
            &session,
            dir.path(),
            &[codex.display().to_string(), "-c".into(), command],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .expect("launch placeholder fixture");
    let sent = driver.send_keys(&session, "Summarize recent commits");
    let output = wait_for_file_contents(&received, "Summarize", Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(500));
    let extra_enter = extra.exists();
    let _ = driver.terminate(&session);

    sent.expect("an accepted message must not be reported as a failed delivery");
    assert!(
        output.contains("Summarize recent commits"),
        "message not submitted: {output:?}"
    );
    assert!(
        !extra_enter,
        "the driver pressed Enter again on the dim placeholder composer"
    );
}

/// Opt-in acceptance against the installed Codex TUI. Normal CI never sends an
/// external prompt; set `PM_ACTUAL_CODEX_SUBMIT=1` explicitly to verify that a
/// real production-shaped multiline paste is submitted without a human Enter.
#[test]
#[ignore]
fn actual_codex_long_paste_is_submitted() {
    if std::env::var("PM_ACTUAL_CODEX_SUBMIT").ok().as_deref() != Some("1") {
        return;
    }
    if !tmux_available()
        || !Command::new("codex")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    {
        eprintln!("skipping actual Codex submit test: codex/tmux unavailable");
        return;
    }
    let socket = TmuxSocket::new("am-actual-codex-submit");
    let session = format!("amacs-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let root = std::env::current_dir().expect("current repo directory");
    let launched = driver.launch_interactive(
        &session,
        &root,
        &[
            "env".to_string(),
            "-u".to_string(),
            "CLAUDECODE".to_string(),
            "codex".to_string(),
            "--ask-for-approval".to_string(),
            "never".to_string(),
            "--sandbox".to_string(),
            "workspace-write".to_string(),
        ],
        &agent_manager::tmux::ManagedEnv::default(),
    );
    let ready = launched.as_ref().is_ok_and(|_| {
        wait_for_pane_text_within(&driver, &session, "OpenAI Codex", Duration::from_secs(15))
    });
    if ready {
        // Match production's cold-start grace. The first OpenAI Codex frame is a
        // loading splash; pasting while it switches to the configured model can
        // duplicate the collapsed item and is not a send_keys submission failure.
        std::thread::sleep(Duration::from_secs(8));
    }
    let marker = "AM_DRIVER_LONG_SUBMIT_OK";
    let payload = format!(
        "You are a long-running agent working toward a test goal on a heartbeat.\n\n\
         ## Your goal\n\nDo not inspect or modify files.\n\n\
         ## What to do now\n\nReply with exactly {marker}.\n\n{}",
        (0..60)
            .map(|index| format!("- safe production-shaped filler line {index:02}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let sent = ready.then(|| driver.send_keys(&session, &payload));
    let submitted = sent.as_ref().is_some_and(|result| result.is_ok()).then(|| {
        wait_for_pane_text_within(
            &driver,
            &session,
            "› You are a long-running agent working toward a test goal on a heartbeat.",
            Duration::from_secs(15),
        )
    });
    let final_pane = driver
        .capture_tail(&session, 120)
        .unwrap_or_else(|error| format!("<capture failed: {error}>"));
    let pane_command = Command::new("tmux")
        .args([
            "-L",
            socket.name(),
            "display-message",
            "-p",
            "-t",
            &format!("={session}:"),
            "#{pane_current_command}",
        ])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|error| format!("<command probe failed: {error}>"));

    let _ = driver.terminate(&session);

    launched.expect("launch actual Codex");
    assert!(ready, "actual Codex composer never became ready");
    assert!(payload.len() > 2_000 && payload.contains('\n'));
    sent.expect("actual Codex send should run")
        .expect("send long prompt to actual Codex");
    assert_eq!(
        submitted,
        Some(true),
        "actual Codex must submit the long prompt without a manual Enter; \
         pane_current_command={pane_command:?}\n{final_pane}"
    );
    assert!(
        !final_pane.contains("› - safe production-shaped filler line"),
        "a multiline heartbeat must remain one user turn, not split into a second prompt:\n{final_pane}"
    );
}

/// ACCEPTANCE (real tmux): a nudge must still reach the agent when its pane was left in
/// COPY-MODE — and this is another bug class a `FakeDriver` cannot catch, because the
/// whole failure lives in a tmux fact.
///
/// Reproduction in one sentence: a human attaches, scrolls back, and detaches while still
/// in copy-mode. Measured on tmux 3.6a, what then happens to a nudge is that
/// `paste-buffer` exits **0** and the text lands, and the following `send-keys Enter` is
/// consumed by the copy-mode key table — the mode flips 1→0 and no newline ever reaches
/// the pty. Of a two-line payload the pty saw 19 of 34 bytes and nothing was submitted.
/// Because BOTH tmux calls reported success, `send_keys` returned `Ok`, `nudge` read that
/// as delivered and cleared `pending_context`, and the payload — including a human's
/// parked answer — was destroyed. Every production nudge takes this path, since
/// `loop_nudge_prompt` is multi-line.
///
/// The observable is deliberately a `cat` stub's OUTPUT FILE, not the pane text: `cat` in
/// canonical mode only flushes complete lines, so the payload's last line appears there
/// only if a real newline actually arrived. The pane would render the pasted text either
/// way, which is exactly how this stayed invisible.
///
/// Also pins the `display-message -p` trap the probe is built around (exits 0 printing
/// NOTHING for an unresolvable target) by probing a bogus session and requiring
/// "not in a mode" rather than `Err` or a false positive.
///
/// Hygiene mirrors `send_keys_against_real_tmux_reaches_the_pane`: a private per-pid
/// socket, every result recorded into a local, and `kill-server` BEFORE the first
/// assertion, so a failing assertion can never leak a tmux server or session. No
/// pmd/pmtui is started, so there is no daemon pid to kill.
///
/// `#[ignore]`; run with `cargo test --test integration -- --ignored \
/// nudge_reaches_a_pane_left_in_copy_mode`.
#[test]
#[ignore]
fn nudge_reaches_a_pane_left_in_copy_mode() {
    if !tmux_available() {
        eprintln!("skipping copy-mode nudge test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = TmuxSocket::new("am-copymode");
    let session = format!("amcm-{}", std::process::id());
    let driver = TmuxDriver::with_socket(socket.name());
    let recv = dir.path().join("received.txt");
    let launched = driver.launch_interactive(
        &session,
        dir.path(),
        &[
            "sh".to_string(),
            "-c".to_string(),
            format!("cat > {}", recv.display()),
        ],
        &agent_manager::tmux::ManagedEnv::default(),
    );

    // Put the pane into copy-mode, exactly as a human scrolling back and detaching leaves it.
    let entered = Command::new("tmux")
        .args([
            "-L",
            socket.name(),
            "copy-mode",
            "-t",
            &format!("={session}:"),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    std::thread::sleep(Duration::from_millis(300));
    let in_mode_before = driver.pane_in_mode(&session);
    let in_mode_bogus = driver.pane_in_mode("amcm-no-such-session");

    // Multi-line ⇒ the paste path, i.e. the one every real nudge takes.
    let first = format!("amcm-first-{}", std::process::id());
    let last = format!("amcm-last-{}", std::process::id());
    let payload = format!("{first}\n{last}");
    let sent = driver.send_keys(&session, &payload);
    let received = wait_for_file_contents(&recv, &last, Duration::from_secs(5));
    let in_mode_after = driver.pane_in_mode(&session);

    let terminated = driver.terminate(&session);
    let _ = Command::new("tmux")
        .args(["-L", socket.name(), "kill-server"])
        .status();

    // ---- assertions (server is already gone) ----
    launched.expect("launch_interactive should start the cat stub");
    assert!(entered, "tmux copy-mode should have been entered");
    assert!(payload.contains('\n'), "payload must take the paste path");
    assert_eq!(
        in_mode_before.ok(),
        Some(true),
        "the pane really is in copy-mode before the nudge"
    );
    assert_eq!(
        in_mode_bogus.ok(),
        Some(false),
        "an unresolvable target must read not-in-a-mode, never Err — display-message -p \
         exits 0 printing nothing"
    );
    sent.expect("send_keys must succeed against a pane in copy-mode");
    assert!(
        received.contains(&first),
        "first line must reach the process, got {received:?}"
    );
    // THE REGRESSION: pre-fix this line never arrived, because the submitting Enter was
    // eaten by the copy-mode key table while both tmux calls still reported success.
    assert!(
        received.contains(&last),
        "LAST line must reach the process — its absence IS the swallowed submit, got \
         {received:?}"
    );
    assert!(
        received.ends_with('\n'),
        "the submitting newline must reach the process, got {received:?}"
    );
    assert_eq!(
        in_mode_after.ok(),
        Some(false),
        "the pane must be out of copy-mode after the nudge"
    );
    terminated.expect("terminate should not error");
}
