//! The create form's Directory completion: the candidate list under the row, picking from it, and
//! what `Tab`, `→` and `Esc` do with a pick and a ghost. The completion rules themselves live in
//! `path_complete.rs`'s tests — these cover the form state, the keys and the render, which is where
//! a completion feature usually breaks: by stealing a key that had another job.

use super::*;

/// A create-form app focused on the Directory row, with `dir` set to `typed` and the completion
/// already refreshed — the state one keystroke in that field leaves behind.
fn dir_app(reg: &Path, typed: &str) -> App {
    let mut app = creating_loop_app(
        reg,
        Path::new("/"),
        Engine::Claude,
        Tier::Standard,
        "g",
        300,
    );
    let form = form_of(&mut app);
    form.field = CreateForm::DIRECTORY;
    form.dir = typed.to_string().into();
    form.caret_end();
    form.refresh_dir_completion();
    app
}

fn form_of(app: &mut App) -> &mut CreateForm {
    let UiMode::Creating(form) = &mut app.mode else {
        unreachable!()
    };
    form
}

#[test]
fn the_row_lists_what_is_there_before_anything_is_typed() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    // The form opens on a real directory, so the list has something to say immediately: the whole
    // point of a list rather than only a ghost is answering "what is here" without typing a word.
    let mut app = dir_app(&d.path().join("reg.json"), &d.path().display().to_string());
    let root = d.path().display();
    let form = form_of(&mut app);
    // FULL PATHS, not bare names: a candidate's value is what the field would become, and that is
    // what the human compares against the path they are editing.
    assert_eq!(
        form.dir_option_paths(),
        [
            format!("{root}/project-alpha/"),
            format!("{root}/project-beta/")
        ]
    );

    // But there is no GHOST on a path that already names a directory — the only completion left is
    // the separator, and a ghost there would spend the Tab that was about to leave the row.
    assert_eq!(form.dir_ghost(), None);
    handle_create_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        form_of(&mut app).field,
        CreateForm::NAME,
        "Tab out of a complete path must still be plain navigation"
    );
}

#[test]
fn the_list_belongs_to_the_focused_row_only() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha")).unwrap();
    let mut app = dir_app(&d.path().join("reg.json"), &d.path().display().to_string());
    let form = form_of(&mut app);
    assert!(!form.dir_option_paths().is_empty());
    // Another row's focus is not this row's: the list expands into the card's ONE reserved area,
    // which a focused Model row also uses, so both must never claim it at once.
    form.field = CreateForm::WORKER_MODEL;
    assert!(form.dir_option_paths().is_empty());
    // A pick is the focused row's too, so no key can act on one left behind here.
    form.field = CreateForm::DIRECTORY;
    assert!(form.move_dir_pick(true));
    form.field = CreateForm::WORKER_MODEL;
    assert_eq!(form.dir_pick(), None);
    assert!(!form.move_dir_pick(true), "no list to move in off the row");
}

#[test]
fn typing_narrows_the_list_and_raises_a_ghost() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta", "zebra"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let root = d.path().display().to_string();
    let mut app = dir_app(&d.path().join("reg.json"), &format!("{root}/"));
    assert_eq!(form_of(&mut app).dir_option_paths().len(), 3);

    for c in "pro".chars() {
        handle_create_key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    let form = form_of(&mut app);
    assert_eq!(
        form.dir_option_paths(),
        [
            format!("{root}/project-alpha/"),
            format!("{root}/project-beta/")
        ]
    );
    assert_eq!(form.dir_ghost(), Some("ject-"));
}

#[test]
fn a_ghost_needs_the_directory_row_focused_and_the_caret_at_the_end() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha")).unwrap();
    let mut app = dir_app(
        &d.path().join("reg.json"),
        &format!("{}/proj", d.path().display()),
    );
    let form = form_of(&mut app);
    assert_eq!(form.dir_ghost(), Some("ect-alpha/"));

    // Editing in the MIDDLE of a path offers nothing: a ghost is only ever drawn at the end of the
    // line, so accepting one there would insert text where no suggestion was shown.
    form.caret_left();
    assert_eq!(form.dir_ghost(), None);
    form.caret_end();
    assert_eq!(form.dir_ghost(), Some("ect-alpha/"));

    form.field = CreateForm::NAME;
    assert_eq!(form.dir_ghost(), None);
}

#[test]
fn the_completion_is_recomputed_only_when_the_text_changed() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha")).unwrap();
    let mut app = dir_app(
        &d.path().join("reg.json"),
        &format!("{}/proj", d.path().display()),
    );
    let form = form_of(&mut app);

    // A directory created AFTER the cache was filled must not change the live suggestion: proof
    // the refresh is guarded on the text and the renderer is not reading the filesystem.
    std::fs::create_dir_all(d.path().join("project-beta")).unwrap();
    form.refresh_dir_completion();
    assert_eq!(form.dir_ghost(), Some("ect-alpha/"));

    // One keystroke changes the text, so the next refresh does see both siblings.
    form.backspace();
    form.refresh_dir_completion();
    assert_eq!(form.dir_ghost(), Some("ject-"));
}

#[test]
fn right_completes_the_path_and_descends_one_level_per_press() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha/inner")).unwrap();
    let root = d.path().display().to_string();
    let mut app = dir_app(&d.path().join("reg.json"), &format!("{root}/proj"));

    handle_create_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let form = form_of(&mut app);
    assert_eq!(form.dir.as_str(), format!("{root}/project-alpha/"));
    assert_eq!(form.field, CreateForm::DIRECTORY);
    // The accepted completion left a trailing separator, so the single child below is the next
    // offer — one more press descends.
    assert_eq!(form.dir_ghost(), Some("inner/"));

    handle_create_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let form = form_of(&mut app);
    assert_eq!(form.dir.as_str(), format!("{root}/project-alpha/inner/"));
    // `inner` is empty, so the chain terminates and `→` is a caret move again — it has nothing left
    // to take, and the caret is already at the end.
    assert_eq!(form.dir_ghost(), None);
}

#[test]
fn tab_advances_from_every_row_including_the_toggles() {
    let d = tempfile::tempdir().unwrap();
    let mut app = dir_app(&d.path().join("reg.json"), "/no/such/path/at/all");
    handle_create_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).field, CreateForm::NAME);

    let mut app = dir_app(&d.path().join("reg2.json"), "/tmp");
    form_of(&mut app).field = CreateForm::ENGINE;
    handle_create_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).field, CreateForm::WORKER_MODEL);
}

#[test]
fn right_accepts_the_ghost_and_otherwise_still_moves_the_caret() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha")).unwrap();
    let root = d.path().display().to_string();
    let mut app = dir_app(&d.path().join("reg.json"), &format!("{root}/proj"));

    handle_create_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        form_of(&mut app).dir.as_str(),
        format!("{root}/project-alpha/")
    );

    // With the caret off the end there is no ghost, so `→` is a caret move again — nothing about
    // the completion took the key away from its own job.
    let form = form_of(&mut app);
    form.caret_home();
    let before = form.dir.caret();
    handle_create_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir.caret(), before + 1);
}

#[test]
fn a_pasted_path_gets_a_completion_too() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha")).unwrap();
    let root = d.path().display().to_string();
    let mut app = dir_app(&d.path().join("reg.json"), "");

    // A paste does not arrive through `handle_create_key`, so it needs its own refresh — without
    // one, the most likely way to get a long path into the field would be the one that offers
    // nothing.
    handle_paste(&mut app, &format!("{root}/proj"));
    assert_eq!(form_of(&mut app).dir_ghost(), Some("ect-alpha/"));
}

#[test]
fn opening_the_form_fills_the_list_before_any_key() {
    let d = tempfile::tempdir().unwrap();
    let (registry, root) = reg_with_agent_loop(d.path(), "bot");
    std::fs::create_dir_all(root.join("child-dir")).unwrap();
    let mut app = app_with_driver(
        vec![agent_loop_view("bot")],
        UiMode::Board,
        Box::new(FakePane::default()),
    );
    app.registry_path = registry;
    app.model_catalog.insert(Engine::Claude, Vec::new());

    // The Task Board opens the form on the selected session's root. A CLICK can focus the Directory
    // row without any key reaching the key handler, so the list is filled when the form opens —
    // beside the model catalogs, which are eager for exactly the same reason.
    app.begin_task_create();
    let form = form_of(&mut app);
    form.field = CreateForm::DIRECTORY;
    assert_eq!(
        form.dir_option_paths(),
        [format!("{}/child-dir/", root.display())]
    );
}

#[test]
fn the_ghost_renders_after_the_caret_in_dim_above_the_candidate_list() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let mut app = dir_app(
        &d.path().join("reg.json"),
        &format!("{}/proj", d.path().display()),
    );
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let form = form_of(&mut app).clone();
    t.draw(|fr| render_create(fr, fr.area(), &form)).unwrap();

    let text = screen_text(&t);
    assert!(
        text.contains("project-"),
        "the suggestion is drawn after what was typed:\n{text}"
    );
    // The suggestion's FIRST cell is the caret, so the caret stays visible ON the offer.
    let ghost = styles_under_row(&t, "Directory", "ect-").expect("ghost");
    assert!(
        ghost[0].1.contains(Modifier::REVERSED),
        "the first ghost cell carries the caret: {ghost:?}"
    );
    // Everything after it is DIM — the whole point: visibly not typed yet.
    assert!(
        ghost[1..].iter().all(|(_, m)| m.contains(Modifier::DIM)),
        "the rest of the ghost is dim: {ghost:?}"
    );
    // And both candidates are listed below, dim, with the trailing `/` that says each is a
    // directory.
    assert!(text.contains("project-alpha/"), "{text}");
    assert!(text.contains("project-beta/"), "{text}");
    let row = styles_under_row(&t, "project-beta/", "project-beta/").expect("list row");
    assert!(
        row.iter().all(|(_, m)| m.contains(Modifier::DIM)),
        "{row:?}"
    );

    assert!(
        text.contains("\u{2192} complete"),
        "with nothing picked the row advertises completing, not taking:\n{text}"
    );

    // Another row advertises ITS key instead — the two chips do not fit together, and a hint wider
    // than the card is dropped whole rather than clipped.
    let mut other = form.clone();
    other.field = CreateForm::GOAL;
    t.draw(|fr| render_create(fr, fr.area(), &other)).unwrap();
    let text = screen_text(&t);
    assert!(text.contains("^E $EDITOR"), "{text}");
    assert!(!text.contains("\u{2192} complete"), "{text}");
    assert!(
        !text.contains("project-beta/"),
        "the list belongs to the focused row:\n{text}"
    );
}

#[test]
fn arrows_pick_a_candidate_and_still_move_between_fields_elsewhere() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let mut app = dir_app(&d.path().join("reg.json"), &d.path().display().to_string());
    assert_eq!(
        form_of(&mut app).dir_pick(),
        None,
        "nothing is picked on arrival"
    );

    // `↓` enters the list at the top and `↑` at the bottom, and both wrap — the same as every
    // other list in the dashboard.
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(0));
    assert_eq!(
        form_of(&mut app).field,
        CreateForm::DIRECTORY,
        "an arrow spent on the list must not also change field"
    );
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(1));
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(0), "wraps");
    handle_create_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(1));

    // Shift+Tab is the way OFF the row while the list has the arrows.
    handle_create_key(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).field, CreateForm::WORKER_MODEL);

    // And on a row with no list the arrows are plain field navigation, exactly as before.
    let mut app = dir_app(&d.path().join("reg2.json"), "/no/such/path");
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).field, CreateForm::NAME);
    handle_create_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).field, CreateForm::DIRECTORY);
}

#[test]
fn tab_always_leaves_the_row_whatever_is_offered() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let root = d.path().display().to_string();
    // Typed `pro`, so there is a ghost (`ject-`) AND a list AND a pick — every reason this row
    // could have to eat the key. It must not: `Tab` is the form's one unconditional exit. Two
    // CONDITIONAL exits is what trapped the human here, which is the bug this test pins.
    let mut app = dir_app(&d.path().join("reg.json"), &format!("{root}/pro"));
    assert_eq!(form_of(&mut app).dir_ghost(), Some("ject-"));
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(1));

    handle_create_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    let form = form_of(&mut app);
    assert_eq!(form.field, CreateForm::NAME);
    assert_eq!(
        form.dir.as_str(),
        format!("{root}/pro"),
        "leaving must not silently commit a candidate or a completion"
    );
}

#[test]
fn enter_takes_the_pick_instead_of_creating_the_session() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let root = d.path().display().to_string();
    let mut app = dir_app(&d.path().join("reg.json"), &d.path().display().to_string());
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);

    // A pick is a sublayer with its own Enter. Submitting from inside a list the human is still
    // choosing in would create a session they had not finished pointing anywhere.
    handle_create_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Creating(_)),
        "the form must still be open"
    );
    let form = form_of(&mut app);
    assert_eq!(form.dir.as_str(), format!("{root}/project-alpha/"));
    assert_eq!(form.dir_pick(), None, "the taken pick is spent");

    // With nothing picked, Enter means what it always meant. `inner` does not exist, so this is the
    // ordinary submit path and it registers the session.
    handle_create_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        !matches!(app.mode, UiMode::Creating(_)),
        "a second Enter submits: {:?}",
        app.status
    );
}

#[test]
fn a_new_list_drops_the_old_pick() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let root = d.path().display().to_string();
    let mut app = dir_app(&d.path().join("reg.json"), &format!("{root}/"));
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(1));

    // Typing asks a new question. Keeping an INDEX across a recomputed list is how a picker commits
    // the neighbour of what was highlighted.
    handle_create_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), None);
}

#[test]
fn esc_backs_out_of_the_list_before_it_cancels_the_form() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("project-alpha")).unwrap();
    let mut app = dir_app(&d.path().join("reg.json"), &d.path().display().to_string());
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(form_of(&mut app).dir_pick(), Some(0));

    // One Esc drops the pick and KEEPS the form: losing a filled-in form to a stray press would be
    // a bad trade for a picker nobody asked to keep.
    handle_create_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Creating(_)), "form still open");
    assert_eq!(form_of(&mut app).dir_pick(), None);

    // The second one means what Esc always meant.
    handle_create_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Normal));
}

#[test]
fn the_picked_row_is_the_only_one_that_is_not_dim() {
    let d = tempfile::tempdir().unwrap();
    for c in ["project-alpha", "project-beta"] {
        std::fs::create_dir_all(d.path().join(c)).unwrap();
    }
    let mut app = dir_app(&d.path().join("reg.json"), &d.path().display().to_string());
    handle_create_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let form = form_of(&mut app).clone();
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|fr| render_create(fr, fr.area(), &form)).unwrap();

    // Colored BOLD marks the pick — the dashboard's emphasis everywhere — against dim candidates.
    let picked = styles_under_row(&t, "project-alpha/", "project-alpha/").expect("picked row");
    assert!(
        picked.iter().all(|(_, m)| m.contains(Modifier::BOLD)),
        "{picked:?}"
    );
    assert!(
        picked.iter().all(|(_, m)| !m.contains(Modifier::DIM)),
        "{picked:?}"
    );
    let other = styles_under_row(&t, "project-beta/", "project-beta/").expect("other row");
    assert!(
        other.iter().all(|(_, m)| m.contains(Modifier::DIM)),
        "{other:?}"
    );
    // SELECT MODE advertises its own keys, and stops advertising the one it took: `enter` takes
    // the candidate here, so offering `enter create` would be a lie.
    let text = screen_text(&t);
    assert!(text.contains("enter take"), "{text}");
    assert!(!text.contains("enter create"), "{text}");
}
