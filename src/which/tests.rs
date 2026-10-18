//! The one `PATH` search, including the case that used to have two answers.

use std::ffi::OsString;

use super::{Found, is_executable, on_path};

/// A directory holding one executable file, one plain file and one subdirectory, so the search is
/// tested against everything that can sit on `PATH` wearing the right name.
fn fixture() -> (tempfile::TempDir, OsString) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("agent");
    std::fs::write(&bin, "#!/bin/sh\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    std::fs::write(dir.path().join("data"), "not a program\n").expect("write");
    std::fs::create_dir(dir.path().join("folder")).expect("mkdir");
    let path = dir.path().as_os_str().to_os_string();
    (dir, path)
}

#[test]
fn a_bare_name_is_found_on_the_path_it_is_given() {
    let (_dir, path) = fixture();
    assert_eq!(on_path("agent", Some(&path)), Found::Yes);
    assert_eq!(on_path("missing", Some(&path)), Found::No);
}

/// NEITHER A DIRECTORY NOR A DATA FILE IS A PROGRAM, however right its name is. Without the executable
/// bit a `claude/` directory on `PATH` would read as the agent being installed.
#[test]
fn only_an_executable_file_counts() {
    let (dir, path) = fixture();
    assert_eq!(on_path("folder", Some(&path)), Found::No);
    assert!(!is_executable(&dir.path().join("folder")));
    assert!(!is_executable(&dir.path().join("nope")));
    #[cfg(unix)]
    assert_eq!(
        on_path("data", Some(&path)),
        Found::No,
        "a readable file without the executable bit is not a program"
    );
}

/// A NAME THAT LOOKS LIKE A LOCATION IS PROBED WHERE IT POINTS, as a shell would, so it never depends
/// on `PATH` and can never answer `PathUnknown`.
#[test]
fn a_name_with_a_separator_is_probed_as_itself() {
    let (dir, _path) = fixture();
    let absolute = dir.path().join("agent").to_string_lossy().into_owned();
    assert_eq!(on_path(&absolute, None), Found::Yes);
    let missing = dir.path().join("nope").to_string_lossy().into_owned();
    assert_eq!(on_path(&missing, None), Found::No);
}

/// THE CASE THAT USED TO HAVE TWO ANSWERS.
///
/// With no `PATH` at all — a `pmd` under a systemd or launchd unit that exports none — the supervisor's
/// copy of this said "installed" (fail open, deliberately) and the tmux driver's said "not installed",
/// and the driver gates a launch. One process, both answers, so the launch refused an agent the decider
/// was still consulting about. `PathUnknown` is why that cannot happen quietly again: it is neither
/// yes nor no until a caller says which it wants.
#[test]
fn an_unreadable_path_is_its_own_answer_not_a_silent_no() {
    assert_eq!(on_path("agent", None), Found::PathUnknown);

    assert!(
        Found::PathUnknown.or_unknown(),
        "a caller whose alternative is to disable itself reads it as yes"
    );
    assert!(
        !Found::PathUnknown.is_yes(),
        "a caller that must not claim presence reads it as no"
    );

    // And the two readings still agree about the answers that ARE answers.
    assert!(Found::Yes.or_unknown() && Found::Yes.is_yes());
    assert!(!Found::No.or_unknown() && !Found::No.is_yes());
}
