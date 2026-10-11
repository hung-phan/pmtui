//! The Settings `Updates` row's decisions, without a network.
//!
//! Every awkward state this row can land in is reachable here: GitHub unreachable, a repository
//! with nothing published, a `source@` marker that cannot be compared, and an installer marker that
//! disagrees with the running binary. Those are the states a live check almost never shows you, and
//! the ones where a wrong answer is actively misleading.

use super::*;
use crate::update::{Check, Outcome, RUNNING, Status, actions, status};

/// A stand-in `install.sh` that prints what a test wants and exits how a test wants.
///
/// The real script is driven end to end by `tests/install_sh_test.sh`; what these tests need is the
/// dashboard's half — that it runs the thing, believes its two lines, and says something true about
/// every exit code. A stub is the only way to reach "GitHub is unreachable" without unplugging the
/// network.
fn stub_installer(dir: &std::path::Path, stdout: &str, code: i32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("install.sh");
    std::fs::write(
        &script,
        // `%b`, not `%s`: the callers write the two-line contract with `\n` escapes, and the point
        // of a stub is that it emits what the real installer emits.
        format!("#!/bin/sh\nprintf '%b' '{stdout}'\nexit {code}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// A version strictly above whatever this build is, so the "available" case does not need
/// re-pinning every release. Bumping the MAJOR is the one move no future version can overtake.
fn newer_tag() -> String {
    let major: u64 = RUNNING
        .split('.')
        .next()
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    format!("v{}.0.0", major + 1)
}

#[test]
fn parse_reads_both_versions_and_ignores_anything_else() {
    let check = Check::parse(
        "current=v0.1.2\nlatest=v0.1.5\nsomething=we do not know\n",
        Some(0),
    );
    assert_eq!(check.installed.as_deref(), Some("v0.1.2"));
    assert_eq!(check.latest.as_deref(), Some("v0.1.5"));
    assert_eq!(check.outcome, Outcome::Found);
}

#[test]
fn an_empty_value_is_absent_not_a_version() {
    // `current=` is what the installer prints when it never installed this machine. Treating that
    // as the literal version "" would put an empty string in front of the human.
    let check = Check::parse("current=\nlatest=v0.1.5\n", Some(0));
    assert_eq!(check.installed, None);
    assert_eq!(check.latest.as_deref(), Some("v0.1.5"));
}

#[test]
fn an_unreachable_check_is_never_reported_as_up_to_date() {
    // THE LIE THIS PREVENTS. `latest_tag` exits 1 when GitHub could not be asked at all. Folding
    // that into "no newer version" would tell someone they are current while their network is down.
    let offline = status(&Check::parse("current=v0.1.2\nlatest=\n", Some(1)));
    assert_eq!(offline, Status::Unreachable);

    // Exit 10 is a different fact: GitHub answered, and there is nothing published.
    let empty_repo = status(&Check::parse("current=v0.1.2\nlatest=\n", Some(10)));
    assert_eq!(empty_repo, Status::NothingPublished);

    // A killed child has no code at all, which is also "could not ask".
    assert_eq!(Outcome::from_code(None), Outcome::Unreachable);
}

#[test]
fn a_success_with_no_tag_is_treated_as_unreachable() {
    // Exit 0 promises a tag. An empty one breaks the contract, and the honest reading of an answer
    // we cannot use is that we did not get one.
    let check = Check::parse("current=v0.1.2\nlatest=\n", Some(0));
    assert_eq!(status(&check), Status::Unreachable);
}

#[test]
fn a_newer_release_is_offered_and_the_current_one_is_not() {
    let latest = newer_tag();
    let available = status(&Check::parse(
        &format!("current={RUNNING}\nlatest={latest}\n"),
        Some(0),
    ));
    assert_eq!(
        available,
        Status::Available {
            latest: latest.clone()
        }
    );
    assert!(
        actions(&available)
            .iter()
            .any(|action| action.label == format!("Update to {latest}")),
        "a newer release must be actionable"
    );

    let current = status(&Check::parse(
        &format!("current=v{RUNNING}\nlatest=v{RUNNING}\n"),
        Some(0),
    ));
    assert_eq!(current, Status::Current);
    let labels: Vec<String> = actions(&current)
        .into_iter()
        .map(|action| action.label)
        .collect();
    assert_eq!(
        labels,
        vec!["Check now".to_string()],
        "nothing to update to means no update action"
    );
}

#[test]
fn versions_compare_numerically_not_as_text() {
    // THE TRAP. As text `v0.1.10` sorts BELOW `v0.1.9`, so a string comparison would hide exactly
    // the update a long-lived project needs most — and it would do it silently, on the tenth patch.
    // Compared against RUNNING rather than the installed marker: the row answers "is the binary I
    // am running behind?", which is the question being asked.
    assert!(matches!(
        status(&Check::parse("current=x\nlatest=v9999.0.0\n", Some(0))),
        Status::Available { .. }
    ));
    assert!(matches!(
        status(&Check::parse("current=x\nlatest=v0.0.1\n", Some(0))),
        Status::Current
    ));
}

#[test]
fn a_tag_that_cannot_be_compared_is_a_fact_not_a_verdict() {
    // A `source@<sha>` install, a date tag, anything unparseable: the row must not claim to know.
    // It still offers the update, because `install.sh --update --if-newer` is the real decision and
    // declines on its own — withholding the action would strand the human with no way forward.
    let check = Check::parse(
        "current=source@abc1234\nlatest=nightly-2026-10-11\n",
        Some(0),
    );
    let status = status(&check);
    assert_eq!(
        status,
        Status::Incomparable {
            latest: "nightly-2026-10-11".to_string()
        }
    );
    assert_eq!(
        actions(&status).len(),
        2,
        "an unknown shape still offers the update"
    );
}

#[test]
fn the_row_names_the_installed_marker_only_when_it_disagrees() {
    // THE FACT THAT EXPLAINS A DEV MACHINE. `scripts/link-release.sh` points ~/.local/bin at a
    // working-tree build, leaving the installer's marker at whatever it last installed — so
    // auto-update appears to do nothing. Saying so is this row's whole value in that case.
    let stale = Status::Current.value(Some("v0.1.2"));
    assert!(
        stale.contains("(installed v0.1.2)"),
        "a disagreeing marker must be named: {stale}"
    );

    // On an ordinary install they are the same, and repeating it would be noise.
    let agreed = Status::Current.value(Some(&format!("v{RUNNING}")));
    assert!(
        !agreed.contains("installed"),
        "a marker that agrees must stay quiet: {agreed}"
    );
    let unknown = Status::Current.value(None);
    assert!(!unknown.contains("installed"), "{unknown}");
}

#[test]
fn every_status_names_the_version_that_is_running() {
    // The row's one job in every state: say what this binary IS, before saying anything about what
    // is published. A state that answered only "could not reach GitHub" would leave the human
    // without the fact they came to the screen for.
    for status in [
        Status::Unchecked,
        Status::Available {
            latest: "v9.0.0".to_string(),
        },
        Status::Current,
        Status::Incomparable {
            latest: "nightly".to_string(),
        },
        Status::NothingPublished,
        Status::Unreachable,
    ] {
        let value = status.value(None);
        assert!(
            value.starts_with(RUNNING),
            "{status:?} must lead with the running version: {value}"
        );
    }
    assert!(Status::NothingPublished.value(None).contains("no releases"));
    assert!(Status::Unreachable.value(None).contains("unreachable"));
    assert!(
        Status::Available {
            latest: "v9.0.0".to_string()
        }
        .value(None)
        .contains("→ v9.0.0")
    );
    assert!(
        Status::Incomparable {
            latest: "nightly".to_string()
        }
        .value(None)
        .contains("latest nightly")
    );
}

#[test]
fn a_tag_with_no_numbers_at_all_cannot_be_compared() {
    // `parts("v")` yields an EMPTY vector, which would otherwise compare as "less than everything"
    // and silently read as up to date.
    let check = Check::parse("current=v0.1.2\nlatest=v\n", Some(0));
    assert_eq!(
        status(&check),
        Status::Incomparable {
            latest: "v".to_string()
        }
    );
}

#[test]
fn checking_reports_what_the_installer_said() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);

    // A newer release: the row offers the update and the status line names the version.
    let latest = newer_tag();
    app.installer_path = stub_installer(
        dir.path(),
        &format!("current=v0.1.2\\nlatest={latest}\\n"),
        0,
    );
    app.check_for_update();
    assert_eq!(
        app.update_status,
        Status::Available {
            latest: latest.clone()
        }
    );
    assert_eq!(app.status, format!("{latest} is available"));
    // The marker the installer reported is kept, because the row names it when it disagrees.
    assert_eq!(app.update_installed.as_deref(), Some("v0.1.2"));

    // Nothing newer.
    app.installer_path = stub_installer(dir.path(), "current=\\nlatest=v0.0.1\\n", 0);
    app.check_for_update();
    assert_eq!(app.update_status, Status::Current);
    assert_eq!(app.status, "already on the newest release");
    assert_eq!(app.update_installed, None, "a bare current= is absent");

    // A tag this module will not compare. Reported, not judged.
    app.installer_path = stub_installer(dir.path(), "current=source@abc\\nlatest=nightly\\n", 0);
    app.check_for_update();
    assert_eq!(app.status, "newest release is nightly");

    // Exit 10: GitHub answered, nothing published.
    app.installer_path = stub_installer(dir.path(), "current=v0.1.2\\nlatest=\\n", 10);
    app.check_for_update();
    assert_eq!(app.update_status, Status::NothingPublished);
    assert_eq!(app.status, "no releases published yet");

    // Exit 1: could not ask. NOT "up to date".
    app.installer_path = stub_installer(dir.path(), "current=v0.1.2\\nlatest=\\n", 1);
    app.check_for_update();
    assert_eq!(app.update_status, Status::Unreachable);
    assert_eq!(app.status, "could not reach GitHub");
}

#[test]
fn a_build_that_was_not_installed_by_the_installer_says_so() {
    // A cargo build, a packaged binary, a checkout: there is no `install.sh` to drive, and the row
    // must say that rather than report a failed check — the difference between "your network is
    // down" and "this build cannot update itself".
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = std::path::PathBuf::from("/nonexistent/install.sh");

    app.check_for_update();
    assert_eq!(app.update_status, Status::Unreachable);
    assert!(
        app.status
            .contains("no installer at /nonexistent/install.sh"),
        "{}",
        app.status
    );
    assert!(app.status.contains("not installed by install.sh"));

    // And the update must not be QUEUED either: the handoff would suspend the terminal to run
    // nothing.
    app.status = "untouched".into();
    app.request_update();
    assert!(!app.pending_update, "nothing to run means nothing to queue");
    assert!(app.status.contains("no installer at"), "{}", app.status);
}

#[test]
fn an_installer_that_prints_nothing_useful_is_not_up_to_date() {
    // Exit 0 promising a tag and delivering none. Already pinned purely; this proves the dashboard
    // half reads the same way when a REAL process does it.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = stub_installer(dir.path(), "", 0);
    app.check_for_update();
    assert_eq!(app.update_status, Status::Unreachable);
    assert_eq!(app.update_installed, None);
}

#[test]
fn updating_queues_the_installer_and_the_drain_runs_it() {
    use crate::session::drain_terminal_requests;

    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = stub_installer(dir.path(), "", 0);

    app.request_update();
    assert!(app.pending_update);

    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");

    // The installer gets the REAL terminal, so the drain must hand it over and take it back.
    assert_eq!(handoff.events, ["suspend", "restore"]);
    assert!(!app.pending_update, "the request is consumed, not repeated");
    // A NEW BINARY IS NOT A NEW PROCESS: the success message has to say restart, because this image
    // is still the old one and a human reading "updated" would believe otherwise.
    assert_eq!(
        app.status,
        "installer finished — restart pmtui to run the new version"
    );
}

#[test]
fn an_installer_that_fails_is_reported_rather_than_claimed() {
    use crate::session::drain_terminal_requests;

    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = stub_installer(dir.path(), "", 3);
    app.request_update();

    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");
    assert!(app.status.contains("installer exited"), "{}", app.status);
    assert!(!app.pending_update);

    // And when the TERMINAL could not be handed over, the installer never ran at all — which must
    // not read as a finished update.
    app.installer_path = stub_installer(dir.path(), "", 0);
    app.request_update();
    let mut broken = UpdateHandoff {
        fail_suspend: true,
        ..UpdateHandoff::default()
    };
    drain_terminal_requests(&mut app, &mut broken, "/bin/true");
    assert!(
        app.status.contains("could not run the installer"),
        "{}",
        app.status
    );
}

/// The terminal-handoff seam, recording the suspend/restore pair the installer must be wrapped in.
///
/// Local rather than shared with `tests/session.rs`: this file needs only the pair and one failure
/// mode, and a handoff that records nothing else keeps the assertion above readable as a sentence.
#[derive(Default)]
struct UpdateHandoff {
    events: Vec<&'static str>,
    fail_suspend: bool,
}

impl crate::session::TerminalHandoff for UpdateHandoff {
    fn suspend(&mut self) -> anyhow::Result<()> {
        self.events.push("suspend");
        if self.fail_suspend {
            anyhow::bail!("test suspend failure");
        }
        Ok(())
    }

    fn restore(&mut self) -> anyhow::Result<()> {
        self.events.push("restore");
        Ok(())
    }
}

#[test]
fn the_updates_row_offers_actions_rather_than_a_remembered_value() {
    // A Settings row normally stores what was picked. These are INSTRUCTIONS, so the dropdown opens
    // on no row at all — opening it on "Check now" would imply checking is the setting in force.
    let mut app = app_with(Vec::new(), UiMode::Normal);
    let row = app.setting_row(crate::settings::SettingKind::Updates);
    assert_eq!(row.current, None);
    assert_eq!(row.value, Status::Unchecked.value(None));
    assert_eq!(row.options.len(), 1, "unchecked offers only the check");

    app.update_status = Status::Available {
        latest: "v9.0.0".to_string(),
    };
    let row = app.setting_row(crate::settings::SettingKind::Updates);
    let values: Vec<&str> = row
        .options
        .iter()
        .map(|option| option.value.as_str())
        .collect();
    assert_eq!(values, [crate::update::ACTION_CHECK, "update"]);
}

#[test]
fn committing_an_update_action_runs_it_and_remembers_nothing() {
    // THE FILE MUST NOT LEARN AN INSTRUCTION. `commit_setting` normally saves the picked option to
    // `pmtui.json`; writing "check" there would make checking a remembered preference, applied on
    // every start.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.settings_path = dir.path().join("pmtui.json");
    app.installer_path = stub_installer(dir.path(), "current=v0.1.2\\nlatest=v0.0.1\\n", 0);

    // Driven through the keys a human presses: the Updates row is the SECOND one, the first Enter
    // opens its dropdown, the second commits the row the cursor is on.
    app.mode = UiMode::Settings {
        cursor: 1,
        open: None,
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(app.mode, UiMode::Settings { open: Some(0), .. }));
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    // THE KEYPRESS ONLY ANNOUNCES IT. The check itself blocks, and a frame has to carry that word to
    // the screen before the blocking starts — otherwise a slow network reads as a dead dashboard.
    assert!(app.pending_check);
    assert_eq!(app.status, "checking GitHub for the newest release…");
    assert_eq!(
        app.update_status,
        Status::Unchecked,
        "the answer must not exist before the check runs"
    );

    crate::app::updates::drain_pending_check(&mut app);
    assert_eq!(app.update_status, Status::Current);
    assert!(!app.pending_check, "a check runs once per request");
    // …and every OTHER frame must leave it alone: this runs on every pass of the event loop, so an
    // unasked check here would be one per frame.
    app.status = "untouched".into();
    crate::app::updates::drain_pending_check(&mut app);
    assert_eq!(app.status, "untouched");
    assert!(
        !app.settings_path.exists(),
        "an action must not be written to the preferences file"
    );

    // Now that something is comparable-and-newer the dropdown grows an Update row; commit that one.
    app.update_status = Status::Available {
        latest: "v9.0.0".to_string(),
    };
    app.mode = UiMode::Settings {
        cursor: 1,
        open: Some(1),
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.pending_update, "the update is queued for the handoff");
    assert!(!app.settings_path.exists());

    // An action this build does not know is reported, not performed. A newer installer could
    // offer one, and a silent no-op would look like a dashboard that ignores Enter.
    app.commit_setting(
        crate::settings::SettingKind::Updates,
        &crate::settings::SettingOption {
            label: "Something else".into(),
            note: String::new(),
            value: "teleport".into(),
        },
    );
    assert_eq!(app.status, "unknown update action teleport");
}

#[test]
fn a_wide_terminal_states_the_whole_answer_and_a_narrow_one_keeps_its_summary() {
    // BOTH FIXED WIDTHS WERE WRONG, and each failure was invisible to the other's test. A value
    // column sized for `Theme` elided the one answer this row exists to give; a column sized for
    // this row pushed `every colour the dashboard draws` off an 80-column terminal, the narrowest
    // the view supports. The column is therefore computed, and both ends are asserted here.
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.update_status = Status::Available {
        latest: "v0.1.7".to_string(),
    };
    app.update_installed = Some("v0.1.4".to_string());
    app.open_settings();
    let full = app.setting_row(crate::settings::SettingKind::Updates).value;
    assert!(
        full.contains("v0.1.7") && full.contains("installed v0.1.4"),
        "{full}"
    );

    // Wide: the whole value, and every summary.
    let mut wide = Terminal::new(TestBackend::new(120, 24)).unwrap();
    wide.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&wide);
    assert!(
        screen.contains(&full),
        "the answer was elided at 120: {screen}"
    );
    for kind in crate::settings::SettingKind::ALL {
        assert!(screen.contains(kind.summary()), "{:?}: {screen}", kind);
    }

    // Narrow: the summaries still survive whole, and it is the VALUE that gives way — the row keeps
    // its own `…`, and the status line is where the long sentence goes.
    let mut narrow = Terminal::new(TestBackend::new(80, 24)).unwrap();
    narrow.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&narrow);
    for kind in crate::settings::SettingKind::ALL {
        assert!(
            screen.contains(kind.summary()),
            "{:?} lost its summary at 80 columns: {screen}",
            kind
        );
    }
    assert!(
        screen.contains(&format!("{RUNNING} → v0.1.7")),
        "the row must still say what is available: {screen}"
    );
}

#[test]
fn the_installer_is_looked_for_where_the_installer_puts_it() {
    // `install.sh` keeps a copy of itself at `$STATE_DIR/install.sh` precisely so `--update` works
    // after the `curl | bash` pipe is gone, and the launcher wrappers already call that path. Read
    // from the environment ONCE, at construction, because edition 2024 forbids `set_var` in tests —
    // so this asserts the shape against the live environment rather than a planted one.
    use crate::app::updates::installer_in;

    let resolved = crate::app::updates::installer_from_env();
    assert!(
        resolved.ends_with("agent-manager/install.sh"),
        "{resolved:?}"
    );

    // `$XDG_DATA_HOME` wins when it is set…
    assert_eq!(
        installer_in(Some("/xdg".into()), Some("/home/someone".into())),
        std::path::PathBuf::from("/xdg/agent-manager/install.sh")
    );
    // …and an EMPTY one counts as unset, which is what the shell means by it and what `install.sh`
    // does with the same two variables.
    for empty in [None, Some(std::ffi::OsString::new())] {
        assert_eq!(
            installer_in(empty, Some("/home/someone".into())),
            std::path::PathBuf::from("/home/someone/.local/share/agent-manager/install.sh")
        );
    }
    // No HOME either: a relative path, which `is_file()` will simply fail — better than a panic on a
    // machine that is already unusual.
    assert_eq!(
        installer_in(None, None),
        std::path::PathBuf::from(".local/share/agent-manager/install.sh")
    );
    // And the update invocation stays `--if-newer`: the installer, not this row's presentational
    // comparison, is the authority on whether replacing the binaries is warranted.
    assert_eq!(
        crate::app::updates::update_args(),
        ["--update", "--if-newer", "-y"]
    );
}

#[test]
fn an_unchecked_row_says_how_to_check_and_offers_only_that() {
    // The row never checks on its own: opening a screen must not reach the network, where a stall
    // is indistinguishable from a frozen dashboard. So it has to say what to press.
    let value = Status::Unchecked.value(None);
    assert!(value.starts_with(RUNNING), "{value}");
    assert!(value.contains("Enter to check"), "{value}");
    let labels: Vec<String> = actions(&Status::Unchecked)
        .into_iter()
        .map(|action| action.label)
        .collect();
    assert_eq!(labels, vec!["Check now".to_string()]);
}
