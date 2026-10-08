//! Typing at a real pane the way a human does: raw tmux key names, literal text,
//! and several keys delivered in ONE `send-keys` call. Kept apart from
//! [`crate::probe`] because everything here WRITES to a pane, and everything there
//! only reads.

use std::process::Command;

/// Send raw tmux key names (`Tab`, `Space`, `Enter`, …) to a pane. `send-keys` takes
/// a PANE target, so it needs the `=name:` form — the bare `=name` anchor tmux
/// accepts for `has-session` is rejected here.
pub(crate) fn send_key(socket: &str, session: &str, key: &str) -> bool {
    Command::new("tmux")
        .args([
            "-L",
            socket,
            "send-keys",
            "-t",
            &format!("={session}:"),
            key,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Send LITERAL text to a pane (`-l`), so `n`/a goal string arrive as the characters
/// a human types rather than as tmux key names.
pub(crate) fn send_literal(socket: &str, session: &str, text: &str) -> bool {
    Command::new("tmux")
        .args([
            "-L",
            socket,
            "send-keys",
            "-t",
            &format!("={session}:"),
            "-l",
            text,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Deliver one SGR mouse left-click directly to a pane. Coordinates are terminal-cell
/// zero-based, matching crossterm; the wire format is one-based.
pub(crate) fn send_mouse_click(socket: &str, session: &str, column: u16, row: u16) -> bool {
    let press = format!(
        "\u{1b}[<0;{};{}M",
        column.saturating_add(1),
        row.saturating_add(1)
    );
    send_literal(socket, session, &press)
}

/// Deliver one SGR mouse-wheel notch (button 64 up, 65 down) directly to a pane, with the
/// same zero-based coordinates as [`send_mouse_click`].
pub(crate) fn send_mouse_wheel(
    socket: &str,
    session: &str,
    column: u16,
    row: u16,
    down: bool,
) -> bool {
    let notch = format!(
        "\u{1b}[<{};{};{}M",
        if down { 65 } else { 64 },
        column.saturating_add(1),
        row.saturating_add(1)
    );
    send_literal(socket, session, &notch)
}

/// Send SEVERAL tmux key names in ONE `send-keys` call, so they land in the pane's input
/// buffer back-to-back with no process-spawn gap between them.
///
/// Load-bearing for these tests: `Enter Enter` = submit-the-create-form THEN
/// Enter-on-the-new-row, delivered microseconds apart, which pins the case where the
/// per-session `driver.lock` is still FREE because the `pmd` the create just spawned has
/// not swept yet. Two separate `send_key` calls would each cost a process spawn (~10ms) —
/// the same order as pmd's boot — and the test would flake between the two routes.
pub(crate) fn send_keys_atomic(socket: &str, session: &str, keys: &[&str]) -> bool {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", socket, "send-keys", "-t", &format!("={session}:")]);
    cmd.args(keys);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}
