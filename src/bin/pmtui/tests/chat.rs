//! When a chat holds the conversation: the chat pane the log falls back to, the row's
//! chat chip, the return copy that distinguishes a detach from an end, and the marker
//! `refresh` reads before it ever probes tmux.

use super::*;

#[test]
fn the_log_renders_the_chat_pane_when_there_is_no_agent_pane() {
    // *"i don't see we render the claude on the main session"* — on a STANDARD row there is no
    // `pmloop-` at all (pmd does not launch one), so the Log said `agent not running — m
    // switches autopilot` while the claude the human was typing into sat in a `pmchat-` pane
    // this dashboard never looked at. True about the agent pmd would drive, and useless.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    let chat = session_name("bot", &root);
    let mut v = agent_loop_view("bot");
    v.human_attached = true;
    // ONLY the chat session is alive — the loop pane does not exist, which is the whole case.
    let mut app = app_with_driver(
        vec![v],
        UiMode::Normal,
        Box::new(FakePane::live(&chat, "CHAT_PANE_MARKER_42\n")),
    );
    app.registry_path = reg_path;

    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&t);

    assert!(
        screen.contains("CHAT_PANE_MARKER_42"),
        "the chat's own pane must render — it is the claude the human is talking to: {screen}"
    );
    // …and the title says WHOSE output it is, so this can never be mistaken for the driven
    // agent's pane.
    assert!(screen.contains("bot · live"), "{screen}");
    assert!(
        !screen.contains("agent not running"),
        "the placeholder must not claim nothing is running while a chat is live: {screen}"
    );
}

#[test]
fn row_and_preview_say_when_a_chat_holds_the_conversation() {
    // A live `pmchat-` stops pmd driving this session (`JobScheduler::drive` gate 1 →
    // `chat_lock::is_active`). The daemon side is silent by design, so the dashboard has to say
    // it: a `chat` chip on the row, plus a preview line naming the CONSEQUENCE and the way out.
    //
    // AUTOPILOT, and that is the point of the fixture. The line used to read "poll parked …
    // /exit resumes it" on EVERY row with a live chat, including Standard ones — where nothing
    // was ever going to poll it, so the sentence described a machine that was not running. It
    // now speaks about autopilot only where there IS autopilot; the Standard case is asserted
    // below.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut v = autopilot_loop_view("bot");
    v.human_attached = true;
    let mut app = app_with(vec![v], UiMode::Normal);
    app.registry_path = reg_path.clone();

    assert!(
        project_row_text(&app.projects[0]).contains("user"),
        "the row needs a human-attached chip: {}",
        project_row_text(&app.projects[0])
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|f| render(f, &app))
        .expect("render chat chip");
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("human attached"),
        "the preview must name the consequence: {screen}"
    );
    assert!(
        screen.contains("resumes automatically on detach"),
        "the preview must name the way out: {screen}"
    );
    // NO BOUND CLAIMED. This asserted `contains("12h")` until m39, when the reap that bound
    // described was removed (user: *"i don't think we should cap polling if the tmux or claude
    // skill ok"*). A message promising a cap the harness does not enforce is the lie this
    // codebase keeps having to fix, so the absence is asserted, not just the new text.
    assert!(
        !screen.contains("cap"),
        "the preview must not promise a cap that no longer exists: {screen}"
    );
    // NO INTERNAL VOCABULARY. User, on the old line: *"I understand but what does it mean for the
    // user? it doesn't mean anything"* — "poll" and "parked" are pmd's words, not theirs.
    for jargon in ["poll", "parked"] {
        assert!(
            !screen.contains(jargon),
            "the preview must not explain itself in pmd's vocabulary ({jargon:?}): {screen}"
        );
    }

    // A STANDARD row shows NO chat line at all: nothing drives it, so there is no autopilot to
    // report paused, and the old "open Ns — you drive this session from here" line was noise the user
    // asked to drop (*"remove … the message with label chat — open *s — you drive this session"*).
    // The head meta already says who drives.
    let mut sv = agent_loop_view("bot");
    sv.human_attached = true;
    let mut standard = app_with(vec![sv], UiMode::Normal);
    standard.registry_path = reg_path.clone();
    // 126, not 120: this row's head is long ("… next: dispatch task-4 · you drive it"), and the
    // SESSIONS pane's inner padding widened its sidebar (floor 44), leaving the preview a touch
    // narrower — so the head's tail truncates at 120 the way the head ladder always does when it
    // runs out of room. Give it the width to show in full; the assertion is about the head NAMING
    // the driver, not about truncation.
    let mut t2 = Terminal::new(TestBackend::new(126, 30)).unwrap();
    t2.draw(|f| render(f, &standard))
        .expect("render standard chat");
    let std_screen = screen_text(&t2);
    assert!(
        !std_screen.contains("autopilot paused"),
        "a Standard row has no autopilot to pause: {std_screen}"
    );
    assert!(
        !std_screen.contains("you drive this session from here"),
        "the redundant Standard-chat line must be gone: {std_screen}"
    );
    assert!(
        std_screen.contains("you drive it"),
        "the head meta still says whose session this is: {std_screen}"
    );

    // And for a STANDARD row a live session is EXACTLY what "running" means (m74/m75). pmd never
    // drives a Standard session, so its frozen ledger posture is not the truth — its live (or
    // detached-but-surviving) REPL is. `status_category` buckets on `session_live` (set by
    // `refresh` from a real pmchat-/pmloop- probe, NOT the marker-gated `human_attached`): a live pane
    // ⇒ RUNNING (●), nothing up ⇒ IDLE (○), mirroring an Interactive row (user: *"when i use
    // standard, the status on the top bar doesn't really reflect correctly"* → "mirror
    // Interactive"). This REVERSES the earlier "a chat is only a chip" rule — that rule was the
    // very behavior that reported a live Standard session as idle.
    let mut idle = agent_loop_view("bot"); // Standard AgentLoop (view() default tier)
    idle.posture = Posture::Fresh;
    let mut chatting = idle.clone();
    chatting.session_live = true;
    assert_eq!(
        status_category(&idle),
        2,
        "a Standard row with no live session is idle (○)"
    );
    assert_eq!(
        status_category(&chatting),
        1,
        "a Standard row with a live session is running (●), not idle"
    );
}

#[test]
fn chat_return_copy_distinguishes_a_detach_from_an_end() {
    let detached = chat_return_status("bot", true);
    assert!(
        detached.contains("detached") && detached.contains("still running"),
        "a detach must say the unified terminal remains available: {detached}"
    );
    let ended = chat_return_status("bot", false);
    assert!(
        ended.contains("terminal ended"),
        "a confirmed end must name the terminal state: {ended}"
    );
    assert!(
        !ended.contains("still running"),
        "an ended terminal must not claim to still be running: {ended}"
    );
    assert!(chat_park_note(true).contains("Enter"));
    assert!(!chat_park_note(true).contains("bot"));
    assert!(chat_park_note(false).contains("terminal ended"));
    assert!(!chat_park_note(false).contains("pmd"));
}

#[test]
fn chat_chip_is_absent_without_a_live_chat() {
    // The complement: no chat ⇒ no chip and no park line, so the indicator can only
    // ever appear when something really is holding the conversation.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, _root) = reg_with_agent_loop(dir.path(), "bot");
    let mut app = app_with(vec![agent_loop_view("bot")], UiMode::Normal);
    app.registry_path = reg_path;
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render no chat");
    let screen = screen_text(&terminal);
    assert!(
        !screen.contains("autopilot paused"),
        "the park line showed with no chat: {screen}"
    );
}

#[test]
fn refresh_reads_the_marker_before_probing_tmux_for_a_chat() {
    // The MARKER is the gate, and the LIVE SESSION is the answer:
    //   * no marker  ⇒ no tmux probe at all (agent-loop rows cost zero subprocesses
    //     per tick, which is what `refresh` did before this field existed) — asserted
    //     here as "a live pmchat- alone never lights the chip";
    //   * marker + live session ⇒ chip on.
    // Needs a REAL tmux server because `refresh` builds its own `TmuxDriver` (the
    // preview seam is a different driver), so a FakeDriver cannot see this.
    if !tmux_available() {
        eprintln!("skipping chat-marker refresh test: tmux not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_agent_loop(dir.path(), "bot");
    std::fs::create_dir_all(&root).unwrap();
    let sp = ProjectPaths::for_session(&root, "bot");
    let socket = TmuxSocket::new("pmtui-chat");
    let driver = TmuxDriver::with_socket(socket.name());
    let chat_session = session_name("bot", &root);
    driver
        .launch_interactive(
            &chat_session,
            &root,
            &["sh".to_string()],
            &tmux::ManagedEnv::default(),
        )
        .unwrap();

    let mut app = loop_app(&reg_path);
    app.socket = socket.name().to_string();

    app.refresh();
    let without_marker = app.projects[0].human_attached;
    chat_lock::mark(
        &sp,
        std::process::id(),
        &chat_session,
        socket.name(),
        SystemClock.now(),
    )
    .unwrap();
    app.refresh();
    let with_marker = app.projects[0].human_attached;

    let _ = driver.terminate(&chat_session);
    let _ = std::process::Command::new("tmux")
        .arg("-L")
        .arg(socket.name())
        .arg("kill-server")
        .output();

    assert!(
        !without_marker,
        "with no marker the row must not probe/claim a chat"
    );
    assert!(
        with_marker,
        "fresh attach marker + a live terminal ⇒ human-attached is on"
    );
}
