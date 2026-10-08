//! The keybar: the tiers it sheds chips through, the order the menu is drawn in, the chips
//! that do not apply to a row, the direction a mode chip names, and the widths it must
//! never overflow.

use super::*;

#[test]
fn keybar_advertises_the_goal_key() {
    // The affordance has to be discoverable: Normal mode shows a `g Goal` chip
    // alongside the existing ones (wide terminal so the whole bar renders). The row
    // carries an open stop because the keybar is CONTEXT-sensitive — `a Answer` is
    // only offered when there is something to answer.
    let mut v = autopilot_loop_view("bot");
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    let app = app_with(vec![v], UiMode::Normal);
    // Wide enough that the whole bar renders (only `p Pause`, the lowest-ranked action, may shed).
    // 170, not 160: the `v Verbose` chip's label is a touch wider than the old `Review`, which
    // nudged the full-bar threshold up a couple of columns — this is the width that shows it all.
    let mut terminal = Terminal::new(TestBackend::new(210, 30)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_text(&terminal);
    assert!(screen.contains("Goal"), "no goal chip in the keybar");
    // The pre-existing chips are still there — nothing was reused to make room. The mode chip
    // names the direction it would FLIP to, and this row is on Autopilot (which is what `g` now
    // requires), so it reads "→ standard". `Pause` is NOT asserted: `p` exists (m36) but ranks
    // last of the action keys, so `fit_chips` may drop it at this width — which is by design and
    // is pinned by the width-tier tests below, not here.
    for label in ["Answer", "Mode \u{2192} standard", "Quit", "Delete"] {
        assert!(screen.contains(label), "{label} chip disappeared");
    }
}

// --- M28: send one message to a running agent (`s`) --------------------------

#[test]
fn keybar_tiers_adapt_to_width() {
    let app = keybar_app();

    // Wide: today's full `key + label` chips.
    let wide = keybar_line(&app, 120);
    let text = line_text(&wide);
    assert!(
        text.contains("Restart"),
        "wide tier lost its labels: {text}"
    );
    assert!(text.contains("Quit"), "wide tier lost `q Quit`: {text}");
    assert!(text.contains("Answer"), "wide tier lost `a Answer`: {text}");
    assert!(wide.width() <= 120, "wide bar overflows: {}", wide.width());

    // Medium: keys only, still chip-styled — so MORE keys fit than the labelled
    // bar could show at this width.
    let med = keybar_line(&app, 70);
    let text = line_text(&med);
    assert!(
        !text.contains("Quit"),
        "medium tier still shows labels: {text}"
    );
    assert!(
        text.contains('q') && text.contains('?'),
        "keys missing: {text}"
    );
    assert!(
        text.matches('·').count() > line_text(&keybar_line(&app, 100)).matches('·').count(),
        "keys-only tier should fit more chips than the labelled bar at 100: {text}"
    );
    assert!(med.width() <= 70, "medium bar overflows: {}", med.width());

    // Narrow: just enough to be honest — the `?` help hint plus `q`.
    let narrow = keybar_line(&app, 40);
    let text = line_text(&narrow);
    assert!(text.contains('?'), "narrow tier lost the `?` hint: {text}");
    assert!(text.contains('q'), "narrow tier lost `q`: {text}");
    assert!(
        narrow.width() <= 40,
        "narrow bar overflows 40 cols: {} ({text})",
        narrow.width()
    );
}

#[test]
fn keybar_never_overflows_any_width() {
    // The whole point of the tiers: the bar FITS, at every width, in every mode,
    // with and without a status. `Line::width` is ratatui's own display-width
    // measure, so this also checks `text_cols`'s char-count approximation.
    let mut app = keybar_app();
    for status in [
        "",
        "ready",
        &"auth-rewrite goal updated — next check-in".repeat(4),
    ] {
        app.status = status.into();
        for mode in [
            UiMode::Normal,
            UiMode::Answering {
                input: "B".into(),
                choice: 0,
                scroll: 0,
            },
            UiMode::Creating(CreateForm::new()),
            UiMode::Confirming {
                id: "auth-rewrite".into(),
                session: "pmi-x".into(),
                what: Confirmable::Remove,
            },
            UiMode::EditingGoal {
                id: "auth-rewrite".into(),
                brief: PathBuf::from("/nonexistent/brief.md"),
                current: "ship it".into(),
                input: goal_buf("ship it faster"),
                then_autopilot: false,
            },
            UiMode::Help { scroll: 0 },
        ] {
            app.mode = mode;
            for w in 1..=200u16 {
                let line = keybar_line(&app, w);
                assert!(
                    line.width() <= usize::from(w),
                    "{w} cols: bar is {} wide: {}",
                    line.width(),
                    line_text(&line)
                );
            }
        }
    }
}

#[test]
fn the_keybar_renders_the_menu_in_the_order_the_user_asked_for() {
    // ANTI-DRIFT on the LAYOUT, which has now been specified twice and is invisible to
    // every other test here (they all assert `contains`, which is order-blind).
    //
    //   *"n New, d remove, r restart (restart the claude/codex in the current tmux),
    //    s Send (instead of o), a Autopilot, g Goal"*
    //   *"for the a Answer, can you put it before Mode"*
    //
    // The bar renders in `BINDINGS` order, so the table IS the layout — which also means
    // that inserting a new binding in a tidy-looking place silently rearranges the menu.
    // That is what this catches.
    let order: Vec<&str> = BINDINGS
        .iter()
        .filter(|b| b.scope == Scope::Normal)
        .map(|b| b.key)
        .collect();
    assert_eq!(
        order,
        vec![
            // Navigation first: where you are before what you do. Movement, `Tab`, and New are
            // documented in help and rendered as top controls rather than duplicated here.
            "Enter", //
            // The action block, in the user's words. `p` is not one of the six the user
            // named; it sits with `r` because they are the pair you reach for when a
            // session is misbehaving.
            // `a` (Apply) sits with `d`: both are what you do to a FINISHED job — bring its commit
            // over, or clear its row — and the chip only appears on a job row that committed.
            "f", "R", "d", "a", "p", "r", "s", "m", "g",
            // `i` (Directive) sits beside `g` (Goal): the two per-session standing-text settings,
            // the positive mandate and its restrictive limit, one letter apart.
            "i", "c", //
            // `e` (Decider) — pick an autopilot session's decider engine + model. An autonomy dial
            // like `c`, so it sits beside it, one letter along.
            "e",
            // `w` (Worker) — pick the session's worker model, straight after the decider picker since
            // both open the same model list.
            "w",
            // `v` (Verbose) — the decision lane. An action like the block above, placed before the
            // meta keys so `?`/`q` stay the honest minimum at the end.
            "v", // The honest minimum last — the two keys that are never shed.
            "?", "q",
        ],
        "the bottom menu is not in the order the user specified"
    );

    // And the ORDER is not the shed priority: context-sensitive `s` is the attention key,
    // so it must still be the last action chip to go as the bar narrows. Asserted
    // because the natural way to "move a chip" is to renumber ranks, which would look
    // right on a wide terminal and quietly drop Answer first on a narrow one.
    let rank_of = |k: &str| {
        BINDINGS
            .iter()
            .find(|b| b.scope == Scope::Normal && b.key == k)
            .map(|b| b.rank)
            .unwrap_or(u8::MAX)
    };
    for other in ["n", "d", "r", "m", "g", "c", "Enter", "/"] {
        assert!(
            rank_of("s") < rank_of(other),
            "`s` must outlast `{other}` on a narrowing bar (ranks {} vs {})",
            rank_of("s"),
            rank_of(other)
        );
    }
}

#[test]
fn keybar_hides_chips_that_do_not_apply() {
    // Context-sensitivity, on the two axes that survive: an OPEN STOP gates `a Answer`,
    // and the AUTOPILOT tier gates `g Goal`/`c Cadence` (a cadence on a row pmd does not
    // drive reads nothing). `?` keeps everything discoverable, which is what makes hiding
    // fair.
    //
    // A STANDARD row with no open stop: no `Answer` (nothing to answer) and no
    // `Goal`/`Cadence` (pmd does not drive it). `Attach`/`Delete`/`Restart`/`Send` still
    // apply to any agent-loop row.
    let app = app_with(vec![agent_loop_view("bot")], UiMode::Normal); // Standard, no stops
    let text = line_text(&keybar_line(&app, 220));
    for gone in ["Answer", "Goal", "Cadence"] {
        assert!(
            !text.contains(gone),
            "{gone} chip offered on a Standard row with no stop: {text}"
        );
    }
    for shown in ["Attach", "Delete", "Restart", "Send"] {
        assert!(text.contains(shown), "{shown} chip missing: {text}");
    }

    // An AUTOPILOT row: `Goal` applies (it is the direction pmd steers by). Still no
    // `Answer` without an open stop.
    let text = line_text(&keybar_line(
        &app_with(vec![autopilot_loop_view("bot")], UiMode::Normal),
        220,
    ));
    assert!(
        text.contains("Goal"),
        "Goal chip missing on Autopilot: {text}"
    );
    assert!(
        !text.contains("Answer"),
        "Answer offered with no open stop: {text}"
    );

    // A row WITH an open stop shows the one canonical Answer action. `s` remains a
    // keyboard alias but its Message chip yields to Answer instead of duplicating it.
    let text = line_text(&keybar_line(&keybar_app(), 220));
    for shown in ["Attach", "Delete", "Answer", "Restart"] {
        assert!(text.contains(shown), "{shown} chip missing: {text}");
    }

    // An empty list still keeps the two bottom-menu escape hatches. New is the
    // persistent top-right control, so it is deliberately absent from this surface.
    let app = app_with(vec![], UiMode::Normal);
    let text = line_text(&keybar_line(&app, 220));
    for shown in ["Help", "Quit"] {
        assert!(text.contains(shown), "{shown} chip missing: {text}");
    }
}

#[test]
fn keybar_mode_chip_names_the_direction_of_the_press() {
    // Preserved from the pre-adaptive bar, through the `m` -> `m` rename: the autonomy
    // dial is a 2-value toggle, so the chip says what the press will DO to the selected
    // row. The ARROW is what carries that — a bare "Mode" would read as a state readout.
    let mut app = keybar_app();
    app.projects[0].tier = Some(Tier::Standard);
    let bar = |a: &App| line_text(&keybar_line(a, 160));
    assert!(
        bar(&app).contains("Mode \u{2192} autopilot"),
        "{}",
        bar(&app)
    );
    app.projects[0].tier = Some(Tier::Autopilot);
    assert!(
        bar(&app).contains("Mode \u{2192} standard"),
        "{}",
        bar(&app)
    );
    // No readable tier => no direction to name, so the bare noun and NOT a guess.
    app.projects[0].tier = None;
    let text = bar(&app);
    assert!(text.contains("Mode"), "{text}");
    assert!(
        !text.contains('\u{2192}'),
        "an unknown tier must not claim a direction: {text}"
    );
}

#[test]
fn keybar_enter_chip_says_resume_on_a_paused_row() {
    // Same rule as the `m` chip above: the bar names what the press will DO. `Enter` on a
    // paused row resumes it, and "Attach" in front of a row whose agent `p` just killed
    // describes something that cannot happen.
    let mut app = keybar_app();
    let bar = |a: &App| line_text(&keybar_line(a, 160));
    assert!(bar(&app).contains("Attach"), "{}", bar(&app));
    app.projects[0].enabled = false;
    let text = bar(&app);
    assert!(text.contains("Resume"), "{text}");
    assert!(
        !text.contains("Attach"),
        "a paused row must not be offered an attach: {text}"
    );
}

#[test]
fn paused_task_inspector_offers_resume_not_attach_or_pause() {
    let mut app = keybar_app();
    app.projects[0].enabled = false;
    app.mode = UiMode::Board;
    app.board_detail_open = true;

    let text = line_text(&keybar_line(&app, 240));

    assert!(text.contains("Resume"), "{text}");
    assert!(!text.contains("Attach"), "{text}");
    assert!(!text.contains("Pause"), "{text}");
}

#[test]
fn a_staged_spawn_row_offers_only_the_keys_its_handlers_accept() {
    // Control: the same row merely paused offers every start, so the fixture reaches each chip.
    let mut paused = agent_loop_view("kid");
    paused.enabled = false;
    let control = line_text(&keybar_line(&app_with(vec![paused], UiMode::Normal), 240));
    for offered in ["Resume", "Fork", "Pause", "Restart", "Send", "Mode"] {
        assert!(control.contains(offered), "{offered}: {control}");
    }

    // Staged: every start and Message refuses it (`start_refusal`), so only what still acts stays.
    let dir = tempfile::tempdir().unwrap();
    let (registry, _root) = reg_with_agent_loop(dir.path(), "kid");
    let mut app = app_with(vec![staged_spawn_view("kid")], UiMode::Normal);
    app.registry_path = registry;
    let bar = line_text(&keybar_line(&app, 240));
    for kept in ["Rename", "Delete", "Worker", "Help", "Quit"] {
        assert!(bar.contains(kept), "{kept} missing: {bar}");
    }
    for refused in [
        "Resume", "Attach", "Fork", "Pause", "Restart", "Send", "Answer", "Mode",
    ] {
        assert!(!bar.contains(refused), "offers {refused}: {bar}");
    }

    // The hit regions come from the same frame, so no click reaches a hidden chip, and the one a
    // click does reach runs the same handler as its key.
    let mut terminal = Terminal::new(TestBackend::new(240, 40)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let hits = app.key_hits.borrow().clone();
    assert_eq!(
        hits.iter().map(|hit| hit.code).collect::<Vec<_>>(),
        vec![
            KeyCode::Char('R'),
            KeyCode::Char('d'),
            KeyCode::Char('w'),
            KeyCode::Char('?'),
            KeyCode::Char('q'),
        ]
    );
    let delete = hits
        .iter()
        .find(|hit| hit.code == KeyCode::Char('d'))
        .unwrap();
    let mut handled = false;
    handle_event(
        &mut app,
        Event::Mouse(ratatui::crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: delete.area.x,
            row: delete.area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &mut handled,
    );
    assert!(handled);
    assert!(
        matches!(
            app.mode,
            UiMode::Confirming {
                what: Confirmable::Remove,
                ..
            }
        ),
        "{:?}",
        app.mode
    );
}

#[test]
fn a_staged_spawn_row_routes_s_to_its_refusal_even_with_an_open_stop() {
    // `s`, its chip and the shelf read `message_route`, and a staged row's first Message is its
    // spawn request's: nothing may answer or message it before the broker launches it.
    let mut v = staged_spawn_view("kid");
    assert_eq!(message_route(&v), MessageRoute::SpawnStaged);
    v.tier = Some(Tier::Autopilot);
    v.stops = vec![stop("s1", "ambiguity", RiskClass::Medium)];
    assert_eq!(message_route(&v), MessageRoute::SpawnStaged);
    // The staged mark wins over a hand-edited fork label, as `start_refusal` does.
    v.incomplete_fork = true;
    assert_eq!(message_route(&v), MessageRoute::SpawnStaged);
}
