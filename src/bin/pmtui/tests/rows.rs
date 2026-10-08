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
    let autopilot = project_row_line(&v, ROW_FIXED_W, 1_000);
    let tag = autopilot
        .spans
        .iter()
        .find(|span| span.content.contains("[A]"))
        .expect("autopilot tag");
    assert_eq!(tag.style.fg, Some(agent_manager::theme::accent_alt()));
    assert!(tag.style.add_modifier.contains(Modifier::BOLD));

    v.tier = Some(Tier::Standard);
    let standard = project_row_line(&v, ROW_FIXED_W, 1_000);
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
    // `last_activity` was already loaded and thrown away; the row surfaces it. And it
    // is the FIRST thing shed when the LIST PANE runs out of columns, because the age
    // is context while identity, status, and mode are required.
    let mut v = view("auth-rewrite", Posture::NeedsYou, vec![]);
    v.last_activity = Some(1_000_000);
    let now = 1_000_000 + 2 * 3_600; // 2h ago

    let wide = project_row_text_at(&v, 80, now);
    assert!(wide.contains("2h"), "no age on a roomy row: {wide}");

    // Exactly at the threshold it is still shown, one column below it is gone —
    // and everything that is NOT the age survives either way.
    assert!(project_row_text_at(&v, ROW_AGE_W, now).contains("2h"));
    let narrow = project_row_text_at(&v, ROW_AGE_W - 1, now);
    assert!(
        !narrow.contains("2h"),
        "age not shed on a narrow pane: {narrow}"
    );
    for keep in ["auth-rewrite", "needs you", "[A]"] {
        assert!(narrow.contains(keep), "{keep:?} shed instead: {narrow}");
    }
}

/// THE AGE MUST NEVER EVICT THE `chat` CHIP. The row is rendered with no wrap, so
/// ratatui cuts the TAIL — and the age was originally pushed AHEAD of the trailing
/// chips, so an overflowing row kept a nice-to-have and dropped the one chip that
/// explains why a session is not advancing. That shipped, and only the real-tmux
/// `chat_chip_says_the_poll_is_parked` caught it; no unit test did. This is that unit
/// test: at a width that fits the chip but not chip-plus-age, the AGE is what goes.
#[test]
fn the_age_column_is_shed_before_the_chat_chip_not_after() {
    let mut v = view("auth-rewrite", Posture::Monitoring, vec![]);
    v.last_activity = Some(1_000_000);
    v.human_attached = true;
    let now = 1_000_000 + 2 * 3_600; // 2h ago

    // Roomy: both fit.
    let wide = project_row_text_at(&v, 200, now);
    assert!(
        wide.contains("user") && wide.contains("2h"),
        "a roomy row shows both: {wide}"
    );

    // A width that fits the CHIP but not chip-plus-age. Anchored on the chip's own cost
    // rather than on `ROW_AGE_W`: the chip wins, and the age is bought only if it does not
    // evict it.
    let tight = project_row_text_at(&v, ROW_FIXED_W + ROW_CHAT_W, now);
    assert!(
        tight.contains("user"),
        "the human-attached chip must survive: {tight}"
    );
    assert!(
        !tight.contains("2h"),
        "the AGE is what sheds when the chip needs the columns: {tight}"
    );

    // And the row still fits the pane it was given, chip included.
    let row = project_row_line(&v, ROW_AGE_W, now);
    assert!(
        row.width() <= usize::from(ROW_AGE_W),
        "row is {} cols wide at a {ROW_AGE_W}-col pane",
        row.width()
    );
}

#[test]
fn row_age_never_widens_the_row_past_its_pane() {
    // The threshold's arithmetic, asserted rather than trusted: with the age shown,
    // an attention row still fits `ROW_AGE_W` columns.
    let mut v = view("sixteen-char-idx", Posture::Stuck, vec![]);
    v.last_activity = Some(0);
    v.stops = vec![stop("s1", "publish", RiskClass::Hard)];
    let row = project_row_line(&v, ROW_AGE_W, 51 * 7 * 86_400); // widest age: `51w`
    assert!(
        row.width() <= usize::from(ROW_AGE_W),
        "row is {} wide: {row:?}",
        row.width()
    );
    let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("51w") && text.contains("stuck"), "{text}");
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

    // 43 cols: 39 inner, below the threshold — the age goes, the row does not.
    let mut terminal = Terminal::new(TestBackend::new(43, 12)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render 43");
    let tight = screen_text(&terminal);
    assert!(!tight.contains('—'), "age not shed at 43 cols: {tight}");
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
    let id = project_row_line(&asking, 80, 1_000)
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
    let attention = project_row_line(&view("x", Posture::NeedsYou, vec![]), 80, 1_000)
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
        format!(" {:<13}", "needs you"),
        "attention must keep the fixed 14-column status field"
    );

    let mut calm_view = view("y", Posture::Monitoring, vec![]);
    calm_view.session_live = true;
    let calm = project_row_line(&calm_view, 80, 1_000)
        .spans
        .into_iter()
        .find(|s| s.content.contains("monitoring"))
        .expect("the calm label span");
    assert!(!calm.style.add_modifier.contains(Modifier::REVERSED));
    assert!(
        calm.content.starts_with(' ') && !calm.content.starts_with("  "),
        "a calm label should stay left-aligned: {:?}",
        calm.content
    );
}

#[test]
fn hard_and_soft_rows_use_restrained_semantic_text() {
    let mut app = app_with(
        vec![
            view("asking", Posture::NeedsYou, vec![]),
            view("wedged", Posture::Stuck, vec![]),
        ],
        UiMode::Normal,
    );
    app.selected = 0;
    let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
    terminal.draw(|f| render(f, &app)).expect("render");
    let hard = styles_under_row(&terminal, "wedged", "stuck").expect("no stuck row");
    assert!(
        hard.iter()
            .all(|(fg, m)| *fg == agent_manager::theme::hard()
                && m.contains(Modifier::BOLD)
                && !m.contains(Modifier::REVERSED))
    );
    let soft = styles_under_row(&terminal, "asking", "needs you").expect("no needs-you row");
    assert!(
        soft.iter()
            .all(|(fg, m)| *fg == agent_manager::theme::soft()
                && m.contains(Modifier::BOLD)
                && !m.contains(Modifier::REVERSED))
    );
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
            let row = project_row_line(&v, w, 1_000);
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
    assert_eq!(content_w, ROW_FIXED_W);
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
            let row = project_row_line(&v, content_w, 1_000);
            let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
            assert_eq!(row.width(), usize::from(ROW_FIXED_W), "{text}");
            assert!(text.contains(" needs you "), "{text}");
            assert!(text.ends_with(tag), "{text}");
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
    let row = rows
        .iter()
        .find(|row| row.contains('发'))
        .unwrap_or_else(|| panic!("wide name never rendered: {rows:#?}"));
    assert!(row.contains("needs you") && row.contains("[A]"), "{row}");
}

#[test]
fn a_driven_row_wears_no_special_colour_on_its_id() {
    let mut v = view("driven", Posture::Working, vec![]);
    v.tier = Some(Tier::Autopilot);
    v.session_live = true;
    let first = project_row_line(&v, 80, 1_000);
    let second = project_row_line(&v, 80, 1_000);
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
    let plain = project_row_line(&standard, 80, 1_000);
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

#[test]
fn stacked_layout_keeps_every_section_row_visible_at_its_width_boundary() {
    let app = live_sectioned_app();
    let mut terminal = Terminal::new(TestBackend::new(NARROW_W, 24)).unwrap();
    terminal.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&terminal);
    for id in ["asking", "driven", "mine", "stopped"] {
        assert!(
            screen.contains(id),
            "{id:?} hidden at {NARROW_W}x24: {screen}"
        );
    }
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
        let offline = project_row_line(&v, ROW_FIXED_W, 1_000);
        let text: String = offline
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
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
        rows[header + 1].contains("gone") && rows[header + 1].contains("offline"),
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
    let tight = project_row_text_at(&v, 44, 1_000);
    assert!(tight.contains("user"), "the chip was evicted: {tight:?}");
    assert!(
        tight.contains("needs you"),
        "the label must survive at 44 columns: {tight:?}"
    );
    // …and the row still fits the pane it was given.
    assert!(project_row_line(&v, 44, 1_000).width() <= 44);
    let line = project_row_line(&v, 44, 1_000);
    let attached = line
        .spans
        .iter()
        .find(|span| span.content == " user")
        .expect("human-attached signal");
    assert_eq!(attached.style.fg, Some(agent_manager::theme::accent()));
    assert!(attached.style.add_modifier.contains(Modifier::BOLD));
    // Roomier: unchanged.
    let wide = project_row_text_at(&v, 60, 1_000);
    assert!(
        wide.contains("user") && wide.contains("needs you"),
        "{wide:?}"
    );
}

#[test]
fn a_disabled_attention_row_recedes_as_paused() {
    let mut v = view("x", Posture::NeedsYou, vec![]);
    v.enabled = false;
    let line = project_row_line(&v, 80, 1_000);
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
    let label = project_row_line(&v, 80, 1_000)
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
    assert!(project_row_text_at(&v, ROW_FIXED_W, 1_000).contains("paused"));
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
    assert!(
        rows.iter()
            .any(|row| row.contains("kid") && row.contains("starting…")),
        "{rows:#?}"
    );
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
    assert_eq!(content_w, ROW_FIXED_W);

    let mut plain = agent_loop_view("child");
    plain.session_live = true;
    let mut spawned = plain.clone();
    spawned.spawned_by = Some("parent-with-a-very-long-session-id".into());
    spawned.spawned_by_label = Some("發布協調工作流程 parent label that is far too long".into());
    assert_eq!(
        project_row_line(&spawned, content_w, 1_000),
        project_row_line(&plain, content_w, 1_000),
        "spawn lineage must not change the Session row"
    );

    let mut paused = agent_loop_view("child");
    paused.enabled = false;
    for v in [&spawned, &staged_view("child"), &paused] {
        let row = project_row_line(v, content_w, 1_000);
        assert_eq!(row.width(), usize::from(ROW_FIXED_W), "{row:?}");
    }

    let app = app_with(vec![spawned, staged_view("kid")], UiMode::Normal);
    let rows = session_rows(&app, 100, 20);
    let child = rows.iter().find(|row| row.contains("child")).unwrap();
    let kid = rows.iter().find(|row| row.contains("kid")).unwrap();
    assert!(!rows.iter().any(|row| row.contains("parent")), "{rows:#?}");
    assert!(child.trim_end().ends_with("[S]"), "{child}");
    assert!(kid.trim_end().ends_with("[S]"), "{kid}");
    assert_eq!(
        text_cols(child.trim_end()),
        text_cols(kid.trim_end()),
        "{rows:#?}"
    );
}
