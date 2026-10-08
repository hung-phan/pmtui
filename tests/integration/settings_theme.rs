//! Acceptance: the `0` Settings view really repaints a real terminal, and the choice really lands on
//! disk.
//!
//! The unit tests deliberately never install a theme other than the default — applying is
//! process-wide and every other colour assertion in that binary would then be comparing two themes.
//! So THE SWITCH is proven here, where pmtui is its own process: the styled capture must carry the
//! new theme's background afterwards and not the old one, and `pmtui.json` must name the chosen id.

use std::process::{Command, Stdio};
use std::time::Duration;

use agent_manager::tmux::{Driver, TmuxDriver};

use crate::keystrokes::send_key;
use crate::probe::{TmuxSocket, tmux_available, wait_for_pane_text_within};
use crate::seed::seed_standard_loop_session;

/// `48;2;r;g;b` — the SGR a true-colour background is drawn with, which is what a styled capture
/// shows and therefore the only honest way to ask "did the palette change?".
fn bg_sgr(theme_id: &str) -> String {
    let theme = opaline::builtins::load_by_name(theme_id).expect("a theme the picker offered");
    let color = theme.color(opaline::names::tokens::BG_BASE);
    match ratatui::style::Color::from(color) {
        ratatui::style::Color::Rgb(r, g, b) => format!("48;2;{r};{g};{b}"),
        other => panic!("{theme_id}'s base background is not true colour: {other:?}"),
    }
}

#[test]
#[ignore = "acceptance: real pmtui theme switch over tmux"]
fn picking_a_theme_repaints_the_dashboard_and_is_remembered() {
    if !tmux_available() {
        eprintln!("skipping settings-theme acceptance: tmux not available");
        return;
    }

    let themes = agent_manager::theme::available();
    let default = themes[0].clone();
    assert_eq!(default.id, agent_manager::theme::DEFAULT_THEME);
    // The first row below the default whose canvas actually differs: a repaint is only observable
    // where the two themes disagree, and asserting on a row that happens to share a background would
    // pass whether or not anything was applied.
    let (steps, picked) = themes
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, choice)| bg_sgr(&choice.id) != bg_sgr(&default.id))
        .expect("some theme paints a different canvas than the default");

    let host = TmuxSocket::new("am-settings-theme");
    let inner = TmuxSocket::new("am-settings-theme-inner");
    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, dir.path(), "alpha");
    let preferences = dir.path().join("pmtui.json");

    let command = format!(
        "'{}' --registry '{}' --socket '{}'",
        env!("CARGO_BIN_EXE_pmtui"),
        registry.display(),
        inner.name()
    );
    let status = Command::new("tmux")
        .args([
            "-L",
            host.name(),
            "new-session",
            "-d",
            "-s",
            "ui",
            "-x",
            "120",
            "-y",
            "32",
            &command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("launch scratch pmtui");
    assert!(status.success());
    let driver = TmuxDriver::with_socket(host.name());
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "SESSIONS",
        Duration::from_secs(5)
    ));

    // A fresh dashboard has no preferences file: the default is in force because it is the default,
    // not because something wrote it down.
    assert!(!preferences.exists(), "{}", preferences.display());
    let before = driver
        .capture_tail_styled("ui", 40)
        .expect("styled capture of the session view");
    assert!(
        before.contains(&bg_sgr(&default.id)),
        "the dashboard does not paint the default canvas: {before}"
    );

    assert!(send_key(host.name(), "ui", "0"));
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "SETTINGS",
        Duration::from_secs(5)
    ));
    // THE TABLE: one row per setting, naming the value in force. The options are not on screen yet —
    // the page is a table of settings, not one setting's list.
    let table = driver.capture_tail("ui", 40).expect("capture settings");
    assert!(
        table.contains("Theme") && table.contains(&default.display),
        "the row must name the setting and its value: {table}"
    );
    assert!(
        table.contains('▾'),
        "the row must show there is a list under it: {table}"
    );
    assert!(
        !table.contains(&picked.display),
        "an unopened dropdown is showing its options: {table}"
    );
    // The view names the file it writes, so the setting is findable outside pmtui.
    assert!(
        table.contains("pmtui.json"),
        "the view must name its file: {table}"
    );

    // ENTER DROPS THE LIST, on the value in force.
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        &picked.display,
        Duration::from_secs(5)
    ));
    let opened = driver.capture_tail("ui", 40).expect("capture the dropdown");
    let marked = opened
        .lines()
        .find(|line| line.contains('●') && line.contains(&default.display))
        .unwrap_or_else(|| panic!("the value in force is not marked: {opened}"));
    // Names, not ids and not colour swatches — the user's call: *"we can just use the name"*.
    assert!(
        !marked.contains(&default.id) && !marked.contains('█'),
        "the list is showing more than the name: {marked}"
    );

    for _ in 0..steps {
        assert!(send_key(host.name(), "ui", "j"));
    }
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(
        wait_for_pane_text_within(
            &driver,
            "ui",
            &format!("theme → {}", picked.display),
            Duration::from_secs(5)
        ),
        "the status never confirmed the theme"
    );

    // THE REPAINT, on a real terminal: the new canvas is on screen and the old one is gone.
    let after = driver
        .capture_tail_styled("ui", 40)
        .expect("styled capture after the switch");
    assert!(
        after.contains(&bg_sgr(&picked.id)),
        "the new theme's canvas never reached the screen: {after}"
    );
    assert!(
        !after.contains(&bg_sgr(&default.id)),
        "the old canvas is still being painted: {after}"
    );
    // …and the committing closed the list, leaving the ROW stating the new value.
    let plain = driver
        .capture_tail("ui", 40)
        .expect("capture settings again");
    let row = plain
        .lines()
        .find(|line| line.contains("Theme") && line.contains('▾'))
        .unwrap_or_else(|| panic!("the settings row is gone: {plain}"));
    assert!(
        row.contains(&picked.display),
        "the row still names the old value: {row}"
    );
    assert!(
        !plain.contains(&default.display),
        "the dropdown is still open after a pick: {plain}"
    );

    // REMEMBERED, beside the registry it belongs to and by ID.
    let written = std::fs::read_to_string(&preferences).expect("preferences were written");
    assert!(
        written.contains(&picked.id),
        "{} does not name the choice: {written}",
        preferences.display()
    );

    // `1` is the way back, and the session view is painted in the new theme too — the canvas is the
    // dashboard's, not the view's.
    assert!(send_key(host.name(), "ui", "1"));
    assert!(wait_for_pane_text_within(
        &driver,
        "ui",
        "SESSIONS",
        Duration::from_secs(5)
    ));
    let back = driver
        .capture_tail_styled("ui", 40)
        .expect("styled capture of the session view again");
    assert!(
        back.contains(&bg_sgr(&picked.id)),
        "the session view kept the old canvas: {back}"
    );
}
