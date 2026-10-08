//! Developer-facing gallery of real pmtui frames at laptop terminal sizes.
//! The fixture uses typed scratch state and private tmux sockets, then persists
//! plain and ANSI captures under `PMTUI_UI_OUT` for visual review. It also runs in
//! the mandatory ignored real-tmux suite, so each capture first waits for text only
//! its target state draws: a passing run never names a frame after a state it missed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_manager::clock::{Clock, SystemClock};
use agent_manager::job::{self, AgentLoopState, JobRun};
use agent_manager::pmstate::{OpenStop, StopKind, StopStatus};
use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, session_name};

use crate::keystrokes::{send_key, send_literal, send_mouse_click};
use crate::pmtui_fixture::{EnterFixture, PmdSibling, enter_fixture_at};
use crate::probe::wait_until;

const DEFAULT_SIZES: &str = "80x24,100x28,120x32";

#[test]
#[ignore = "developer gallery: writes real pmtui captures for visual review"]
fn laptop_ui_gallery_renders_real_main_interaction_states() {
    let output = gallery_output_dir();
    std::fs::create_dir_all(&output).expect("create pmtui gallery output");

    for (index, (width, height)) in gallery_sizes().into_iter().enumerate() {
        let fx = enter_fixture_at(
            &format!("gallery-{index}"),
            PmdSibling::Missing,
            width,
            height,
        );
        assert!(fx.up, "pmtui did not paint at {width}x{height}");
        seed_gallery(&fx);
        launch_gallery_panes(&fx);
        let g = Gallery {
            fx: &fx,
            output: &output,
            width,
            height,
        };
        // Every wait below polls for text that ONLY its target state draws, so a frame is never
        // written from the state before it. Text the previous screen already shows (a keybar
        // chip, a dormant shelf, a Board behind an overlay) cannot prove a state rendered.
        g.await_text("main", "Release Gate");
        // pmtui draws a freshly launched pane as working until two stable refreshes confirm it
        // idle. Waiting for the idle panes launched above to flip to `○` keeps every frame on
        // the settled fleet (and hands-on in the Pending lane) rather than a first-second
        // transient.
        g.await_pane("the settled fleet", |pane| {
            ["hands-on", "monitoring", "autopilot-docs"]
                .iter()
                .all(|id| pane.contains(&format!("\u{25cb} {id}")))
        });
        g.frame("main");

        g.key("Tab");
        // The Task lane title. The Session rail spells its group `NEEDS YOU (2)`, and its
        // `PAUSED / OFFLINE` group shows at larger sizes, so neither word alone proves Tasks.
        g.await_text("tasks", "NEEDS YOU 2");
        g.frame("tasks");

        let pane = fx
            .host
            .capture_tail(&fx.host_session, usize::from(height) + 4)
            .expect("capture Task card controls");
        let lines: Vec<_> = pane.lines().collect();
        let visible_start = lines.len().saturating_sub(usize::from(height));
        let (restart_x, restart_y) = lines
            .iter()
            .enumerate()
            .skip(visible_start)
            .find_map(|(row, line)| {
                let byte = line.find(" r  Restart")?;
                Some((
                    line[..byte].chars().count() as u16,
                    (row - visible_start) as u16,
                ))
            })
            .unwrap_or_else(|| panic!("Restart card action missing at {width}x{height}: {pane}"));
        assert!(send_mouse_click(
            &fx.host_socket,
            &fx.host_session,
            restart_x,
            restart_y,
        ));
        g.await_text("the selected-card Restart confirmation", "Confirm restart");
        g.key("Escape");
        g.await_gone("Task lanes after cancelling Restart", "Confirm restart");

        g.key("Enter");
        // The detail's transcript rule; Task lanes never draw it.
        g.await_text("task-inspector", "preview");
        g.frame("task-inspector");

        g.literal("s");
        // The overlay title: the inspector keybar already offers `s Answer`.
        g.await_text("task-answer", "Answer \u{b7} Release Gate");
        g.frame("task-answer");
        g.key("Escape");
        g.await_gone(
            "Task detail after cancelling Answer",
            "Answer \u{b7} Release Gate",
        );

        g.literal("n");
        g.await_text("task-create", "New task");
        g.frame("task-create");
        g.key("Escape");
        g.await_gone("Task detail after cancelling create", "New task");
        g.key("Escape");
        g.await_gone("Task lanes after closing detail", "preview");

        g.literal("l");
        // `l` from Needs You selects the first Pending card, whose title then carries the `▸`
        // gutter. Matched as `▸ <glyph> hands-on`, because side-by-side lanes put Release Gate's
        // own `▸` on the same screen row as the unselected hands-on title.
        g.await_pane("tasks-pending", |pane| {
            pane.lines().any(|line| selected_card(line, "hands-on"))
        });
        g.frame("tasks-pending");
        g.key("Tab");
        g.await_text("the Session view", "SESSIONS");

        g.select_session("needs-review");
        g.literal("s");
        // The overlay title: the Session keybar already offers `s Answer`.
        g.await_text("answer", "Answer \u{b7} Release Gate");
        g.frame("answer");
        g.key("Escape");
        g.await_gone(
            "the Session view after cancelling Answer",
            "Answer \u{b7} Release Gate",
        );

        g.literal("/");
        g.await_text("the switcher", "Switch session");
        g.literal("monitor");
        // The echoed query plus its narrowed count: typing streams in, and `mon` already
        // narrows to one match before the rest of the query arrives.
        g.await_pane("switcher", |pane| {
            pane.contains("> monitor_") && pane.contains("1 match")
        });
        g.frame("switcher");
        g.key("Escape");
        g.await_gone(
            "the Session view after cancelling the switcher",
            "Switch session",
        );

        g.literal("?");
        // A help section heading; the keybar's `? Help` chip is not the overlay.
        g.await_text("help", "NAVIGATION");
        g.frame("help");
        g.key("Escape");
        g.await_gone("the Session view after closing help", "NAVIGATION");

        g.select_session("hands-on");
        g.literal("s");
        // The ACTIVE composer's keybar: the dormant shelf already shows `Message · hands-on`.
        g.await_text("the Message composer", "Keep draft");
        let long_message = format!(
            "{}End of the long message.",
            "Review the laptop layout while this deliberately long message wraps across several rows without truncating or drawing more than one caret. ".repeat(5)
        );
        g.literal(&long_message);
        // The typed text streams in, and every repetition ends the same way, so only the unique
        // final sentence proves the whole message was consumed. The caret is a REVERSED cell now,
        // not a drawn `_`, and `capture-pane -p` reports no attributes — so the text is the evidence
        // and the frame below is where a human checks the caret.
        g.await_pane("message-long", |pane| {
            composer_text(pane)
                .is_some_and(|text| text.ends_with(&unspaced("End of the long message.")))
        });
        assert!(
            fx.host.is_alive(&fx.host_session).unwrap_or(false),
            "long Message input crashed pmtui at {width}x{height}"
        );
        g.frame("message-long");
        // Keep the draft and return to the dashboard, the state `stop_dashboard` quits from.
        g.key("Escape");
        g.await_gone("the Session view after keeping the draft", "Keep draft");

        // THE GOAL BUFFER (`g`), on a session pmd drives — the same editor as the Message field, which
        // is the whole claim of this change and the one a unit test cannot make: the chord has to reach
        // the library through a real terminal's encoding of it. `Ctrl+J` arrives as `Char('j')+CONTROL`,
        // which is why it is the newline this asserts rather than `Shift+Enter`.
        g.select_session("monitoring");
        g.literal("g");
        g.await_text("the Goal field", "Goal \u{b7} monitoring");
        g.literal(" then cite the source");
        g.key("C-j");
        g.literal("and keep the suite green");
        g.await_pane("goal-multiline", |pane| {
            pane.contains("then cite the source") && pane.contains("and keep the suite green")
        });
        g.frame("goal-multiline");
        g.key("Escape");
        g.await_gone(
            "the Session view after cancelling the goal",
            "Goal \u{b7} monitoring",
        );

        // THE DIRECTIVE BUFFER (`i`) and its rescind CHORD. `^X` alone is the editor prefix, so this
        // also proves it does not act on its own: the field is still open after it.
        g.literal("i");
        g.await_text("the Directive field", "Directive \u{b7} monitoring");
        g.literal("never rewrite history");
        g.key("C-x");
        g.await_text(
            "the Directive field after a bare ^X",
            "never rewrite history",
        );
        g.frame("directive");
        g.key("C-x");
        g.key("C-r");
        g.await_gone(
            "the Session view after rescinding",
            "Directive \u{b7} monitoring",
        );
    }

    println!("pmtui UI gallery: {}", output.display());
}

/// One gallery size's driver: keystrokes into the hosted `pmtui`, waits that fail with the pane
/// they last saw, and frame capture.
struct Gallery<'a> {
    fx: &'a EnterFixture,
    output: &'a Path,
    width: u16,
    height: u16,
}

impl Gallery<'_> {
    fn key(&self, key: &str) {
        assert!(
            send_key(&self.fx.host_socket, &self.fx.host_session, key),
            "send {key} at {}x{}",
            self.width,
            self.height
        );
    }

    fn literal(&self, text: &str) {
        assert!(
            send_literal(&self.fx.host_socket, &self.fx.host_session, text),
            "type {text:?} at {}x{}",
            self.width,
            self.height
        );
    }

    /// Poll (bounded) until the pane satisfies `ready`. A failed capture never counts, so a
    /// dead dashboard cannot satisfy an absence check.
    fn await_pane(&self, state: &str, ready: impl Fn(&str) -> bool) {
        let rendered = wait_until(Duration::from_secs(5), || {
            self.fx
                .host
                .capture_tail(&self.fx.host_session, 200)
                .is_ok_and(|pane| ready(&pane))
        });
        assert!(
            rendered,
            "{state} never rendered at {}x{}; pane was:\n{}",
            self.width,
            self.height,
            self.fx
                .host
                .capture_tail(&self.fx.host_session, 200)
                .unwrap_or_default()
        );
    }

    fn await_text(&self, state: &str, needle: &str) {
        self.await_pane(state, |pane| pane.contains(needle));
    }

    fn await_gone(&self, state: &str, needle: &str) {
        self.await_pane(state, |pane| !pane.contains(needle));
    }

    /// Select `id` through the quick switcher and wait for the switch to land.
    fn select_session(&self, id: &str) {
        self.literal("/");
        self.await_text("the switcher", "Switch session");
        self.literal(id);
        self.key("Enter");
        self.await_gone("the Session view after switching", "Switch session");
        self.await_text("the switched selection", &format!("selected {id}"));
    }

    fn frame(&self, state: &str) {
        write_frame(self.fx, self.output, self.width, self.height, state);
    }
}

/// Whether `line` draws `id`'s Task card title with the selection gutter: `▸ <glyph> <id>`.
fn selected_card(line: &str, id: &str) -> bool {
    line.match_indices("\u{25b8} ").any(|(at, marker)| {
        let mut after = line[at + marker.len()..].chars();
        after.next();
        after.as_str().starts_with(&format!(" {id}"))
    })
}

/// The Message composer's text as drawn, WITH EVERY SPACE DROPPED. `None` when no composer is on
/// screen.
///
/// The composer is the rightmost pane in both layouts, so each of its rows ends with its right
/// border and its text starts after the last border before that. Spaces go because WHERE THE ROWS
/// BREAK IS THE LIBRARY'S BUSINESS: `ratatui-textarea` wraps on word boundaries and counts
/// punctuation as one of them, so at 120 columns it put `message` on one row and `.` on the next.
/// Neither concatenating the rows nor joining them on a space reconstructs that, and pinning the
/// breaks would make this test assert the library's line-breaking rather than ours. Comparing the
/// character sequence modulo wrapping whitespace still proves the whole message arrived and nothing
/// was truncated, which is what the long-message case is for.
fn composer_text(pane: &str) -> Option<String> {
    let mut rows = pane
        .lines()
        .skip_while(|row| !row.contains("\u{250c} Message \u{b7} "));
    rows.next()?;
    let mut text = String::new();
    for row in rows {
        let Some(body) = row.trim_end().strip_suffix('\u{2502}') else {
            break;
        };
        let Some((_, inner)) = body.rsplit_once('\u{2502}') else {
            break;
        };
        text.extend(inner.chars().filter(|c| !c.is_whitespace()));
    }
    Some(text)
}

/// `text` with its whitespace dropped — the form [`composer_text`] returns.
fn unspaced(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn gallery_output_dir() -> PathBuf {
    std::env::var_os("PMTUI_UI_OUT").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("target/pmtui-ui"),
        PathBuf::from,
    )
}

fn gallery_sizes() -> Vec<(u16, u16)> {
    std::env::var("PMTUI_UI_SIZES")
        .unwrap_or_else(|_| DEFAULT_SIZES.to_string())
        .split(',')
        .map(|size| {
            let (width, height) = size
                .trim()
                .split_once('x')
                .unwrap_or_else(|| panic!("invalid PMTUI_UI_SIZES entry {size:?}"));
            let width = width
                .parse::<u16>()
                .unwrap_or_else(|_| panic!("invalid gallery width in {size:?}"));
            let height = height
                .parse::<u16>()
                .unwrap_or_else(|_| panic!("invalid gallery height in {size:?}"));
            (width, height)
        })
        .collect()
}

fn seed_gallery(fx: &EnterFixture) {
    let now = SystemClock.now();
    let mut entries = vec![
        entry(
            &fx.proj,
            "needs-review",
            "Choose the release window",
            true,
            Engine::Claude,
            Some(300),
        ),
        entry(
            &fx.proj,
            "needs-security",
            "Approve the dependency exception",
            true,
            Engine::Claude,
            Some(300),
        ),
        entry(
            &fx.proj,
            "active-build",
            "Run the repository verification matrix",
            true,
            Engine::Codex,
            Some(120),
        ),
        entry(
            &fx.proj,
            "working-tests",
            "Fix the remaining tmux regression",
            true,
            Engine::Codex,
            Some(120),
        ),
        entry(
            &fx.proj,
            "monitoring",
            "Watch the merged release checks",
            true,
            Engine::Claude,
            Some(300),
        ),
        entry(
            &fx.proj,
            "autopilot-docs",
            "Refresh product documentation",
            true,
            Engine::Claude,
            Some(300),
        ),
        entry(
            &fx.proj,
            "hands-on",
            "Prototype the keyboard workflow",
            true,
            Engine::Claude,
            None,
        ),
        entry(
            &fx.proj,
            "pending-design",
            "Review the task inspector layout",
            true,
            Engine::Claude,
            None,
        ),
        entry(
            &fx.proj,
            "pending-refactor",
            "Simplify the board projection",
            true,
            Engine::Codex,
            None,
        ),
        entry(
            &fx.proj,
            "pending-docs",
            "Write the local development guide",
            true,
            Engine::Claude,
            None,
        ),
        entry(
            &fx.proj,
            "paused-cleanup",
            "Remove stale fixture state",
            false,
            Engine::Codex,
            None,
        ),
        entry(
            &fx.proj,
            "paused-release",
            "Prepare the next release branch",
            false,
            Engine::Claude,
            None,
        ),
    ];
    entries[0].display_name = Some("Release Gate".into());
    entries[5].forked_from = Some("active-build".into());
    Registry { projects: entries }
        .save(&fx.reg_path)
        .expect("write gallery registry");

    let mut needs = AgentLoopState::fresh(Engine::Claude, Some(300), now - 900);
    needs.run = JobRun::Blocked {
        stop_ids: vec!["stop-release".into()],
        since: now - 420,
    };
    needs.last_status = Some(
        "Release candidate is green; deployment ownership still needs a human decision.".into(),
    );
    needs.last_plan = Some("Wait for the release-window decision, then continue.".into());
    needs.open_stops = vec![OpenStop {
        id: "stop-release".into(),
        kind: StopKind::Ambiguity,
        pane_dialog: None,
        channel: None,
        context_ref: Some("release checklist".into()),
        question: Some("Ship in the current window or hold for the morning review?".into()),
        options: vec!["Ship now".into(), "Hold for morning review".into()],
        authorized_responders: Vec::new(),
        message_id: None,
        first_posted: now - 420,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    }];
    save_session(&fx.proj, "needs-review", Tier::Autopilot, needs);
    let mut needs_security = AgentLoopState::fresh(Engine::Claude, Some(300), now - 800);
    needs_security.run = JobRun::Blocked {
        stop_ids: vec!["stop-security".into()],
        since: now - 180,
    };
    needs_security.last_status = Some("Dependency exception needs an owner decision.".into());
    needs_security.open_stops = vec![OpenStop {
        id: "stop-security".into(),
        kind: StopKind::Ambiguity,
        pane_dialog: None,
        channel: None,
        context_ref: Some("security review".into()),
        question: Some("Accept the bounded exception or replace the dependency?".into()),
        options: vec!["Accept exception".into(), "Replace dependency".into()],
        authorized_responders: Vec::new(),
        message_id: None,
        first_posted: now - 180,
        last_polled: None,
        last_seen_reply_ts: None,
        status: StopStatus::AwaitingReply,
    }];
    save_session(&fx.proj, "needs-security", Tier::Autopilot, needs_security);

    let mut active = AgentLoopState::fresh(Engine::Codex, Some(120), now - 1800);
    active.run = JobRun::Running {
        seq: 8,
        session: session_name("active-build", &fx.proj),
        deadline: now + 900,
    };
    active.last_status = Some(
        "Running the repository verification matrix and checking the responsive dashboard.".into(),
    );
    active.last_plan = Some("Finish verification, inspect captures, then commit.".into());
    active.updated_at = now - 8;
    save_session(&fx.proj, "active-build", Tier::Autopilot, active);
    let mut tests = AgentLoopState::fresh(Engine::Codex, Some(120), now - 1200);
    tests.run = JobRun::Running {
        seq: 5,
        session: session_name("working-tests", &fx.proj),
        deadline: now + 600,
    };
    tests.last_status = Some("Reproducing the tmux input regression.".into());
    tests.last_plan = Some("Fix, rerun, then report.".into());
    save_session(&fx.proj, "working-tests", Tier::Autopilot, tests);

    let mut monitoring = AgentLoopState::fresh(Engine::Claude, Some(300), now - 3600);
    monitoring.run = JobRun::Monitoring { until: now + 247 };
    monitoring.last_status = Some("All checks passed; monitoring the next scheduled wake.".into());
    monitoring.last_plan = Some("Recheck CI and report only if state changes.".into());
    monitoring.updated_at = now - 53;
    save_session(&fx.proj, "monitoring", Tier::Autopilot, monitoring);
    let mut docs = AgentLoopState::fresh(Engine::Claude, Some(300), now - 2400);
    docs.run = JobRun::Monitoring { until: now + 360 };
    docs.last_status = Some("Documentation is current; waiting for the next wake.".into());
    save_session(&fx.proj, "autopilot-docs", Tier::Autopilot, docs);

    let mut hands_on = AgentLoopState::fresh(Engine::Claude, None, now - 300);
    hands_on.last_status = Some("Interactive session ready for the next message.".into());
    hands_on.last_plan = Some("You drive it.".into());
    save_session(&fx.proj, "hands-on", Tier::Standard, hands_on);
    for id in ["pending-design", "pending-refactor", "pending-docs"] {
        save_session(
            &fx.proj,
            id,
            Tier::Standard,
            AgentLoopState::fresh(Engine::Claude, None, now - 120),
        );
    }

    let mut paused = AgentLoopState::fresh(Engine::Codex, None, now - 7200);
    paused.last_status = Some("Cleanup paused before any destructive action.".into());
    save_session(&fx.proj, "paused-cleanup", Tier::Standard, paused);
    save_session(
        &fx.proj,
        "paused-release",
        Tier::Standard,
        AgentLoopState::fresh(Engine::Claude, None, now - 3600),
    );
}

fn entry(
    root: &Path,
    id: &str,
    title: &str,
    enabled: bool,
    engine: Engine,
    cadence_s: Option<u64>,
) -> ProjectEntry {
    ProjectEntry {
        id: id.into(),
        display_name: None,
        root: root.to_path_buf(),
        enabled,
        mode: Mode::AgentLoop,
        engine: Some(engine),
        worker_model: None,
        initial_prompt: None,
        task_title: Some(title.into()),
        forked_from: None,
        spawned_by: None,
        launch: None,
        conversation_id: None,
        cadence_s,
    }
}

fn save_session(root: &Path, id: &str, autonomy: Tier, ledger: AgentLoopState) {
    let paths = ProjectPaths::for_session(root, id);
    state::write_json_atomic(
        &paths.config(),
        &Config {
            autonomy,
            step_timeout_s: 1800,
            max_failures: 3,
            stuck_threshold: 3,
            coordinator_lease_s: 1860,
            decider_engine: Engine::Claude,
            decider_model: None,
        },
    )
    .expect("write gallery config");
    state::write_text_atomic(&paths.brief(), "Keep the UI fixture safe and inspectable.")
        .expect("write gallery goal");
    job::save(&paths, &ledger).expect("write gallery ledger");
}

fn launch_gallery_panes(fx: &EnterFixture) {
    launch_pane(
        fx,
        "active-build",
        "printf 'Compiling the responsive workbench...\\n'; exec sleep 600",
    );
    for id in ["monitoring", "autopilot-docs", "hands-on"] {
        launch_pane(fx, id, "printf '> \\n'; exec cat");
    }
}

fn launch_pane(fx: &EnterFixture, id: &str, command: &str) {
    fx.agent
        .launch_interactive(
            &session_name(id, &fx.proj),
            &fx.proj,
            &["/bin/sh".into(), "-c".into(), command.into()],
            &agent_manager::tmux::ManagedEnv::default(),
        )
        .unwrap_or_else(|error| panic!("launch gallery pane {id}: {error:#}"));
}

fn write_frame(fx: &EnterFixture, output: &Path, width: u16, height: u16, state: &str) {
    let lines = usize::from(height).saturating_add(4);
    let plain = fx
        .host
        .capture_tail(&fx.host_session, lines)
        .expect("capture plain pmtui frame");
    let ansi = fx
        .host
        .capture_tail_styled(&fx.host_session, lines)
        .expect("capture styled pmtui frame");
    assert!(
        plain.contains("pmtui"),
        "blank {state} frame at {width}x{height}"
    );
    let stem = format!("{width}x{height}-{state}");
    std::fs::write(output.join(format!("{stem}.txt")), plain).expect("write plain UI frame");
    std::fs::write(output.join(format!("{stem}.ansi")), ansi).expect("write ANSI UI frame");
}
