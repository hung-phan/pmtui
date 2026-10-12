//! Acceptance: the Settings `Updates` row really asks a real installer, and really hands the real
//! terminal over to it.
//!
//! The unit tests inject `App::installer_path` directly, which proves the decisions but says nothing
//! about the two things only a process can have: an `$XDG_DATA_HOME` to resolve the installer from,
//! and a terminal to give away. Both are exercised here against a STUB `install.sh` — the script's
//! own `--check` contract is proven by `tests/install_sh_test.sh`, so what is left to prove is that
//! the dashboard runs it, believes it, and says something a human can act on.
//!
//! No network: the stub is the only installer the child can see, because `$XDG_DATA_HOME` is the one
//! this test wrote.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use agent_manager::tmux::{Driver, TmuxDriver};

use crate::keystrokes::send_key;
use crate::probe::{TmuxSocket, tmux_available, wait_for_pane_text_within};
use crate::seed::seed_standard_loop_session;

/// The version this test binary was compiled from — the same string the dashboard compiles in, so
/// the row's "what am I running" half can be asserted rather than guessed.
const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// A tag no future release can overtake, so "an update is available" needs no re-pinning.
const LATEST: &str = "v9.9.9";

/// Write a stub `install.sh` where `installer_from_env` will look for it, and return the
/// `$XDG_DATA_HOME` to hand the child.
///
/// It answers `--check` with the two-line contract and LOGS every invocation, so the update path can
/// be asserted on the argv the dashboard actually chose rather than on the status line alone.
fn stub_installer(root: &Path, log: &Path) -> std::path::PathBuf {
    let data = root.join("data");
    let dir = data.join("agent-manager");
    std::fs::create_dir_all(&dir).expect("create the installer directory");
    let script = dir.join("install.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
echo "$@" >> '{log}'
case "$1" in
  --check)
    printf 'current=v0.0.1\nlatest={LATEST}\n'
    exit 0
    ;;
esac
echo "stub installer ran"
exit 0
"#,
            log = log.display(),
        ),
    )
    .expect("write the stub installer");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make the stub executable");
    data
}

/// Launch a scratch `pmtui` in a real tmux pane with `$XDG_DATA_HOME` pointed at `data`.
fn launch(host: &TmuxSocket, inner: &TmuxSocket, registry: &Path, data: &Path) -> TmuxDriver {
    let command = format!(
        "env XDG_DATA_HOME='{}' '{}' --registry '{}' --socket '{}'",
        data.display(),
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
    driver
}

/// `0` then `j` — the Settings view, cursor on the second row, which is `Updates`.
fn open_updates_row(host: &TmuxSocket, driver: &TmuxDriver) {
    assert!(send_key(host.name(), "ui", "0"));
    assert!(wait_for_pane_text_within(
        driver,
        "ui",
        "SETTINGS",
        Duration::from_secs(5)
    ));
    assert!(send_key(host.name(), "ui", "j"));
}

#[test]
#[ignore = "acceptance: real pmtui update check over tmux"]
fn the_updates_row_checks_a_real_installer_and_hands_it_the_terminal() {
    if !tmux_available() {
        eprintln!("skipping settings-update acceptance: tmux not available");
        return;
    }

    let host = TmuxSocket::new("am-settings-update");
    let inner = TmuxSocket::new("am-settings-update-inner");
    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, dir.path(), "alpha");
    let log = dir.path().join("installer.log");
    let data = stub_installer(dir.path(), &log);

    let driver = launch(&host, &inner, &registry, &data);

    // THE ROW BEFORE ANYTHING IS ASKED. It names the running version and says what to press —
    // opening a screen must not reach the network, where a stall looks like a frozen dashboard.
    open_updates_row(&host, &driver);
    let table = driver.capture_tail("ui", 40).expect("capture settings");
    assert!(
        table.contains("Updates") && table.contains(RUNNING),
        "the row must name the setting and the running version: {table}"
    );
    assert!(
        table.contains("not checked yet"),
        "the row must say it has not asked: {table}"
    );
    assert!(
        !log.exists(),
        "the dashboard ran the installer without being asked"
    );

    // ONE KEYPRESS, NO LIST. The keybar names the act before it happens, and no dropdown appears
    // over the row — the whole point of the row having no options.
    assert!(
        table.contains("Check for updates"),
        "the keybar must say what Enter does: {table}"
    );
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(
        wait_for_pane_text_within(
            &driver,
            "ui",
            &format!("{LATEST} is available"),
            Duration::from_secs(15)
        ),
        "the status never reported the check: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    let checked = std::fs::read_to_string(&log).expect("the installer was invoked");
    assert_eq!(checked.trim(), "--check", "unexpected argv: {checked}");

    // The ROW now carries both facts: what is available, and that the installer's own marker
    // disagrees with the running binary — the thing that explains a machine that never auto-updates.
    let row = driver
        .capture_tail("ui", 40)
        .expect("capture settings after the check");
    assert!(
        row.contains(&format!("→ {LATEST}")),
        "the row does not name the new release: {row}"
    );
    assert!(
        row.contains("installed v0.0.1"),
        "the row does not name the disagreeing marker: {row}"
    );

    // THE SAME KEY NOW INSTALLS, and the keybar says so rather than still offering a check.
    assert!(
        wait_for_pane_text_within(&driver, "ui", "Install it", Duration::from_secs(5)),
        "the keybar never changed to the install: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    assert!(send_key(host.name(), "ui", "Enter"));
    // AND THE DASHBOARD COMES BACK AS A NEW PROCESS. The old image knew the answer; a dashboard
    // that has forgotten it is the proof that this one re-executed rather than merely repainted —
    // which is what "refresh pmtui after installing" has to mean.
    assert!(
        wait_for_pane_text_within(&driver, "ui", "SESSIONS", Duration::from_secs(30)),
        "pmtui never restarted after the installer: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    // It came back as ITSELF: the seeded session is still listed, so the `--registry` it was given
    // survived the exec and a throwaway dashboard did not reappear pointed at the real one.
    assert!(
        wait_for_pane_text_within(&driver, "ui", "alpha", Duration::from_secs(10)),
        "the restarted dashboard lost its scratch registry: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    open_updates_row(&host, &driver);
    assert!(
        wait_for_pane_text_within(&driver, "ui", "not checked yet", Duration::from_secs(10)),
        "the dashboard kept the old process's answer, so it did not restart: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    // Read LAST, now that the restart proves the installer finished: `--if-newer` even though the
    // row already decided, because the installer is the authority on whether replacing the binaries
    // is warranted, and `-y` because the human just said yes here.
    let ran = std::fs::read_to_string(&log).expect("the installer log");
    assert!(
        ran.lines().any(|line| line == "--update --if-newer -y"),
        "the update argv is not what the row promised: {ran}"
    );
}

/// An `install.sh` from before `--check` existed, verbatim in behaviour: it rejects the argument on
/// stderr and exits 1 — the SAME exit as "could not reach GitHub".
fn old_installer(root: &Path) -> std::path::PathBuf {
    let data = root.join("old-data");
    let dir = data.join("agent-manager");
    std::fs::create_dir_all(&dir).expect("create the installer directory");
    let script = dir.join("install.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '\u{2717} unknown argument: %s (try --help)\\n' \"$1\" >&2\nexit 1\n",
    )
    .expect("write the old installer");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("make it executable");
    data
}

#[test]
#[ignore = "acceptance: real pmtui update check over tmux"]
fn an_installer_too_old_to_ask_is_named_rather_than_blamed_on_the_network() {
    if !tmux_available() {
        eprintln!("skipping settings-update acceptance: tmux not available");
        return;
    }

    // THE BUG A USER HIT ON DAY ONE. Every machine installed before `--check` shipped has an
    // installer that cannot answer, and reporting its exit 1 as "could not reach GitHub" sent
    // someone with a working network to go and debug their network.
    let host = TmuxSocket::new("am-settings-oldinstaller");
    let inner = TmuxSocket::new("am-settings-oldinstaller-inner");
    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, dir.path(), "alpha");
    let data = old_installer(dir.path());

    let driver = launch(&host, &inner, &registry, &data);
    open_updates_row(&host, &driver);
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(
        wait_for_pane_text_within(&driver, "ui", "too old", Duration::from_secs(15)),
        "the dashboard did not name the real problem: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    let said = driver.capture_tail("ui", 40).expect("capture the row");
    assert!(
        !said.contains("could not reach GitHub"),
        "a working network is still being blamed: {said}"
    );
    // AND THE WAY OUT IS ON SCREEN. Not "update" — a too-old installer cannot repair itself, which
    // was measured: it answers `--update --if-newer` with "Already up to date" and skips its own
    // self-copy. So the offer is a REPAIR, which fetches a current script and runs that.
    assert!(
        said.contains("Repair the installer"),
        "the keybar does not offer the fix: {said}"
    );
}

#[test]
#[ignore = "acceptance: real pmtui update check over tmux"]
fn a_repair_fetches_a_current_installer_and_runs_it() {
    if !tmux_available() {
        eprintln!("skipping settings-update acceptance: tmux not available");
        return;
    }

    // THE WHOLE ESCAPE, end to end in a real process: a stuck machine presses Enter twice and comes
    // back able to check. Fetched over `file://`, so the test needs no network — the only thing a
    // URL scheme changes here is where `curl` reads from.
    let host = TmuxSocket::new("am-settings-repair");
    let inner = TmuxSocket::new("am-settings-repair-inner");
    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, dir.path(), "alpha");
    let data = old_installer(dir.path());
    let log = dir.path().join("installer.log");

    // What the repair will fetch: a script that LOOKS like the installer (it must pass the sniff)
    // and records how it was called.
    let published = dir.path().join("published-install.sh");
    std::fs::write(
        &published,
        format!(
            "#!/bin/sh\ninstall_binary() {{ :; }}\nlatest_tag() {{ :; }}\necho \"fetched $@\" >> '{}'\nexit 0\n",
            log.display()
        ),
    )
    .expect("write the published installer");

    let command = format!(
        "env XDG_DATA_HOME='{}' PMTUI_REPAIR_URL='file://{}' '{}' --registry '{}' --socket '{}'",
        data.display(),
        published.display(),
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

    open_updates_row(&host, &driver);
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(
        wait_for_pane_text_within(&driver, "ui", "too old", Duration::from_secs(15)),
        "the row never reached the stuck state: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );

    // THE SECOND PRESS ESCAPES IT.
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(
        wait_for_pane_text_within(&driver, "ui", "SESSIONS", Duration::from_secs(30)),
        "pmtui never restarted after the repair: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );

    // It ran the FETCHED script, not the stale one on disk — which is the whole point, since the
    // stale one would have said "Already up to date" and changed nothing.
    let ran = std::fs::read_to_string(&log).expect("the fetched installer ran");
    assert!(
        ran.lines()
            .any(|line| line == "fetched --update --if-newer -y"),
        "the repair ran the wrong script or arguments: {ran}"
    );
}

#[test]
#[ignore = "acceptance: real pmtui update check over tmux"]
fn a_build_with_no_installer_says_so_rather_than_failing_a_check() {
    if !tmux_available() {
        eprintln!("skipping settings-update acceptance: tmux not available");
        return;
    }

    let host = TmuxSocket::new("am-settings-noinstaller");
    let inner = TmuxSocket::new("am-settings-noinstaller-inner");
    let dir = tempfile::tempdir().expect("scratch root");
    let registry = dir.path().join("registry.json");
    seed_standard_loop_session(&registry, dir.path(), "alpha");
    // An EMPTY data home: a cargo build, a packaged binary, a checkout. The installer is absent, and
    // that is a different fact from a check that failed.
    let data = dir.path().join("empty-data");
    std::fs::create_dir_all(&data).expect("create an empty data home");

    let driver = launch(&host, &inner, &registry, &data);
    open_updates_row(&host, &driver);
    assert!(send_key(host.name(), "ui", "Enter"));
    assert!(
        wait_for_pane_text_within(&driver, "ui", "no installer at", Duration::from_secs(10)),
        "the dashboard did not say the installer is missing: {}",
        driver.capture_tail("ui", 40).unwrap_or_default()
    );
    let said = driver.capture_tail("ui", 40).expect("capture the status");
    assert!(
        said.contains("agent-manager/install.sh"),
        "the message must name the path it looked in: {said}"
    );
}
