//! What a session row says and how it degrades: fixed columns, semantic state styling,
//! attention-first groups, optional age/human fields, selection, and click mapping.

use super::*;

fn live_sectioned_app() -> App {
    let mut app = sectioned_app();
    for view in &mut app.projects {
        if view.enabled {
            view.session_live = true;
        }
    }
    app
}

#[test]
fn row_text_has_icon_id_posture_and_tier() {
    let mut v = view("auth-rewrite", Posture::Working, vec![]);
    v.session_live = true;
    let row = project_row_text(&v);
    assert!(row.contains("auth-rewrite"), "{row}");
    assert!(row.contains("working"), "{row}");
    assert!(row.contains("[A]"), "{row}");
}

#[test]
fn mode_tags_remain_visible_and_semantically_distinct() {
    let mut v = view("session", Posture::Working, vec![]);
    v.tier = Some(Tier::Autopilot);
    let autopilot = project_row_detail(&v, 40, 0, ROW_TEST_NOW);
    let tag = autopilot
        .spans
        .iter()
        .find(|span| span.content.contains("[A]"))
        .expect("autopilot tag");
    assert_eq!(tag.style.fg, Some(agent_manager::theme::accent_alt()));
    assert!(tag.style.add_modifier.contains(Modifier::BOLD));

    v.tier = Some(Tier::Standard);
    let standard = project_row_detail(&v, 40, 0, ROW_TEST_NOW);
    let tag = standard
        .spans
        .iter()
        .find(|span| span.content.contains("[S]"))
        .expect("standard tag");
    assert!(tag.style.add_modifier.contains(Modifier::DIM));
}

#[test]
fn age_label_is_compact_exact_and_never_negative() {
    // 3 columns is the whole budget, so the unit ladder has to carry the range
    // without ever widening — and without under-reporting how stale a session is.
    for (secs, want) in [
        (0i64, "0s"),
        (7, "7s"),
        (59, "59s"),
        (60, "1m"),
        (150, "2m"),
        (3_599, "59m"),
        (3_600, "1h"),
        (86_399, "23h"),
        (86_400, "1d"),
        (6 * 86_400, "6d"),
        (7 * 86_400, "1w"),
        (51 * 7 * 86_400, "51w"),
        (365 * 86_400, "1y"),
        (900 * 86_400, "2y"),
    ] {
        let got = age_label(Some(1_000_000), 1_000_000 + secs);
        assert_eq!(got, want, "{secs}s should read {want}");
        assert!(got.chars().count() <= 3, "{got:?} is wider than 3 columns");
    }
    // Nothing has ever run, and a skewed/foreign clock — neither may render a
    // negative age or a wide string.
    assert_eq!(age_label(None, 1_000_000), "—");
    assert_eq!(age_label(Some(2_000_000), 1_000_000), "0s");
    // A corrupt far-past timestamp still fits the column.
    assert_eq!(age_label(Some(0), i64::from(u32::MAX) * 4_000), "99y");
}

#[test]
fn fmt_clock_is_a_fixed_wall_clock_not_a_ticking_age() {
    // User: *"it is better to just display the current timestamp"*. `fmt_clock` renders the
    // FIXED HH:MM:SS an event happened at, so a feed reads as a timestamped log rather than a set
    // of counters climbing every render. Offset passed in, so the format is tested without a zone.
    assert_eq!(fmt_clock(0, 0), "00:00:00", "epoch midnight, UTC");
    assert_eq!(fmt_clock(3_661, 0), "01:01:01", "1h 1m 1s past midnight");
    assert_eq!(fmt_clock(86_399, 0), "23:59:59", "last second of the day");
    // The offset shifts the wall clock and wraps within the day.
    assert_eq!(
        fmt_clock(0, 3_600),
        "01:00:00",
        "a +1h zone reads one hour later"
    );
    assert_eq!(
        fmt_clock(0, -3_600),
        "23:00:00",
        "a -1h zone wraps to the previous day's hour"
    );
    // A negative (skewed/foreign-clock) timestamp saturates rather than panicking.
    assert_eq!(fmt_clock(i64::MIN, 0).len(), 8, "always HH:MM:SS wide");
    // The zone-aware entry point is exactly the pure core at the machine's offset.
    assert_eq!(clock_label(3_661).len(), 8);
}

#[test]
fn row_shows_a_compact_age_and_sheds_it_first_on_a_narrow_pane() {
    // `last_activity` was already loaded and thrown away; the row surfaces it. And it is the FIRST
    // thing shed when the LIST PANE runs out of columns, because the age is context while the NAME
    // is what says which session this is.
    let mut v = view("auth-rewrite", Posture::NeedsYou, vec![]);
    v.last_activity = Some(1_000_000);
    let now = 1_000_000 + 2 * 3_600; // 2h ago

    // The age is on the STATE line, right-aligned to the pane edge so the ages form a column.
    let wide = project_row_detail_text_at(&v, 80, 0, now);
    assert!(wide.contains("2h"), "no age on a roomy state line: {wide}");
    assert!(
        wide.ends_with(" 2h"),
        "the age is not flush right: {wide:?}"
    );

    // It is shed only when the line cannot hold it at all, which no real sidebar width reaches.
    let narrow = project_row_detail_text_at(&v, ROW_AGE_COL_W + ROW_DETAIL_INDENT, 0, now);
    assert!(
        !narrow.contains("2h"),
        "age not shed on a pane with no room: {narrow}"
    );

    // THE IDENTITY LINE IS THE NAME, and nothing competes with it.
    let identity = project_row_text_at(&v, 39);
    assert!(identity.contains("auth-rewrite"), "{identity}");
    assert!(
        !identity.contains("2h"),
        "the age belongs below: {identity}"
    );

    // The state words are never shed.
    let detail = project_row_detail_text_at(&v, 39, 0, now);
    for keep in ["needs you", "[A]"] {
        assert!(detail.contains(keep), "{keep:?} is missing: {detail}");
    }
}

/// THE `user` CHIP CAN NO LONGER BE EVICTED BY THE AGE. The two used to share the identity line,
/// the row is rendered with no wrap so ratatui cuts the TAIL, and the age was pushed first — so an
/// overflowing row kept a nice-to-have and dropped the one chip that explains why a session is not
/// advancing. That shipped, and only the real-tmux `chat_chip_says_the_poll_is_parked` caught it.
/// They are now on DIFFERENT LINES, so the hazard is structural rather than a priority rule, and
/// this test pins that: the chip is on the state line, the age is on the identity line, and no
/// width makes one cost the other.
#[test]
fn the_age_and_the_chat_chip_no_longer_compete_for_the_same_columns() {
    let mut v = view("auth-rewrite", Posture::Monitoring, vec![]);
    v.last_activity = Some(1_000_000);
    v.human_attached = true;
    let now = 1_000_000 + 2 * 3_600; // 2h ago

    for content_w in [39, 45, 80] {
        let identity = project_row_text_at(&v, content_w);
        let detail = project_row_detail_text_at(&v, content_w, 0, now);
        assert!(
            detail.contains("user"),
            "the human-attached chip must survive at {content_w}: {detail}"
        );
        assert!(
            detail.contains("2h"),
            "and so must the age, on the same line: {detail}"
        );
        assert!(
            !identity.contains("user") && !identity.contains("2h"),
            "the identity line carries neither: {identity}"
        );
    }
}

#[test]
fn row_age_never_widens_the_row_past_its_pane() {
    // NEITHER LINE EVER OVERFLOWS ITS PANE, at any width down to nothing — the identity line pads
    // or elides the name to fit, and the state line admits each field against the room left.
    let mut v = view("sixteen-char-idx", Posture::Stuck, vec![]);
    v.last_activity = Some(0);
    v.stops = vec![stop("s1", "publish", RiskClass::Hard)];
    let widest_age = 51 * 7 * 86_400; // `51w`
    // The IDENTITY line fits any pane that can hold its glyph at all.
    for w in ROW_FIXED_W..=48u16 {
        let identity = project_row_line(&v, w, 0);
        assert!(
            identity.width() <= usize::from(w),
            "the identity line is {} wide at {w}: {identity:?}",
            identity.width()
        );
    }
    // The STATE line has a floor, and it is a consequence of the invariant rather than an
    // oversight: the state word and the tier tag are never shed, so below indent + word + tag it
    // cannot fit. That floor is far under the 36-column inner width of the narrowest sidebar the
    // layout ever hands it, so no real pane reaches it.
    for w in 36..=48u16 {
        let state = project_row_detail(&v, w, 0, widest_age);
        assert!(
            state.width() <= usize::from(w),
            "the state line is {} wide at {w}: {state:?}",
            state.width()
        );
    }
    // At a real sidebar width the widest age is still drawn whole.
    let text = project_row_detail_text_at(&v, 41, 0, widest_age);
    assert!(text.contains("51w"), "{text}");
    // The state word is on the row's OTHER line, where no width sheds it.
    assert!(
        project_row_detail_text_at(&v, 41, 0, ROW_TEST_NOW).contains("stuck"),
        "the state line must still say stuck"
    );
}

#[test]
fn rendered_rows_shed_the_age_when_the_list_pane_narrows() {
    // The same shedding through the REAL render path, so the pane-width arithmetic
    // (`area.width` minus the borders and the `> ` marker) is under test too.
    //
    // `last_activity: None` renders the age as `—`, which makes the assertion
    // independent of the wall clock `render_sessions` reads; both widths are below
    // `NARROW_W`, so the list has the frame to itself and no preview `—` placeholder
    // can be mistaken for the age column.
    let mut v = view("auth-rewrite", Posture::Working, vec![]);
    v.session_live = true;
    v.last_activity = None;
    let mut app = app_with(vec![v], UiMode::Normal);
    app.status = String::new();

    // 49 cols: list pane 49 → 45 inner ≥ ROW_AGE_W, so the age is drawn.
    let mut terminal = Terminal::new(TestBackend::new(49, 12)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render 49");
    let roomy = screen_text(&terminal);
    assert!(roomy.contains('—'), "no age column at 49 cols: {roomy}");

    // 43 cols: THE AGE NOW SURVIVES HERE, and that is the point of the two-line row. It used to
    // be shed at this width because the status word and the tier tag were fixed fields on the same
    // line; with them moved down, the only thing the age competes with is the name.
    let mut terminal = Terminal::new(TestBackend::new(43, 12)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render 43");
    let tight = screen_text(&terminal);
    assert!(tight.contains('—'), "the age was shed at 43 cols: {tight}");
    // Both of the row's lines are on screen: the name on the identity line, the state word on the
    // one under it.
    assert!(
        tight.contains("auth-rewrite") && tight.contains("working"),
        "the row itself was shed instead: {tight}"
    );
}

#[test]
fn the_rows_trailing_widths_still_add_up_to_the_age_threshold() {
    // The one piece of arithmetic every shedding test depends on, asserted instead of
    // trusted: the age is affordable exactly when the fixed head and the age itself fit.
    // (It used to include a trailing severity badge's four columns. There is no badge at
    // all now, which is why this got narrower rather than why the test got weaker.)
    assert_eq!(ROW_AGE_W, ROW_FIXED_W + ROW_AGE_COL_W);
    // …and every posture word FITS the 13-column label field — the constraint a new or
    // renamed posture could quietly break, since `truncate` would then clip a character and
    // the row would read `monitorin…`. Checked rather than trusted, because that field's
    // width is part of the `ROW_FIXED_W` asserted just above.
    for p in [
        Posture::Fresh,
        Posture::Working,
        Posture::Monitoring,
        Posture::NeedsYou,
        Posture::Running,
        Posture::Stuck,
        Posture::Done,
    ] {
        assert!(
            p.label().chars().count() <= 13,
            "{p:?}'s label does not fit the row: {:?}",
            p.label()
        );
    }
}

#[test]
fn attention_row_uses_a_semantic_foreground_without_a_fill() {
    // TWO rows, with selection deliberately on the calm one, so the attention style is
    // measured independently from the selected-row background.
    let mut calm = view("calm-one", Posture::Monitoring, vec![]);
    calm.session_live = true;
    let asking = view("asking", Posture::NeedsYou, vec![]);
    let mut app = app_with(vec![calm, asking.clone()], UiMode::Normal);
    app.selected = 0;
    let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");

    let label = styles_under(&terminal, "needs you").expect("no needs-you row");
    for (fg, m) in &label {
        assert_eq!(
            *fg,
            agent_manager::theme::soft(),
            "the label lost its hue: {label:?}"
        );
        assert!(
            m.contains(Modifier::BOLD) && !m.contains(Modifier::REVERSED),
            "attention should be strong text, not a filled banner: {label:?}"
        );
    }
    // The id is ORDINARY TEXT, and attention must not turn it into a filled state label: which
    // session is shouting has to be readable without depending on a hue.
    let id = project_row_line(&asking, 80, 0)
        .spans
        .into_iter()
        .find(|span| span.content.contains("asking"))
        .expect("no id span");
    assert!(
        id.style.fg == Some(attention::text())
            && !id.style.add_modifier.contains(Modifier::REVERSED),
        "the id was hued or picked up attention fill: {id:?}"
    );
    // And the unselected calm row is untouched.
    let calm = styles_under(&terminal, "monitoring").expect("no calm row");
    assert!(calm.iter().all(|(_, m)| !m.contains(Modifier::REVERSED)));
}

#[test]
fn every_secondary_label_keeps_the_same_left_aligned_field() {
    let attention = project_row_detail(&view("x", Posture::NeedsYou, vec![]), 80, 0, ROW_TEST_NOW)
        .spans
        .into_iter()
        .find(|s| s.content.contains("needs you"))
        .expect("the attention label span");
    assert!(
        !attention.style.add_modifier.contains(Modifier::REVERSED),
        "the calm rail must not reverse-fill status fields: {attention:?}"
    );
    assert_eq!(
        attention.content.as_ref(),
        "needs you",
        "the state word is the line's own first field, not a padded column"
    );

    let mut calm_view = view("y", Posture::Monitoring, vec![]);
    calm_view.session_live = true;
    let calm = project_row_detail(&calm_view, 80, 0, ROW_TEST_NOW)
        .spans
        .into_iter()
        .find(|s| s.content.contains("monitoring"))
        .expect("the calm label span");
    assert!(!calm.style.add_modifier.contains(Modifier::REVERSED));
    assert!(
        !calm.content.starts_with(' '),
        "a calm label should stay left-aligned: {:?}",
        calm.content
    );
}

#[test]
fn hard_and_soft_rows_use_restrained_semantic_text() {
    // ON THE BUILT LINES, not the screen. The state word's own span carries the style, and a screen
    // lookup cannot be scoped to the sidebar: the PREVIEW pane's title names the selected session
    // and its head repeats the state word, both on screen rows the sidebar shares — so a search by
    // text read the preview's styling and reported it as the row's.
    let hard = project_row_detail(&view("wedged", Posture::Stuck, vec![]), 41, 0, ROW_TEST_NOW)
        .spans
        .into_iter()
        .find(|s| s.content.contains("stuck"))
        .expect("no stuck word");
    assert_eq!(hard.style.fg, Some(agent_manager::theme::hard()));
    assert!(hard.style.add_modifier.contains(Modifier::BOLD));
    assert!(!hard.style.add_modifier.contains(Modifier::REVERSED));

    let soft = project_row_detail(
        &view("asking", Posture::NeedsYou, vec![]),
        41,
        0,
        ROW_TEST_NOW,
    )
    .spans
    .into_iter()
    .find(|s| s.content.contains("needs you"))
    .expect("no needs-you word");
    assert_eq!(soft.style.fg, Some(agent_manager::theme::soft()));
    assert!(soft.style.add_modifier.contains(Modifier::BOLD));
    assert!(!soft.style.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn semantic_states_do_not_change_the_fixed_row_width() {
    let mut offline = view("offline", Posture::Working, vec![]);
    offline.session_live = false;
    let states = [
        view("hard", Posture::Stuck, vec![]),
        view("soft", Posture::NeedsYou, vec![]),
        view("live", Posture::Working, vec![]),
        view("calm", Posture::Monitoring, vec![]),
        offline,
    ];
    for v in states {
        for w in 24u16..=64 {
            let row = project_row_line(&v, w, 0);
            assert!(
                row.width() <= usize::from(w).max(usize::from(ROW_FIXED_W)),
                "{} overflows at {w}: {} cols",
                v.id,
                row.width()
            );
        }
    }
}

#[test]
fn wide_display_names_keep_the_status_and_tier_on_the_floor_width_row() {
    // Up to ~125 terminal columns the list pane sits at its 44-column floor, which leaves the
    // row exactly its fixed head. A display name is measured in terminal cells, not chars, so
    // CJK and emoji names cannot push the status label or tier tag past the pane edge.
    let content_w = sessions_width(100).saturating_sub(8);
    assert_eq!(content_w, 36, "the 44-column sidebar's inner width");
    for name in [
        "发布协调工作流程",
        "发布协调工作流程发布协调工作流程",
        "🚀 release 🚀🚀🚀🚀🚀🚀",
        "⚠\u{fe0f}⚠\u{fe0f} production hotfix release",
        "❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}",
    ] {
        for (tier, tag) in [(Tier::Autopilot, "[A]"), (Tier::Standard, "[S]")] {
            let mut v = view("wide", Posture::NeedsYou, vec![]);
            v.display_name = Some(name.into());
            v.tier = Some(tier);
            v.session_live = true;
            let row = project_row_line(&v, content_w, 0);
            let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
            assert_eq!(row.width(), usize::from(content_w), "{text}");
            let detail = project_row_detail_text_at(&v, content_w, 0, ROW_TEST_NOW);
            assert!(detail.starts_with("    needs you"), "{detail}");
            assert!(detail.contains(tag), "{detail}");
        }
    }

    let mut app = app_with(
        vec![view("wide", Posture::NeedsYou, vec![])],
        UiMode::Normal,
    );
    app.projects[0].display_name = Some("发布协调工作流程发布协调工作流程".into());
    app.projects[0].session_live = true;
    let rows = session_rows(&app, 100, 20);
    // The test buffer reads a wide glyph's trailing cell back as a blank, so match one glyph.
    let at = rows
        .iter()
        .position(|row| row.contains('发'))
        .unwrap_or_else(|| panic!("wide name never rendered: {rows:#?}"));
    let detail = &rows[at + 1];
    assert!(
        detail.contains("needs you") && detail.contains("[A]"),
        "{detail}"
    );
}

#[test]
fn a_driven_row_wears_no_special_colour_on_its_id() {
    let mut v = view("driven", Posture::Working, vec![]);
    v.tier = Some(Tier::Autopilot);
    v.session_live = true;
    let first = project_row_line(&v, 80, 0);
    let second = project_row_line(&v, 80, 0);
    assert_eq!(
        first, second,
        "row styling must not depend on wall-clock phase"
    );
    let id = first
        .spans
        .iter()
        .find(|span| span.content.contains("driven"))
        .expect("id span");
    // Cyan here used to seed a TachyonFX hue sweep. The sweep is gone, so an autopilot id reads
    // exactly like a standard one — the lane, the `[A]` tag and `control pmd` carry that fact.
    let standard = view("plain", Posture::Working, vec![]);
    let plain = project_row_line(&standard, 80, 0);
    let plain_id = plain
        .spans
        .iter()
        .find(|span| span.content.contains("plain"))
        .expect("id span");
    assert_eq!(id.style.fg, Some(attention::text()));
    assert_eq!(
        id.style.fg, plain_id.style.fg,
        "a driven row's id is coloured differently from a standard one"
    );
    // RGB in a row is EXPECTED now: every colour comes from the active opaline theme, which is a real
    // palette rather than the terminal's sixteen slots. What must not come back is a colour invented
    // at the call site, and `theme::text()` above is the assertion that it did not.
}

#[test]
fn the_cursor_follows_the_session_when_a_refresh_reorders_the_list() {
    // SEEN LIVE, one keystroke after the sections landed: `p` on the top row moved it into
    // PAUSED, the refresh re-sorted, and the cursor stayed on index 0 — which was now a
    // DIFFERENT session. A reflex second `p` would have paused that one.
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "zbot", Tier::Autopilot);
    // A second row that sorts ABOVE `zbot` once `zbot` is paused (paused is the last section),
    // so a stale index would land on it.
    let (_, _) = (
        {
            let mut reg = Registry::load(&reg_path).unwrap();
            reg.projects.push(ProjectEntry {
                id: "other".into(),
                display_name: None,
                root: root.clone(),
                enabled: true,
                mode: Mode::AgentLoop,
                engine: Some(Engine::Claude),
                worker_model: None,
                initial_prompt: None,
                task_title: None,
                forked_from: None,
                spawned_by: None,
                launch: None,
                conversation_id: None,
                cadence_s: Some(300),
            });
            reg.save(&reg_path).unwrap();
            seed_agent_loop(
                &ProjectPaths::for_session(&root, "other"),
                Tier::Autopilot,
                Engine::Claude,
                Engine::Claude,
                None,
                "goal",
                Some(300),
                1000,
            )
            .unwrap();
        },
        (),
    );

    let mut app = App::new(reg_path, "pm-test".into());
    let bot = app
        .projects
        .iter()
        .position(|v| v.id == "zbot")
        .expect("seeded");
    app.projects[bot].posture = Posture::NeedsYou;
    app.projects.sort_by(row_order);
    let bot = app
        .projects
        .iter()
        .position(|v| v.id == "zbot")
        .expect("attention row sorted first");
    app.selected = bot;

    handle_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);

    assert_eq!(
        app.selected_view().map(|v| v.id.as_str()),
        Some("zbot"),
        "the cursor must stay on the session the human acted on, not on its old index"
    );
    // …and it really did move in the list, so the assertion above is not vacuous.
    assert_ne!(app.selected, bot, "the row should have changed section");
}

#[test]
fn refresh_resets_preview_state_when_the_selected_session_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let (reg_path, root) = reg_with_tier(dir.path(), "removed", Tier::Standard);
    Registry::update(&reg_path, |registry| {
        registry.projects.push(ProjectEntry {
            id: "remaining".into(),
            display_name: None,
            root: root.clone(),
            enabled: true,
            mode: Mode::AgentLoop,
            engine: Some(Engine::Claude),
            worker_model: None,
            initial_prompt: None,
            task_title: None,
            forked_from: None,
            spawned_by: None,
            launch: None,
            conversation_id: None,
            cadence_s: None,
        });
    })
    .unwrap();
    seed_agent_loop(
        &ProjectPaths::for_session(&root, "remaining"),
        Tier::Standard,
        Engine::Claude,
        Engine::Claude,
        None,
        "",
        None,
        1_000,
    )
    .unwrap();

    let mut app = App::new(reg_path.clone(), "pm-test".into());
    app.selected = app
        .projects
        .iter()
        .position(|view| view.id == "removed")
        .expect("removed row selected");
    app.detail_scroll = 42;
    app.detail_max.set(120);
    app.pending_first_chat = Some("removed".into());
    app.armed_drains_left = Some(3);
    Registry::update(&reg_path, |registry| {
        registry.projects.retain(|entry| entry.id != "removed");
    })
    .unwrap();

    app.refresh();

    assert_eq!(
        app.selected_view().map(|view| view.id.as_str()),
        Some("remaining")
    );
    assert_eq!(app.detail_scroll, 0);
    assert_eq!(app.detail_max.get(), 0);
    assert!(app.pending_first_chat.is_none() && app.armed_drains_left.is_none());
}

#[test]
fn the_list_is_sectioned_by_attention_mode_and_availability() {
    // Asserted on the RENDERED rows, in order, because the whole feature is about what sits
    // above what — a `contains` over the flattened screen would pass on a list with the
    // headers in the wrong places or the rows under the wrong ones.
    let app = live_sectioned_app();
    let rows = session_rows(&app, 100, 20);
    let ix = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} never rendered: {rows:#?}"))
    };

    // Every section names itself, counts its rows, and says what it means.
    assert!(
        rows[ix("NEEDS YOU")].contains("(1) · action needed"),
        "{rows:#?}"
    );
    assert!(
        rows[ix("AUTOPILOT")].contains("(1) · pmd drives"),
        "{rows:#?}"
    );
    assert!(
        rows[ix("STANDARD")].contains("(1) · you drive"),
        "{rows:#?}"
    );
    assert!(
        rows[ix("PAUSED / OFFLINE")].contains("(1) · not running"),
        "{rows:#?}"
    );

    // Human attention comes first, then live Autopilot and Standard rows, then stopped rows.
    // The fixture is deliberately unsorted, so this also proves rendering does not trust the vec.
    assert!(ix("NEEDS YOU") < ix("asking"));
    assert!(ix("asking") < ix("AUTOPILOT"));
    assert!(ix("AUTOPILOT") < ix("driven"));
    assert!(ix("driven") < ix("STANDARD"));
    assert!(ix("STANDARD") < ix("mine"));
    assert!(ix("mine") < ix("PAUSED / OFFLINE"));
    assert!(ix("PAUSED / OFFLINE") < ix("stopped"));
    for header in ["NEEDS YOU", "AUTOPILOT", "STANDARD", "PAUSED / OFFLINE"] {
        assert_eq!(
            rows.iter().filter(|r| r.contains(header)).count(),
            1,
            "duplicate {header:?} header: {rows:#?}"
        );
    }
}

/// WHAT THE STACKED TIER STILL GUARANTEES. Every row is three lines now (identity, state, gap), so
/// four sessions want 18 rows where this tier allots 15 — "every section row visible" is no longer
/// achievable at this boundary and the list scrolls, which is what `j`/`k` are for.
///
/// What MUST still hold is the rule the product is shaped around: a row a human has to act on is
/// never one they must go looking for. So the NEEDS YOU rows and the selection are visible, and
/// the sections stay in their order.
#[test]
fn the_stacked_tier_keeps_attention_and_the_selection_visible_at_its_width_boundary() {
    let mut app = live_sectioned_app();
    app.selected = app
        .projects
        .iter()
        .position(|v| v.id == "driven")
        .expect("the fixture's autopilot row");
    let mut terminal = Terminal::new(TestBackend::new(NARROW_W, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(
        screen.contains("NEEDS YOU") && screen.contains("asking"),
        "an attention row must never be scrolled out of sight: {screen}"
    );
    assert!(
        screen.contains("driven"),
        "the selected row must be on screen: {screen}"
    );
}

#[test]
fn row_groups_keep_attention_first_and_put_dead_terminals_offline() {
    let mut v = view("session", Posture::Working, vec![]);
    v.mode = Mode::AgentLoop;
    v.tier = Some(Tier::Autopilot);
    v.session_live = true;
    assert_eq!(row_group(&v), RowGroup::Autopilot);

    v.posture = Posture::NeedsYou;
    assert_eq!(row_group(&v), RowGroup::NeedsYou);
    v.session_live = false;
    assert_eq!(
        row_group(&v),
        RowGroup::NeedsYou,
        "active human attention outranks terminal availability"
    );

    // A dead terminal is unavailable whatever its mode, so it leaves the routine groups for
    // PAUSED / OFFLINE while the row keeps its own mode tag.
    for (tier, tag) in [(Tier::Autopilot, "[A]"), (Tier::Standard, "[S]")] {
        v.tier = Some(tier);
        v.posture = Posture::Working;
        assert_eq!(row_group(&v), RowGroup::Paused, "{tag} offline row");
        let text = project_row_detail_text_at(&v, 39, 0, ROW_TEST_NOW);
        assert!(text.contains("offline") && text.contains(tag), "{text}");
    }
    v.tier = Some(Tier::Autopilot);

    let mut app = app_with(vec![v.clone()], UiMode::Normal);
    app.projects[0].id = "gone".into();
    let rows = session_rows(&app, 100, 20);
    let header = rows
        .iter()
        .position(|row| row.contains("PAUSED / OFFLINE (1)"))
        .unwrap_or_else(|| panic!("offline row not grouped: {rows:#?}"));
    assert!(
        rows[header + 1].contains("gone") && rows[header + 2].contains("offline"),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("AUTOPILOT")),
        "{rows:#?}"
    );

    v.enabled = false;
    v.posture = Posture::NeedsYou;
    assert_eq!(
        row_group(&v),
        RowGroup::Paused,
        "an explicit pause stays authoritative"
    );
}

#[test]
fn only_the_sections_that_have_rows_get_a_header() {
    // A header for an empty section would be a promise of rows that are not there, and on a
    // short pane it would spend a row saying nothing.
    let mut app = live_sectioned_app();
    app.projects.retain(|v| v.id == "mine");
    let rows = session_rows(&app, 100, 20);
    assert!(rows.iter().any(|r| r.contains("STANDARD (1)")), "{rows:#?}");
    for absent in ["NEEDS YOU", "AUTOPILOT", "PAUSED / OFFLINE"] {
        assert!(
            !rows.iter().any(|r| r.contains(absent)),
            "{absent} header drawn with no rows: {rows:#?}"
        );
    }
    // …and an empty list draws no headers at all.
    let empty = app_with(vec![], UiMode::Normal);
    let rows = session_rows(&empty, 100, 20);
    for absent in ["NEEDS YOU", "AUTOPILOT", "STANDARD", "PAUSED / OFFLINE"] {
        assert!(
            !rows.iter().any(|r| r.contains(absent)),
            "{absent} header on an empty list: {rows:#?}"
        );
    }
}

#[test]
fn the_gutter_lands_on_the_selected_row_not_a_header() {
    // The headers are LIST ITEMS, so `app.selected` (which indexes `projects`) is no longer
    // the item index — every header above the selection shifts it down. Get that mapping
    // wrong and the `▸` marks a header, or worse, the wrong session.
    let mut app = live_sectioned_app();
    for (i, expect) in [(0, "driven"), (1, "mine"), (2, "stopped"), (3, "asking")] {
        app.selected = i;
        let rows = session_rows(&app, 100, 20);
        let marked: Vec<&String> = rows.iter().filter(|r| r.contains('\u{25b8}')).collect();
        assert_eq!(marked.len(), 1, "exactly one gutter: {rows:#?}");
        assert!(
            marked[0].contains(expect),
            "selected {expect:?} but the gutter is on {:?}",
            marked[0]
        );
    }
}

/// EVERY ROW IS TWO LINES, AND THE SECOND ONE IS THE SEPARATOR. The blank line this started as
/// (user: *"i want the row have more space"*) now carries the row's state cluster, so a section's
/// rows never touch and the gap before the next header is the last row's own state line.
#[test]
fn every_row_is_followed_by_its_own_state_line() {
    let app = live_sectioned_app();
    let rows = session_rows(&app, 100, 40);
    let ids: Vec<String> = app.projects.iter().map(|v| v.id.clone()).collect();
    let is_row = |line: &String| ids.iter().any(|id| line.contains(id.as_str()));

    // No two identity lines are adjacent: each is followed by its own state line.
    for pair in rows.windows(2) {
        let [first, second] = pair else { continue };
        if is_row(first) && is_row(second) {
            panic!("two rows with no state line between: {first:?} then {second:?}");
        }
    }
    // A header is preceded by exactly ONE line that is not an identity line — the previous
    // section's last state line — so the sections are separated once, not twice.
    for (i, row) in rows.iter().enumerate() {
        if i > 0
            && (row.contains("AUTOPILOT") || row.contains("STANDARD") || row.contains("PAUSED"))
        {
            assert!(
                !is_row(&rows[i - 1]),
                "a header sits straight on a row: {row:?}"
            );
            assert!(
                i < 2 || !rows[i - 2].trim().is_empty(),
                "the gap above {row:?} is two lines"
            );
        }
    }
}

/// THE ROW FILLS THE WIDTH IT IS GIVEN, and the name is the field that absorbs the slack.
///
/// MEASURED before this: at the 52-column sidebar the row stopped six columns short of the pane,
/// because every field was fixed — so the right edge was never reached, the ages did not line up
/// down the list, and a depth-2 child elided `autopilot-docs` to `autopilot-d…` while those six
/// columns sat empty two fields to its right.
#[test]
fn the_row_spends_every_spare_column_on_the_name_and_ends_at_the_right_edge() {
    let mut v = view("autopilot-docs", Posture::Monitoring, vec![]);
    v.session_live = true;
    v.last_activity = Some(ROW_TEST_NOW - 60);

    // The whole row is exactly the width it was handed, at every width in the band…
    for content_w in 36..=46 {
        let text = project_row_text_at(&v, content_w);
        assert_eq!(
            text_cols(&text),
            usize::from(content_w),
            "a {content_w}-column row drew {:?}",
            text
        );
    }

    // …and the slack goes to the NAME, which is what stops an indented child from eliding.
    let narrow = project_row_line(&v, 39, 2);
    let wide = project_row_line(&v, 45, 2);
    let name_of = |line: &Line<'static>| -> String {
        line.spans
            .iter()
            .find(|s| s.content.contains("autopilot"))
            .map(|s| s.content.to_string())
            .expect("the name span")
    };
    assert!(
        text_cols(&name_of(&wide)) > text_cols(&name_of(&narrow)),
        "the extra width did not reach the name: {:?} vs {:?}",
        name_of(&narrow),
        name_of(&wide)
    );
    assert!(
        name_of(&wide).contains("autopilot-docs"),
        "a depth-2 child still elides at the cap: {:?}",
        name_of(&wide)
    );

    // THE STATE LINE ENDS WITH THE AGE, flush to the pane edge, which is what makes the ages a
    // column you can read down.
    let state = project_row_detail_text_at(&v, 41, 0, ROW_TEST_NOW);
    assert!(state.ends_with(" 1m"), "{state:?}");
}

/// THE SELECTED ROW'S BAND INCLUDES ITS GAP. Spaced rows turned the selection into a thin stripe
/// with air on both sides; the fill carries one row down so it reads as one block. The two lines
/// must cover the SAME columns, or the band looks like a misdrawn rectangle.
#[test]
fn the_selected_rows_band_covers_its_own_gap_to_the_same_width() {
    let app = live_sectioned_app();
    let selected = app.projects[app.selected].id.clone();
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let buf = terminal.backend().buffer();
    let sel_bg = agent_manager::theme::selection_bg();
    let filled = |y: u16| -> Vec<u16> {
        (buf.area.left()..buf.area.right())
            .filter(|x| buf.cell((*x, y)).is_some_and(|c| c.bg == sel_bg))
            .collect()
    };
    // BY THE GUTTER AND the id: the preview pane's own title names the selected session too, and
    // matching on the id alone found that border row and reported an empty band.
    let row_y = (buf.area.top()..buf.area.bottom())
        .find(|y| {
            let text: String = (buf.area.left()..buf.area.right())
                .map(|x| buf.cell((x, *y)).map_or(" ", |c| c.symbol()).to_string())
                .collect();
            text.contains(attention::GUTTER) && text.contains(&selected)
        })
        .expect("the selected row is on screen");

    let row = filled(row_y);
    let band = filled(row_y + 1);
    assert!(!row.is_empty(), "the selected row carries no fill");
    assert_eq!(
        row, band,
        "the band under the selected row must cover the same columns as the row"
    );
    // And it stops there: the row ABOVE the selection is not part of the band.
    assert!(
        filled(row_y - 1).is_empty(),
        "the fill leaked onto the line above the selected row"
    );
}

#[test]
fn click_hits_follow_rows_across_all_group_headers() {
    let app = live_sectioned_app();
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let screen = screen_rows(&terminal);
    let hits = app.row_hits.borrow();
    assert_eq!(hits.len(), app.projects.len(), "{hits:?}");
    for (y, project_index) in hits.iter().copied() {
        let row = &screen[usize::from(y)].0;
        assert!(
            row.contains(&app.projects[project_index].id),
            "row {y} maps to project {project_index}, but rendered {row:?}"
        );
    }
    for (y, (row, _)) in screen.iter().enumerate() {
        if ["NEEDS YOU", "AUTOPILOT", "STANDARD", "PAUSED / OFFLINE"]
            .iter()
            .any(|header| row.contains(header))
        {
            assert!(
                hits.iter().all(|(hit_y, _)| usize::from(*hit_y) != y),
                "header row {y} became clickable: {row:?}"
            );
        }
    }
}

#[test]
fn the_sort_and_the_screen_agree_on_row_order() {
    // ANTI-DRIFT across the one seam this feature has: `render_sessions` groups the rows it is
    // handed, and `refresh` sorts them — two independent orderings that MUST match, or `j`
    // walks the list in an order the screen does not show. (They are only linked by both
    // using `row_group`, which is exactly the kind of link that rots.)
    let app = live_sectioned_app();
    let mut sorted = app.projects.clone();
    sorted.sort_by(row_order);
    // The WHOLE pipeline `refresh` runs, not half of it: the lineage pass reorders within a group,
    // so a test that stopped at the sort would stop checking the thing most likely to drift.
    arrange_lineage(&mut sorted);
    let sorted_app = App {
        projects: sorted.clone(),
        ..live_sectioned_app()
    };
    let rows = session_rows(&sorted_app, 100, 20);
    let on_screen: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            sorted
                .iter()
                .find(|v| r.contains(v.id.as_str()))
                .map(|v| v.id.clone())
        })
        .collect();
    let by_sort: Vec<String> = sorted.iter().map(|v| v.id.clone()).collect();
    assert_eq!(
        on_screen, by_sort,
        "the sort order and the drawn order differ, so j/k would jump"
    );
}

#[test]
fn a_paused_row_counts_as_idle_not_as_running() {
    // Found by rendering the sectioned list: a session paused mid-`working` kept the green
    // `●` and was tallied under `● N running`, so the header claimed a process pause had
    // killed. The row's own label already said `paused`, which is what made the glyph a
    // visible contradiction rather than a subtle one.
    let mut v = view("stopped", Posture::Working, vec![]);
    v.mode = Mode::AgentLoop;
    v.tier = Some(Tier::Autopilot);
    assert_eq!(
        status_category(&v),
        1,
        "precondition: running while enabled"
    );
    v.enabled = false;
    assert_eq!(status_category(&v), 2, "a paused row is idle, not running");

    // And the same for the loudest counter: a paused row does not ask to be acted on.
    let mut needy = view(
        "stopped",
        Posture::NeedsYou,
        vec![stop("s1", "x", RiskClass::Low)],
    );
    needy.mode = Mode::AgentLoop;
    needy.enabled = false;
    assert_eq!(status_category(&needy), 2);
    // The row still says it is PAUSED (rather than reporting the posture of a process that is
    // not running); the stop it holds is spelled out in the preview, not shouted on the row.
    assert!(
        project_row_text(&needy).contains("paused"),
        "{}",
        project_row_text(&needy)
    );
}

#[test]
fn the_chat_chip_survives_beside_a_hard_stop() {
    // 44 columns is the real-tmux suite's own item width, and `NeedsYou` + a live chat is not
    // exotic — it is a human opening a chat to deal with the escalation. The fixed status field
    // and the admitted human chip must both survive at this width.
    let mut v = view(
        "x",
        Posture::NeedsYou,
        vec![stop("s1", "publish", RiskClass::Hard)],
    );
    v.human_attached = true;
    v.last_activity = Some(0);
    let tight = project_row_detail_text_at(&v, 44, 0, ROW_TEST_NOW);
    assert!(tight.contains("user"), "the chip was evicted: {tight:?}");
    assert!(
        tight.contains("needs you"),
        "the label must survive at 44 columns: {tight:?}"
    );
    // …and the row still fits the pane it was given.
    assert!(project_row_line(&v, 44, 0).width() <= 44);
    let line = project_row_detail(&v, 44, 0, ROW_TEST_NOW);
    let attached = line
        .spans
        .iter()
        .find(|span| span.content == " user")
        .expect("human-attached signal");
    assert_eq!(attached.style.fg, Some(agent_manager::theme::accent()));
    assert!(attached.style.add_modifier.contains(Modifier::BOLD));
    // Roomier: unchanged.
    let wide = project_row_detail_text_at(&v, 60, 0, ROW_TEST_NOW);
    assert!(
        wide.contains("user") && wide.contains("needs you"),
        "{wide:?}"
    );
}

#[test]
fn a_disabled_attention_row_recedes_as_paused() {
    let mut v = view("x", Posture::NeedsYou, vec![]);
    v.enabled = false;
    let line = project_row_detail(&v, 80, 0, ROW_TEST_NOW);
    let label = line
        .spans
        .iter()
        .find(|s| s.content.contains("paused"))
        .expect("no label span");
    // The receding label wears the theme's DIM role. It used to have no `fg` at all — the terminal's
    // own foreground plus `DIM` — which a theme cannot express: "quieter than body text" is a colour
    // the theme author chose, not an attribute the terminal picks.
    assert_eq!(label.style.fg, Some(agent_manager::theme::dim()));
    assert!(label.style.add_modifier.contains(Modifier::DIM));
    assert!(!label.style.add_modifier.contains(Modifier::REVERSED));
}

#[test]
fn a_hard_stop_paints_the_row_red_even_when_the_agent_declared_it_low() {
    // `publish` is forced Hard by `policy::effective_risk` whatever risk the agent asked for.
    let v = view(
        "x",
        Posture::NeedsYou,
        vec![stop("s1", "publish", RiskClass::Low)],
    );
    assert_eq!(attention::level(&v), attention::Level::Hard);
    let label = project_row_detail(&v, 80, 0, ROW_TEST_NOW)
        .spans
        .into_iter()
        .find(|s| s.content.contains("needs you"))
        .expect("the label span");
    assert_eq!(
        label.style.fg,
        Some(agent_manager::theme::hard()),
        "a forced-Hard row is red"
    );
    assert!(
        label.style.add_modifier.contains(Modifier::BOLD)
            && !label.style.add_modifier.contains(Modifier::REVERSED),
        "hard state should be strong text without a filled banner"
    );
}

#[test]
fn disabled_project_is_marked() {
    let mut v = view("x", Posture::Working, vec![]);
    v.enabled = false;
    // "paused" is the word (it read "(disabled)" while `enabled:false` was programmatic-only,
    // and `p`/`Enter` are why it changed), and the LABEL field is where it has to appear: it is
    // inside `ROW_FIXED_W`, so it survives the narrow widths where a trailing badge is not
    // drawn at all. Asserted at the real-tmux suite's own content width, which is what caught
    // the trailing version.
    assert!(project_row_detail_text_at(&v, 39, 0, ROW_TEST_NOW).contains("paused"));
    // …and it replaces the posture rather than sitting next to it: a paused row has no agent,
    // so "working" would be a claim about a process that is not running.
    assert!(!project_row_text(&v).contains("working"));
}

/// A disabled row the spawn broker still owes its one launch, as refresh marks it.
fn staged_view(id: &str) -> ProjectView {
    let mut v = agent_loop_view(id);
    v.enabled = false;
    v.spawn_staged = true;
    v.spawned_by = Some("parent".into());
    v.spawned_by_label = Some("parent".into());
    v
}

#[test]
fn a_staged_row_reads_starting_in_rows_and_board() {
    let staged = staged_view("kid");
    let row = project_row_text(&staged);
    assert!(row.contains(" starting… "), "{row}");
    assert!(!row.contains("paused"), "{row}");
    assert_eq!(
        row_group(&staged),
        RowGroup::Paused,
        "it sits with the paused rows"
    );
    assert_eq!(board_card_cue(&staged, ROW_TEST_NOW).0, "STARTING…");

    // An ordinary pause keeps its word on both surfaces.
    let mut paused = agent_loop_view("held");
    paused.enabled = false;
    assert!(project_row_text(&paused).contains(" paused "));
    assert_eq!(board_card_cue(&paused, ROW_TEST_NOW).0, "PAUSED");

    let mut app = app_with(vec![staged], UiMode::Board);
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    assert!(screen.contains("STARTING…"), "{screen}");
    assert_eq!(
        screen.matches("PAUSED").count(),
        1,
        "only the lane title says PAUSED: {screen}"
    );

    app.mode = UiMode::Normal;
    let rows = session_rows(&app, 100, 20);
    let at = rows
        .iter()
        .position(|row| row.contains("kid"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(rows[at + 1].contains("starting…"), "{rows:#?}");
}

#[test]
fn refresh_marks_a_staged_spawn_row_and_never_as_an_incomplete_fork() {
    use agent_manager::registry::{LaunchRecord, LaunchState};

    let dir = tempfile::tempdir().unwrap();
    let (registry, _root) = reg_with_agent_loop(dir.path(), "kid");
    let staged = |state: LaunchState| {
        Registry::update(&registry, |registry| {
            let row = &mut registry.projects[0];
            row.enabled = false;
            row.spawned_by = Some("parent".into());
            // A hand-edited registry could carry fork lineage too; staging still wins.
            row.forked_from = Some("parent".into());
            row.launch = Some(LaunchRecord {
                request_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                args_hash: "hash".into(),
                state,
                outcome: None,
                kind: agent_manager::registry::LaunchKind::Chat,
                branch: None,
                base_commit: None,
            });
        })
        .unwrap();
        let mut app = app_with(Vec::new(), UiMode::Normal);
        app.registry_path = registry.clone();
        app.refresh();
        app.projects.remove(0)
    };

    let view = staged(LaunchState::Pending);
    assert!(view.spawn_staged && !view.incomplete_fork, "{view:?}");
    let row = project_row_text(&view);
    assert!(row.contains("starting…"), "{row}");

    let view = staged(LaunchState::Started);
    assert!(!view.spawn_staged, "a started launch is an ordinary row");
    assert!(project_row_text(&view).contains("paused"));
}

#[test]
fn session_row_head_width_is_unchanged_for_spawned_rows() {
    // Up to ~125 terminal columns the list pane sits at its 44-column floor, which leaves the
    // row exactly its fixed head. Lineage has no slot there, and `starting…` fits the status
    // field `paused` fills.
    let content_w = sessions_width(100).saturating_sub(8);
    assert_eq!(content_w, 36, "the 44-column sidebar's inner width");

    let mut plain = agent_loop_view("child");
    plain.session_live = true;
    let mut spawned = plain.clone();
    spawned.spawned_by = Some("parent-with-a-very-long-session-id".into());
    spawned.spawned_by_label = Some("發布協調工作流程 parent label that is far too long".into());
    assert_eq!(
        project_row_line(&spawned, content_w, 0),
        project_row_line(&plain, content_w, 0),
        "spawn lineage must not change the Session row"
    );

    let mut paused = agent_loop_view("child");
    paused.enabled = false;
    for v in [&spawned, &staged_view("child"), &paused] {
        let row = project_row_line(v, content_w, 0);
        assert_eq!(row.width(), usize::from(content_w), "{row:?}");
    }

    let app = app_with(vec![spawned, staged_view("kid")], UiMode::Normal);
    let rows = session_rows(&app, 100, 20);
    let at = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing: {rows:#?}"))
    };
    let (child, kid) = (at("child"), at("kid"));
    assert!(!rows.iter().any(|row| row.contains("parent")), "{rows:#?}");
    // The mode tag is on each row's STATE line, and a staged row's reads `starting…` where a
    // paused one reads `paused` — neither costs the identity line a column.
    assert!(rows[child + 1].contains("[S]"), "{:?}", rows[child + 1]);
    assert!(rows[kid + 1].contains("[S]"), "{:?}", rows[kid + 1]);
    // Built rather than scraped: `session_rows` trims each line, which removes exactly the name
    // padding that makes two identity lines the same width.
    assert_eq!(
        project_row_line(&app.projects[0], content_w, 0).width(),
        project_row_line(&app.projects[1], content_w, 0).width(),
        "the IDENTITY lines must be the same width whatever lineage a row carries: {rows:#?}"
    );
}

/// A lineage fixture in ONE group: `api-migration` roots a tree, `release-prep` stands alone.
///
/// `row_order` sorts these by label inside a group, so the sorted order is api-migration, codegen,
/// docs-pass, release-prep, schema-split — deliberately interleaved, so an arrangement that merely
/// preserved the sort would fail every assertion below.
fn lineage_views() -> Vec<ProjectView> {
    let child = |id: &str, parent: Option<&str>| {
        let mut v = view(id, Posture::Working, vec![]);
        v.mode = Mode::AgentLoop;
        v.tier = Some(Tier::Autopilot);
        v.session_live = true;
        v.spawned_by = parent.map(str::to_string);
        v
    };
    let mut views = vec![
        child("api-migration", None),
        child("schema-split", Some("api-migration")),
        child("codegen", Some("schema-split")),
        child("docs-pass", Some("api-migration")),
        child("release-prep", None),
    ];
    views.sort_by(row_order);
    views
}

fn ids(views: &[ProjectView]) -> Vec<&str> {
    views.iter().map(|v| v.id.as_str()).collect()
}

/// EACH CHILD DIRECTLY AFTER ITS OWN PARENT, depth first, and a root's own place is the one the
/// sort gave it. This is the order `j`/`k` walks, so it is also the order the list draws.
#[test]
fn the_tree_reads_as_the_work_was_structured() {
    let mut views = lineage_views();
    assert_eq!(
        ids(&views),
        [
            "api-migration",
            "codegen",
            "docs-pass",
            "release-prep",
            "schema-split"
        ],
        "the sort alone interleaves the tree"
    );

    arrange_lineage(&mut views);

    assert_eq!(
        ids(&views),
        [
            "api-migration",
            "docs-pass",
            "schema-split",
            "codegen",
            "release-prep"
        ],
        "a parent is followed by its descendants, each in the order the sort gave them"
    );
    let members: Vec<usize> = (0..views.len()).collect();
    assert_eq!(
        views
            .iter()
            .enumerate()
            .map(|(i, _)| row_depth(&views, &members, i))
            .collect::<Vec<_>>(),
        [0, 1, 1, 2, 0],
        "and the drawn depth is the distance to its root"
    );
}

/// URGENCY OUTRANKS LINEAGE, and this is the assertion that says so. A needy child is drawn at the
/// top of `NEEDS YOU`, flush, rather than indented under a parent that is in another section — a
/// row folded under a parent elsewhere is a row a human has to go looking for, and telling them
/// which agent needs them is the product's one job. Lineage stays a CUE there: `from <parent>` is
/// in the preview head.
#[test]
fn a_needy_child_is_never_indented_under_a_parent_in_another_section() {
    let mut parent = view("api-migration", Posture::Working, vec![]);
    parent.mode = Mode::AgentLoop;
    parent.tier = Some(Tier::Autopilot);
    parent.session_live = true;
    let mut needy = view(
        "docs-pass",
        Posture::NeedsYou,
        vec![stop("s1", "publish", RiskClass::Hard)],
    );
    needy.mode = Mode::AgentLoop;
    needy.tier = Some(Tier::Autopilot);
    needy.session_live = true;
    needy.spawned_by = Some("api-migration".into());

    let mut views = vec![parent, needy];
    views.sort_by(row_order);
    arrange_lineage(&mut views);

    // Different groups, so the child roots its own section.
    assert_eq!(ids(&views), ["docs-pass", "api-migration"]);
    let needy_at = 0;
    let members = vec![needy_at];
    assert_eq!(
        row_depth(&views, &members, needy_at),
        0,
        "an ancestor outside the group does not indent the row"
    );

    let app = App {
        projects: views,
        ..app_with(Vec::new(), UiMode::Normal)
    };
    let rows = session_rows(&app, 100, 20);
    let drawn = rows
        .iter()
        .find(|row| row.contains("docs-pass"))
        .unwrap_or_else(|| panic!("the needy child was not drawn: {rows:?}"));
    assert!(
        !drawn.contains('\u{2514}'),
        "it must be drawn flush, with no tie: {drawn:?}"
    );
}

/// THE INDENT IS PAID OUT OF THE NAME FIELD. `ROW_FIXED_W` is what every trailing item is admitted
/// against, and that admission has shipped two regressions of its own — a badge no human could see,
/// and a row that kept the age while dropping the chip saying why nothing was advancing. A tie that
/// widened the head would move all of those thresholds at once.
#[test]
fn the_tie_costs_name_columns_and_never_row_columns() {
    let mut v = view("a-very-long-session-name", Posture::Working, vec![]);
    v.mode = Mode::AgentLoop;
    v.tier = Some(Tier::Autopilot);
    v.session_live = true;

    let flush = project_row_line(&v, 39, 0);
    for depth in 1..=ROW_MAX_INDENT {
        let tied = project_row_line(&v, 39, depth);
        assert_eq!(
            tied.width(),
            flush.width(),
            "depth {depth} changed the row's width"
        );
        let text: String = tied.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.starts_with(&format!(
                "{}\u{2514} ",
                " ".repeat((depth - 1) * ROW_INDENT_W)
            )),
            "the tie leads the row at depth {depth}: {text:?}"
        );
        // The tag lives on the STATE line, which carries the same indent so the two lines stay
        // one block.
        let detail = project_row_detail_text_at(&v, 39, depth, ROW_TEST_NOW);
        assert!(detail.contains("[A]"), "{detail:?}");
        assert!(
            detail.starts_with(&" ".repeat(ROW_DETAIL_INDENT as usize + depth * ROW_INDENT_W)),
            "the state line hangs under the name at depth {depth}: {detail:?}"
        );
    }

    // Deeper than the cap draws the cap, rather than eating more of the name.
    let capped: String = project_row_line(&v, ROW_FIXED_W, 9)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    let at_cap: String = project_row_line(&v, ROW_FIXED_W, ROW_MAX_INDENT)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(capped, at_cap);
}

/// DEFENSIVE, and cheap. Lineage cannot point in a circle today — a child names a row that already
/// existed — but an arrangement that dropped or repeated a row would silently desync `j`/`k` from
/// the screen, so the pass is written to place every row exactly once whatever the data says.
#[test]
fn a_lineage_that_points_in_a_circle_still_places_every_row_once() {
    let mut a = view("a", Posture::Working, vec![]);
    a.mode = Mode::AgentLoop;
    a.tier = Some(Tier::Autopilot);
    a.session_live = true;
    a.spawned_by = Some("b".into());
    let mut b = view("b", Posture::Working, vec![]);
    b.mode = Mode::AgentLoop;
    b.tier = Some(Tier::Autopilot);
    b.session_live = true;
    b.spawned_by = Some("a".into());

    let mut views = vec![a, b];
    arrange_lineage(&mut views);

    let mut drawn = ids(&views);
    drawn.sort_unstable();
    assert_eq!(drawn, ["a", "b"], "every row, exactly once");
}
