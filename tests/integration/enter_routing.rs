//! ACCEPTANCE: on a FRESH agent-loop row, WHOSE claude does `Enter` open? The three
//! tests belong together because each is the others' control — Autopilot must land in
//! the daemon's agent, Standard must still create-and-chat, and with no `pmd` to
//! start the press must refuse instead of arming.

use std::time::Duration;

use agent_manager::registry::Registry;
use agent_manager::state::{self, Config, Tier};
use agent_manager::tmux::Driver;

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{PmdSibling, enter_fixture};
use crate::probe::{
    list_clients, newest_status_entry, tmux_available, wait_for_newest_status, wait_for_pane_text,
    wait_until,
};

// ---------------------------------------------------------------------------
// Enter on a FRESH agent-loop row: whose claude does it open? (m13)
//
// The defect: `Enter` had no `Tier` input at all, so on a just-created AUTOPILOT row
// (no conversation id, no `pmloop-`, no `pmchat-`, no `driver.json`) it fell through to
// the per-session `driver.lock` probe and — in the window before pmd's first sweep takes
// that lock — CREATE-and-chatted its OWN claude. That live `pmchat-` then DEADLOCKED the
// daemon: `JobScheduler::drive`'s gate 1 (`human_present` → `chat_lock::is_active`)
// returns before gate 2's `ensure_session`, so the agent the human asked for was never
// launched and the ledger id never pinned — for up to `CHAT_STALE_S` (12h).
//
// No `FakeDriver` test can see this. The deadlock is COMPOSED of a real tmux session's
// liveness, a real 500ms pmd sweep and real file ordering; every layer looks perfect on
// disk in isolation. So these three drive a real `pmtui` with real keystrokes.
//
// TWO tmux servers on purpose. `pmtui` is HOSTED on a scratch server (so the test can
// type at it) but is pointed at a SECOND `--socket` it shares with `pmd` — which is also
// what makes the auto-attach observable: tmux refuses to attach a session on the server
// whose pane you are already in (`server_client_check_nested`), and that check is
// per-server, so hosting elsewhere lets the real attach path run unmodified.
// ---------------------------------------------------------------------------

/// ACCEPTANCE (POSITIVE): Enter on a fresh **Autopilot** row takes the human to the
/// DAEMON's agent — and creates no claude of its own on the way.
///
/// Enter going into claude is correct and stays. What must change is WHICH claude: on
/// Autopilot the human asked for a hands-off drive, so pmd owns the conversation. pmtui
/// must ensure a daemon, arm, and drop in when pmd's `pmloop-` comes up — never mint an
/// id, never seed the registry, never write a chat marker, never launch a `pmchat-`.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored autopilot_enter_attaches_the_daemons_agent`.
#[test]
#[ignore]
fn autopilot_enter_attaches_the_daemons_agent() {
    if !tmux_available() {
        eprintln!("skipping autopilot-enter test: tmux not available");
        return;
    }
    let fx = enter_fixture("ap", PmdSibling::DelayedReal);
    let submitted = fx.create_then_enter(true);
    let created = submitted.then(|| fx.created()).flatten();

    // Everything is recorded into locals; both servers die before the first assertion.
    let mut loop_up = false;
    let mut attached = false;
    let mut clients = String::new();
    let mut duplicate_alive = false;
    let mut attach_marker = false;
    let mut ledger_cid = None;
    if let Some((_, paths, loop_s, chat_s)) = &created {
        // pmd's `ensure_session` resolves+pins the conversation id, launches the agent and
        // parks `Monitoring` in ONE save. If a `pmchat-` had been launched instead, gate 1
        // would defer for 12h and this never happens.
        loop_up = wait_until(Duration::from_secs(20), || {
            fx.agent.is_alive(loop_s).unwrap_or(false)
        });
        // ONE Enter press: the ~500ms idle drain flips Stay→Open and attaches. No "press
        // Enter again".
        attached = loop_up
            && wait_until(Duration::from_secs(15), || {
                !list_clients(&fx.agent_socket, loop_s).trim().is_empty()
            });
        clients = list_clients(&fx.agent_socket, loop_s);
        duplicate_alive = fx.agent.is_alive(chat_s).unwrap_or(false);
        attach_marker = paths.chat_lock().exists();
        ledger_cid = agent_manager::job::load(paths)
            .ok()
            .flatten()
            .and_then(|l| l.conversation_id);
    }
    let registry = Registry::load(&fx.reg_path);
    let pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    // ---- assertions (both servers and the daemon are already gone) ----
    assert!(fx.up, "pmtui should paint its dashboard; pane was:\n{pane}");
    assert!(
        submitted,
        "the create form should accept Autopilot + a goal"
    );
    let (id, paths, loop_s, chat_s) = created.expect("the session should be registered");
    let reg = registry.expect("registry should be readable");
    let entry = reg.projects.iter().find(|p| p.id == id).unwrap();
    let cfg: Config =
        state::read_json(&paths.config()).expect("per-session config should be seeded");
    assert_eq!(
        cfg.autonomy,
        Tier::Autopilot,
        "the form's Autonomy dial is what got seeded"
    );

    // --- pmtui and pmd address the same terminal ---
    assert_eq!(loop_s, chat_s, "split terminal names returned");
    assert!(
        attach_marker,
        "attach intent must bridge the client-connection race"
    );
    assert!(
        duplicate_alive,
        "the duplicate name must observe the same live terminal"
    );
    assert!(
        entry.conversation_id.is_none(),
        "pmtui must not mint/seed a conversation id on autopilot — pmd's ensure_session owns it"
    );

    // --- and the human really did land in the daemon's agent ---
    assert!(
        loop_up,
        "pmd must launch its persistent agent session {loop_s}; pane was:\n{pane}"
    );
    assert!(
        ledger_cid.is_some(),
        "the ledger's conversation_id must be pinned by pmd (ledger writes are the daemon's alone)"
    );
    assert!(
        attached,
        "ONE Enter must drop the human into {loop_s} — list-clients was {clients:?}; pane was:\n{pane}"
    );
    assert!(
        pmd_left.is_empty(),
        "teardown must leave no pmd behind, still running: {pmd_left:?}"
    );
    drop(fx); // whole fixture: reaps pmd + both tmux servers, then the tempdir
}

/// ACCEPTANCE (CONTROL): the identical script with the dial left at **Standard** still
/// create-and-chats. Chattable-on-create is the POINT of Standard — you can talk to a
/// brand-new session immediately even with pmd down — so the autopilot fix must not touch
/// it. This test is what proves the change is a ROUTE split and not a removal.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored standard_enter_still_creates_and_chats`.
#[test]
#[ignore]
fn standard_enter_still_creates_and_chats() {
    if !tmux_available() {
        eprintln!("skipping standard-enter control test: tmux not available");
        return;
    }
    let fx = enter_fixture("st", PmdSibling::DelayedReal);
    let submitted = fx.create_then_enter(false);
    let created = submitted.then(|| fx.created()).flatten();

    let mut chat_up = false;
    let mut chat_marker = false;
    if let Some((_, paths, _, chat_s)) = &created {
        chat_up = wait_until(Duration::from_secs(20), || {
            fx.agent.is_alive(chat_s).unwrap_or(false)
        });
        chat_marker = wait_until(Duration::from_secs(5), || paths.chat_lock().exists());
    }
    let registry = Registry::load(&fx.reg_path);
    let pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let pmd_left = fx.teardown();

    // ---- assertions ----
    assert!(fx.up, "pmtui should paint its dashboard; pane was:\n{pane}");
    assert!(
        submitted,
        "the create form should accept a goal on Standard"
    );
    let (id, paths, _, chat_s) = created.expect("the session should be registered");
    let cfg: Config =
        state::read_json(&paths.config()).expect("per-session config should be seeded");
    assert_eq!(
        cfg.autonomy,
        Tier::Standard,
        "the control stays on Standard"
    );
    assert!(
        chat_up,
        "Standard must still CREATE-and-chat: {chat_s} should be live; pane was:\n{pane}"
    );
    assert!(
        chat_marker,
        "Standard must still write the chat marker (that is how the poll defers to the human)"
    );
    let reg = registry.expect("registry should be readable");
    assert!(
        reg.projects
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .conversation_id
            .is_some(),
        "Standard's create-and-chat mints the id and seeds the registry for pmd to adopt"
    );
    assert!(
        pmd_left.is_empty(),
        "teardown must leave no pmd behind, still running: {pmd_left:?}"
    );
    drop(fx); // whole fixture: reaps pmd + both tmux servers, then the tempdir
}

/// ACCEPTANCE (NEGATIVE): with no `pmd` binary to start, an Autopilot Enter must REFUSE
/// honestly — and a second Enter must repeat the refusal rather than silently do nothing.
///
/// Arming here would be the original complaint in a new costume: an encouraging status in
/// front of a session nothing will ever drive. `pmtui` runs from a COPY in a directory
/// with no `pmd` sibling, which is exactly how `spawn_daemon` resolves the daemon.
///
/// The cancelled Rename between the two Enters is what makes "repeated" observable: its
/// "name unchanged" becomes the STATUS log's NEWEST entry, pushing the first refusal into the
/// dimmed history the log keeps on screen. A refusal back in the newest slot can therefore only
/// be the SECOND Enter producing it — a pane-wide `contains` would match that history instead.
///
/// `#[ignore]`; run with
/// `cargo test --test integration -- --ignored autopilot_enter_without_pmd_refuses_honestly`.
#[test]
#[ignore]
fn autopilot_enter_without_pmd_refuses_honestly() {
    if !tmux_available() {
        eprintln!("skipping autopilot-refusal test: tmux not available");
        return;
    }
    let fx = enter_fixture("np", PmdSibling::Missing);
    let (sock, sess) = (fx.host_socket.clone(), fx.host_session.clone());
    // NOT the atomic double-Enter here: this test watches the status change between the
    // two presses, so they must be separate.
    let submitted = fx.up
        && send_literal(&sock, &sess, "n")
        && wait_for_pane_text(&fx.host, &sess, "New session")
        // Message is focused first, then engine→model→dir→name→autonomy.
        && send_key(&sock, &sess, "Tab")
        && send_key(&sock, &sess, "Tab")
        && send_key(&sock, &sess, "Tab")
        && send_key(&sock, &sess, "Tab")
        && send_key(&sock, &sess, "Tab")
        && send_key(&sock, &sess, "Space")
        && wait_for_pane_text(&fx.host, &sess, "autopilot")
        && send_key(&sock, &sess, "Tab")
        && send_literal(&sock, &sess, "keep the fixture green")
        && send_key(&sock, &sess, "Enter")
        && wait_for_pane_text(&fx.host, &sess, "created");

    // "autopilot needs pmd" is front-loaded on purpose: `keybar_line` truncates the TAIL,
    // so the reason (the missing path) is the part a narrow pane may lose, not the verdict.
    // Every wait below reads the log FILE's newest line (`wait_for_newest_status`), never the pane:
    // the keybar shows only the latest status and truncates it, so the file is both the complete text
    // and the only place "which status is newest" is unambiguous.
    let refused_once = submitted
        && send_key(&sock, &sess, "Enter")
        && wait_for_newest_status(&fx.reg_path, "autopilot needs pmd");
    // A cancelled Rename is a read-only status change. Once its "name unchanged" is the newest
    // entry, the first refusal is history, so the check after the next Enter cannot match it.
    let status_moved = refused_once
        && send_literal(&sock, &sess, "R")
        // The Rename overlay's own hint: the keybar already shows an `R Rename` chip.
        && wait_for_pane_text(&fx.host, &sess, "empty restores id")
        && send_key(&sock, &sess, "Escape")
        && wait_for_newest_status(&fx.reg_path, "name unchanged");
    let refused_twice = status_moved
        && send_key(&sock, &sess, "Enter")
        && wait_for_newest_status(&fx.reg_path, "autopilot needs pmd");

    let created = fx.created();
    let mut chat_alive = false;
    let mut chat_marker = false;
    if let Some((_, paths, _, chat_s)) = &created {
        chat_alive = fx.agent.is_alive(chat_s).unwrap_or(false);
        chat_marker = paths.chat_lock().exists();
    }
    let registry = Registry::load(&fx.reg_path);
    let pane = fx
        .host
        .capture_tail(&fx.host_session, 200)
        .unwrap_or_default();
    let newest = newest_status_entry(&fx.reg_path);
    let pmd_left = fx.teardown();

    // ---- assertions ----
    assert!(fx.up, "pmtui should paint its dashboard; pane was:\n{pane}");
    assert!(submitted, "the create should land; pane was:\n{pane}");
    assert!(
        refused_once,
        "an Autopilot Enter with no pmd must say so, naming m as the escape hatch; \
         newest status {newest:?}; pane was:\n{pane}"
    );
    assert!(
        status_moved,
        "the cancelled Rename should have made \"name unchanged\" the newest status; \
         newest status {newest:?}; pane was:\n{pane}"
    );
    assert!(
        refused_twice,
        "a SECOND Enter must put the refusal back in the newest status slot, not silently do \
         nothing; newest status {newest:?}; pane was:\n{pane}"
    );
    let (id, _, _, _) = created.expect("the session should still be registered");
    assert!(
        !chat_alive && !chat_marker,
        "a refusal must not fall back to creating a chat of its own"
    );
    assert!(
        registry
            .expect("registry should be readable")
            .projects
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .conversation_id
            .is_none(),
        "a refusal mints nothing"
    );
    assert!(
        pmd_left.is_empty(),
        "no pmd could start, so none may be left: {pmd_left:?}"
    );
    drop(fx); // whole fixture: reaps pmd + both tmux servers, then the tempdir
}
