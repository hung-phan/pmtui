//! What the theme seam guarantees: the picker can only offer themes that load, a bad id changes
//! nothing, and every role resolves to the token this module says it does.
//!
//! # Why nothing here installs a non-default theme
//!
//! The active theme is process-wide, and most of this crate's colour assertions read a role at assert
//! time (`assert_eq!(hue, theme::hard())`). A test that switched the palette mid-run would make those
//! compare a screen drawn under one theme against a colour taken from another — a flake with no local
//! cause. So the switching half is exercised through [`load`], which resolves a theme without
//! installing it, and the real end-to-end switch (press `0`, pick, Enter) is covered by the
//! real-terminal acceptance suite, where the dashboard is its own process.

use super::*;

/// The picker's list, and the promise behind it: every row it offers can actually be applied.
#[test]
fn the_offered_themes_all_load_and_lead_with_the_default() {
    let all = available();
    assert!(all.len() > 10, "opaline ships dozens of themes: {all:?}");
    assert_eq!(all[0].id, DEFAULT_THEME, "the default must lead the list");

    let rest: Vec<String> = all[1..].iter().map(|c| c.id.clone()).collect();
    let mut sorted = rest.clone();
    sorted.sort();
    assert_eq!(rest, sorted, "the remainder must be sorted by id");

    for choice in &all {
        // The list is BUILT by loading, so this is the property that filter keeps: a row the dropdown
        // shows is a row Enter can apply.
        load(&choice.id).expect("the Settings view only offers themes that load");
        assert!(!choice.display.is_empty(), "{choice:?} has no display name");
        assert!(
            matches!(choice.variant, "dark" | "light"),
            "{choice:?} has an unknown variant"
        );
    }
    assert!(
        all.iter().any(|c| c.variant == "light"),
        "the variant column is worth a column only if some theme is light"
    );

    // The id is the whole identity as far as the settings file is concerned, so two rows may not
    // share one.
    let mut ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
    ids.sort_unstable();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count, "duplicate theme ids in the picker");
}

/// `ThemeChoice` is what the picker's cursor indexes, so it is compared and cloned by value.
#[test]
fn a_choice_is_compared_and_cloned_by_value() {
    let all = available();
    let first = all[0].clone();
    assert_eq!(first, all[0]);
    assert_ne!(first, all[1]);
    assert!(
        format!("{first:?}").contains(DEFAULT_THEME),
        "a choice must be debuggable by id: {first:?}"
    );
}

/// A typo names itself, and costs nothing: no theme is installed and no id is recorded.
#[test]
fn an_unknown_theme_is_reported_rather_than_silently_defaulted() {
    let before = active();
    let err = apply("no-such-theme").expect_err("an unknown id must fail");
    let message = format!("{err:#}");
    assert!(
        message.contains("no-such-theme") && message.contains("Settings"),
        "the failure must name the id and where the list is: {message}"
    );
    assert_eq!(
        active(),
        before,
        "a failed apply must leave the active theme alone"
    );
}

/// Re-applying the theme in force is the same code path as switching, and is the one apply this
/// process can make without changing what every other test is rendering against.
#[test]
fn applying_a_theme_records_its_id() {
    apply(DEFAULT_THEME).expect("the default theme must load");
    assert_eq!(active(), DEFAULT_THEME);
}

/// The roles come from the ACTIVE THEME's tokens — not from constants that happen to look right.
#[test]
fn every_role_resolves_to_the_token_it_documents() {
    apply(DEFAULT_THEME).expect("the default theme must load");
    let theme = load(DEFAULT_THEME).expect("the default theme must load");
    let token = |name: &str| Color::from(theme.color(name));
    use opaline::names::tokens as t;
    for (role, hue, name) in [
        ("hard", hard(), t::ERROR),
        ("soft", soft(), t::WARNING),
        ("live", live(), t::SUCCESS),
        ("accent", accent(), t::ACCENT_PRIMARY),
        ("accent_alt", accent_alt(), t::ACCENT_SECONDARY),
        ("brand", brand(), t::ACCENT_TERTIARY),
        ("muted", muted(), t::TEXT_MUTED),
        ("dim", dim(), t::TEXT_DIM),
        ("rule", rule(), t::BORDER_UNFOCUSED),
        ("base_bg", base_bg(), t::BG_BASE),
        ("surface_bg", surface_bg(), t::BG_ELEVATED),
        ("selection_bg", selection_bg(), t::BG_SELECTION),
        ("text", text(), t::TEXT_PRIMARY),
    ] {
        assert_eq!(hue, token(name), "{role} does not resolve to {name}");
    }

    // The severities must stay TELLABLE APART, whatever the theme does: they are the one place where
    // hue carries meaning rather than decoration.
    assert_ne!(hard(), soft());
    assert_ne!(soft(), live());
    assert_ne!(hard(), live());
    // …and a role is a real colour, not the terminal's default, or the theme would not be visible.
    assert_ne!(base_bg(), Color::Reset);
}

/// Different themes really do paint differently — the seam reads the theme file rather than baking a
/// palette in. Asserted through `load` so the process's own theme is untouched.
#[test]
fn a_different_theme_yields_a_different_palette() {
    let other = available()
        .into_iter()
        .map(|c| c.id)
        .find(|id| id != DEFAULT_THEME)
        .expect("more than one theme ships");
    let mine = load(DEFAULT_THEME).unwrap();
    let theirs = load(&other).unwrap();
    let differs = [
        opaline::names::tokens::BG_BASE,
        opaline::names::tokens::ERROR,
        opaline::names::tokens::ACCENT_PRIMARY,
    ]
    .iter()
    .any(|token| mine.color(token) != theirs.color(token));
    assert!(differs, "{DEFAULT_THEME} and {other} paint identically");
}

/// A filled badge spends reverse video, never a background, so the theme author owns both sides.
#[test]
fn a_fill_is_reverse_video_in_one_hue() {
    let style = fill(hard());
    assert_eq!(style.fg, Some(hard()));
    assert_eq!(style.bg, None);
    assert!(
        style
            .add_modifier
            .contains(Modifier::REVERSED | Modifier::BOLD)
    );
}
