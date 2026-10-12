//! The Settings `Updates` row's decisions, without a network.
//!
//! Every awkward state this row can land in is reachable here: GitHub unreachable, a repository
//! with nothing published, a `source@` marker that cannot be compared, and an installer marker that
//! disagrees with the running binary. Those are the states a live check almost never shows you, and
//! the ones where a wrong answer is actively misleading.

use super::*;
use crate::update::{Check, Next, Outcome, RUNNING, Status, next, status};

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

/// A stub standing in for every `install.sh` shipped BEFORE `--check` existed.
///
/// Verbatim from the real v0.1.2 script's behaviour, which is what a user's machine actually had:
/// it rejects the argument on stderr and exits 1. That exit is also "could not ask GitHub", which is
/// exactly why the row has to judge on the two key lines instead.
fn old_installer(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("old-install.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '\u{2717} unknown argument: %s (try --help)\\n' \"$1\" >&2\nexit 1\n",
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
    assert_eq!(
        next(&available),
        Next::Install {
            latest: latest.clone()
        },
        "a newer release must be actionable"
    );

    let current = status(&Check::parse(
        &format!("current=v{RUNNING}\nlatest=v{RUNNING}\n"),
        Some(0),
    ));
    assert_eq!(current, Status::Current);
    assert_eq!(
        next(&current),
        Next::Check,
        "nothing to update to leaves nothing to install"
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
    assert!(
        matches!(next(&status), Next::Install { .. }),
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
        Status::InstallerTooOld,
    ] {
        let value = status.value(None);
        assert!(
            value.starts_with(RUNNING),
            "{status:?} must lead with the running version: {value}"
        );
    }
    assert!(Status::NothingPublished.value(None).contains("no releases"));
    assert!(
        Status::Unreachable
            .value(None)
            .contains("could not reach GitHub")
    );
    assert!(
        Status::InstallerTooOld
            .value(None)
            .contains("installer too old")
    );
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
    assert_eq!(
        app.pending_install, None,
        "nothing to run means nothing to queue"
    );
    assert!(app.status.contains("no installer at"), "{}", app.status);
}

#[test]
fn an_installer_that_answers_nothing_is_too_old_rather_than_offline() {
    // THE BUG A USER HIT ON DAY ONE. Every machine installed before `--check` existed has an
    // installer that prints `unknown argument: --check` and exits 1 — the same exit as "could not
    // ask GitHub" — so the row told someone with a working network to go and debug their network.
    // The signal is the CONTRACT: a supporting installer always prints both keys.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = old_installer(dir.path());
    app.check_for_update();
    assert_eq!(app.update_status, Status::InstallerTooOld);
    assert_eq!(app.update_installed, None);
    assert!(
        !app.status.contains("could not reach"),
        "a working network must not be blamed: {}",
        app.status
    );
    assert!(app.status.contains("too old"), "{}", app.status);

    // And the way out is named and reachable: `Enter` runs the installer, whose own `--update`
    // replaces its copy of itself.
    let next = next(&app.update_status);
    assert_eq!(next, Next::FixInstaller);

    // Exit 0 with nothing printed is the same failure to answer, not an empty answer.
    app.installer_path = stub_installer(dir.path(), "", 0);
    app.check_for_update();
    assert_eq!(app.update_status, Status::InstallerTooOld);
}

#[test]
fn enter_does_the_one_thing_each_state_leaves_to_do() {
    // ONE KEYPRESS. A dropdown here asked the human to choose between an option and nothing (user:
    // *"the UI, looks complex. it can be just a simple to tap on to check for update"*), so the row
    // has no list: `Enter` acts, and the third column says what it will do.
    assert_eq!(
        crate::settings::SettingKind::Updates.gesture(),
        crate::settings::Gesture::Act
    );
    assert_eq!(
        crate::settings::SettingKind::Theme.gesture(),
        crate::settings::Gesture::Pick,
        "picking a theme is a choice and keeps its list"
    );

    // Nothing known, nothing published, nothing reachable, nothing newer: asking is the only move
    // left, and also the only one that could change the answer.
    for status in [
        Status::Unchecked,
        Status::Current,
        Status::NothingPublished,
        Status::Unreachable,
    ] {
        let next = next(&status);
        assert_eq!(next, Next::Check, "{status:?}");
    }

    // A tag that could not be compared still offers the install: the comparison here is
    // presentational and `--if-newer` is the real decision.
    for status in [
        Status::Available {
            latest: "v9.0.0".to_string(),
        },
        Status::Incomparable {
            latest: "v9.0.0".to_string(),
        },
    ] {
        let next = next(&status);
        assert_eq!(
            next,
            Next::Install {
                latest: "v9.0.0".to_string()
            },
            "{status:?}"
        );
    }
}

#[test]
fn updating_queues_the_installer_and_the_drain_runs_it() {
    use crate::session::drain_terminal_requests;

    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = stub_installer(dir.path(), "", 0);

    app.request_update();
    assert_eq!(
        app.pending_install,
        Some(crate::update::InstallerRun::Update)
    );

    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");

    // TWO HANDOVERS: one for the installer, one to leave the terminal cooked for the `exec`. The
    // second restore only happens because this test's `relaunch_exe` cannot be executed — on a real
    // success there is no process left to restore for.
    assert_eq!(handoff.events, ["suspend", "restore", "suspend", "restore"]);
    assert_eq!(
        app.pending_install, None,
        "the request is consumed, not repeated"
    );
    assert!(!app.pending_relaunch, "and so is the refresh");
    // A NEW BINARY IS NOT A NEW PROCESS. When the restart cannot happen, the message must still say
    // the update LANDED — otherwise someone goes looking for a problem with the install.
    assert!(
        app.status.contains("the new version is installed"),
        "{}",
        app.status
    );
    assert!(app.status.contains("quit and reopen it"), "{}", app.status);
}

#[test]
fn a_repair_fetches_the_installer_from_wherever_this_machine_installed_from() {
    use crate::update::{looks_like_installer, repair_url};

    // A FORK MUST NOT BE REPOINTED AT UPSTREAM. The installer on disk says where it came from, so
    // that is where a repair fetches from; the constant is only for a script that does not say.
    let forked = repair_url("REPO=\"someone/their-fork\"\nmore=stuff\n");
    assert_eq!(
        forked,
        "https://raw.githubusercontent.com/someone/their-fork/main/install.sh"
    );
    assert_eq!(
        repair_url("REPO='other/name'\n"),
        "https://raw.githubusercontent.com/other/name/main/install.sh"
    );
    // A line this parser does not understand is not a guess to act on.
    for unreadable in [
        "",
        "REPO=\n",
        "REPO=notarepo\n",
        "REPO=\"two words/here there\"\n",
    ] {
        assert!(
            repair_url(unreadable).contains(crate::update::REPO),
            "{unreadable:?} must fall back to the known repository"
        );
    }

    // AND THE DASHBOARD READS IT FROM THE INSTALLER ON DISK when nothing overrides it. Only the
    // derived string is asserted — nothing is fetched — because this is the one path that would
    // reach the network in production, and a test of it once ran the real installer.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("install.sh");
    std::fs::write(&script, "#!/bin/sh\nREPO=\"someone/their-fork\"\n").unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = script;
    app.repair_url = None;
    assert_eq!(
        crate::app::updates::repair_source(&app),
        "https://raw.githubusercontent.com/someone/their-fork/main/install.sh"
    );

    // A build with no installer has nothing to repair and nothing to read a repository from, so the
    // request is refused rather than fetching from a guess.
    app.installer_path = std::path::PathBuf::from("/nonexistent/install.sh");
    app.request_repair();
    assert_eq!(app.pending_install, None);
    assert!(app.status.contains("no installer at"), "{}", app.status);

    // AND THE DOWNLOAD IS SNIFFED. A proxy error page and a truncated response are both a body with
    // a 200, and handing one of those to `bash` is the worst thing available here.
    assert!(looks_like_installer(
        "#!/usr/bin/env bash\ninstall_binary() { :; }\nlatest_tag() { :; }\n"
    ));
    for not_it in [
        "<html>404 not found</html>",
        "",
        "install_binary() { :; }\n",
        "latest_tag() { :; }\n",
        "  install_binary() { :; }\n  latest_tag() { :; }\n",
    ] {
        assert!(!looks_like_installer(not_it), "accepted {not_it:?}");
    }
}

#[test]
fn the_download_is_time_capped_on_whichever_tool_is_present() {
    use crate::app::updates::download_argv;

    let dest = std::path::Path::new("/tmp/out.sh");
    let curl = download_argv(true, "https://example/install.sh", dest);
    assert_eq!(curl[0], "curl");
    assert!(curl.iter().any(|arg| arg == "-m"), "{curl:?}");
    // The destination and the URL are the LAST two, in that order — swapping them would download
    // the destination path.
    assert_eq!(curl[curl.len() - 2], dest);
    assert_eq!(curl[curl.len() - 1], "https://example/install.sh");

    // The machine running these tests has curl, so this is the only way to know the fallback is
    // even well-formed.
    let wget = download_argv(false, "https://example/install.sh", dest);
    assert_eq!(wget[0], "wget");
    assert!(wget.iter().any(|arg| arg == "--timeout=30"), "{wget:?}");
    assert!(wget.iter().any(|arg| arg == "--tries=1"), "{wget:?}");
    assert_eq!(wget[wget.len() - 2], dest);
    assert_eq!(wget[wget.len() - 1], "https://example/install.sh");
}

#[test]
fn a_repair_runs_the_script_it_fetched_rather_than_the_one_on_disk() {
    use crate::session::drain_terminal_requests;

    // THE DEAD END THIS ESCAPES, measured on a real machine before it existed: a pre-`--check`
    // installer answers `--update --if-newer` with "Already up to date" and skips its own self-copy,
    // so running it again changes nothing, forever. The repair therefore fetches a fresh script and
    // runs THAT. Driven over `file://`, so the test needs no network.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    let published = dir.path().join("published-install.sh");
    std::fs::write(
        &published,
        format!(
            "#!/bin/sh\ninstall_binary() {{ :; }}\nlatest_tag() {{ :; }}\necho \"fresh $@\" >> '{}'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();

    let mut app = app_with(Vec::new(), UiMode::Normal);
    // The installer on disk is the old one, and it must NOT be the script that runs.
    app.installer_path = old_installer(dir.path());
    app.update_status = Status::InstallerTooOld;
    app.repair_url = Some(format!("file://{}", published.display()));

    app.mode = UiMode::Settings {
        cursor: 1,
        open: None,
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.pending_install,
        Some(crate::update::InstallerRun::Repair),
        "a too-old installer must queue a REPAIR, not another doomed update"
    );

    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");

    let ran = std::fs::read_to_string(&log).expect("the fetched installer ran");
    assert_eq!(
        ran.trim(),
        "fresh --update --if-newer -y",
        "the repair ran the wrong script, or with the wrong arguments: {ran}"
    );
    // A successful repair refreshes onto the new build like any other install. The drain consumes
    // that request in the same pass, so the observable is the message it leaves behind — and this
    // fixture's `relaunch_exe` cannot be executed, which is the only reason there is one to read.
    assert!(
        app.status.contains("the new version is installed"),
        "a repair must finish the job like an update: {}",
        app.status
    );
}

#[test]
fn a_repair_refuses_to_run_something_that_is_not_the_installer() {
    use crate::session::drain_terminal_requests;

    // The one case where failing LOUDLY matters more than self-healing: whatever came back is about
    // to be handed to `bash`.
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("not-the-installer.sh");
    std::fs::write(&junk, "<html>404 not found</html>\n").unwrap();

    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = old_installer(dir.path());
    app.repair_url = Some(format!("file://{}", junk.display()));
    app.request_repair();

    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");
    assert!(
        app.status.contains("is not the installer"),
        "{}",
        app.status
    );
    assert!(!app.pending_relaunch, "nothing was installed");

    // A URL that cannot be fetched at all is reported too, rather than silently doing nothing.
    app.repair_url = Some(format!("file://{}/missing.sh", dir.path().display()));
    app.request_repair();
    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");
    assert!(
        app.status.contains("could not download") || app.status.contains("could not run"),
        "{}",
        app.status
    );
    assert!(!app.pending_relaunch);
}

#[test]
fn a_failed_install_never_reaches_the_refresh() {
    use crate::session::drain_terminal_requests;

    // The refresh is what finishes the job, so it must be reached by a SUCCESS and nothing else: a
    // dashboard that restarted after a failed install would look like it had updated.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.installer_path = stub_installer(dir.path(), "", 7);
    app.request_update();

    let mut handoff = UpdateHandoff::default();
    drain_terminal_requests(&mut app, &mut handoff, "/bin/true");
    assert_eq!(handoff.events, ["suspend", "restore"], "no second handover");
    assert!(!app.pending_relaunch);
    assert!(app.status.contains("installer exited"), "{}", app.status);
}

/// The terminal-handoff seam, recording the suspend/restore pairs the installer and the restart must
/// be wrapped in.
///
/// Local rather than shared with `tests/session.rs`: this file needs only the pairs and one failure
/// mode, and a handoff that records nothing else keeps the assertions readable as sentences.
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
fn the_updates_row_has_no_list_and_says_what_enter_does() {
    let mut app = app_with(Vec::new(), UiMode::Normal);
    let row = app.setting_row(crate::settings::SettingKind::Updates);
    assert!(
        row.options.is_empty(),
        "an acting row must carry no options: {:?}",
        row.options
    );
    assert_eq!(row.current, None);
    assert_eq!(row.value, Status::Unchecked.value(None));

    app.update_status = Status::Available {
        latest: "v9.0.0".to_string(),
    };
    assert!(
        app.setting_row(crate::settings::SettingKind::Updates)
            .options
            .is_empty(),
        "no state grows this row a list"
    );

    // A picking row still has its list — this change must not flatten Theme too.
    assert!(
        !app.setting_row(crate::settings::SettingKind::Theme)
            .options
            .is_empty()
    );
}

#[test]
fn one_enter_does_it_and_the_preferences_learn_nothing() {
    // ONE KEYPRESS, and NOTHING REMEMBERED. `commit_setting` normally saves the picked option to
    // `pmtui.json`; writing an action there would make it a preference applied at every start.
    let dir = tempfile::tempdir().unwrap();
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.settings_path = dir.path().join("pmtui.json");
    app.installer_path = stub_installer(dir.path(), "current=v0.1.2\\nlatest=v0.0.1\\n", 0);

    // The Updates row is the second one. ONE Enter — no list opens over it.
    app.mode = UiMode::Settings {
        cursor: 1,
        open: None,
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        matches!(app.mode, UiMode::Settings { open: None, .. }),
        "an acting row must not open a list: {:?}",
        app.mode
    );
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

    // Once something newer is known the SAME key installs it instead — the row's state picks the
    // act, so the human never chooses from a menu.
    app.update_status = Status::Available {
        latest: "v9.0.0".to_string(),
    };
    handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        app.pending_install,
        Some(crate::update::InstallerRun::Update),
        "the update is queued for the handoff"
    );
    assert!(matches!(app.mode, UiMode::Settings { open: None, .. }));
    assert!(!app.settings_path.exists());

    // And an option can never reach the commit path for this row, because the row has none — but if
    // one did, it must change nothing rather than be written down as a preference.
    app.pending_install = None;
    app.status = "untouched".into();
    app.commit_setting(
        crate::settings::SettingKind::Updates,
        &crate::settings::SettingOption {
            label: "Something else".into(),
            note: String::new(),
            value: "teleport".into(),
        },
    );
    assert_eq!(app.status, "untouched");
    assert_eq!(app.pending_install, None);
    assert!(!app.settings_path.exists());
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
        assert!(screen.contains(kind.summary()), "{kind:?}: {screen}");
    }

    // Narrow: the summaries still survive whole, and it is the VALUE that gives way — the row keeps
    // its own `…`, and the status line is where the long sentence goes.
    let mut narrow = Terminal::new(TestBackend::new(80, 24)).unwrap();
    narrow.draw(|frame| render(frame, &app)).unwrap();
    let screen = screen_text(&narrow);
    for kind in crate::settings::SettingKind::ALL {
        assert!(
            screen.contains(kind.summary()),
            "{kind:?} lost its summary at 80 columns: {screen}"
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
fn an_unchecked_row_says_so_and_offers_only_the_asking() {
    // The row never checks on its own: opening a screen must not reach the network, where a stall
    // is indistinguishable from a frozen dashboard. So it says it has not asked, and the hint says
    // what asking takes.
    let value = Status::Unchecked.value(None);
    assert!(value.starts_with(RUNNING), "{value}");
    assert!(value.contains("not checked yet"), "{value}");
    assert_eq!(next(&Status::Unchecked), Next::Check);
}
