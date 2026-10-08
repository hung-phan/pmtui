use super::*;

#[test]
fn acquire_with_retry_still_refuses_a_real_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmd-test.lock");
    let _owner = try_acquire(&path).unwrap().expect("owner takes it first");

    let started = std::time::Instant::now();
    let got = acquire_with_retry(&path, 3, std::time::Duration::from_millis(10)).expect("probe ok");

    assert!(got.is_none(), "a live owner must still win every attempt");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(20),
        "expected two retry sleeps, took {:?}",
        started.elapsed()
    );
}

#[test]
fn acquire_with_retry_wins_a_lock_freed_mid_probe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmd-test.lock");
    let probe = try_acquire(&path).unwrap().expect("probe takes it");

    assert!(
        try_acquire(&path).unwrap().is_none(),
        "one attempt against a held lock must fail"
    );

    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&done);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(probe);
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let got =
        acquire_with_retry(&path, 8, std::time::Duration::from_millis(25)).expect("retry probe ok");
    assert!(
        got.is_some(),
        "a lock freed mid-probe must be acquired on a later attempt"
    );
    assert!(
        done.load(std::sync::atomic::Ordering::SeqCst),
        "the releasing thread must have run"
    );
}

#[test]
fn acquire_with_retry_makes_one_attempt_when_tries_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmd-test.lock");
    let _owner = try_acquire(&path).unwrap().expect("owner takes it first");

    let got = acquire_with_retry(&path, 0, std::time::Duration::from_secs(1)).unwrap();

    assert!(got.is_none(), "zero tries is normalized to one attempt");
}

#[test]
fn acquire_with_retry_propagates_lock_file_creation_errors() {
    let dir = tempfile::tempdir().unwrap();
    let regular_file = dir.path().join("not-a-directory");
    std::fs::write(&regular_file, "occupied").unwrap();

    let error = acquire_with_retry(
        &regular_file.join("driver.lock"),
        3,
        std::time::Duration::ZERO,
    )
    .expect_err("an invalid parent must remain an I/O error");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn second_acquire_is_blocked_until_holder_drops() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".daemon/driver.lock");
    let first = try_acquire(&path).unwrap();
    assert!(first.is_some(), "first acquire succeeds");
    assert!(
        try_acquire(&path).unwrap().is_none(),
        "second acquire is blocked while the first is held"
    );

    drop(first);

    assert!(
        acquire_with_retry(&path, REACQUIRE_TRIES, REACQUIRE_DELAY)
            .unwrap()
            .is_some(),
        "acquire succeeds after the holder releases"
    );
}

#[test]
fn second_dashboard_on_the_same_registry_is_refused_until_release() {
    let dir = tempfile::tempdir().unwrap();
    let path = pmtui_lock_path(&dir.path().join("registry.json"), "pmd");
    let first = try_acquire(&path)
        .unwrap()
        .expect("first dashboard takes the lock");

    assert!(
        try_acquire(&path).unwrap().is_none(),
        "a second dashboard on the same registry and socket must be refused"
    );
    drop(first);
    assert!(
        acquire_with_retry(&path, REACQUIRE_TRIES, REACQUIRE_DELAY)
            .unwrap()
            .is_some(),
        "once the first exits, the lock frees for the next launch"
    );
}

#[test]
fn acquire_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/deeper/driver.lock");

    assert!(try_acquire(&path).unwrap().is_some());
    assert!(path.exists());
}

#[test]
fn is_held_reports_absent_held_and_free_without_creating_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmd-probe.lock");

    assert_eq!(is_held(&path).unwrap(), None);
    assert!(
        !path.exists(),
        "an absent-lock probe must not create a file"
    );

    let held = try_acquire(&path).unwrap().expect("free to acquire");
    assert_eq!(is_held(&path).unwrap(), Some(true));
    assert_eq!(
        is_held(&path).unwrap(),
        Some(true),
        "a read-only probe must not steal the lock"
    );

    drop(held);
    wait_until_free(&path);
    assert!(
        acquire_with_retry(&path, REACQUIRE_TRIES, REACQUIRE_DELAY)
            .unwrap()
            .is_some(),
        "the free-lock probe must release the lock it momentarily took"
    );
}

#[test]
fn is_held_errors_are_not_reported_as_free() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("registry.json");
    std::fs::write(&not_a_dir, "{}").unwrap();

    let error = is_held(&not_a_dir.join("pmd-x.lock"))
        .expect_err("a non-NotFound open failure must surface as an error");

    assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
}

#[test]
fn a_flock_errno_other_than_contention_is_an_error_not_a_free_lock() {
    use rustix::io::Errno;

    assert!(super::super::lock_outcome(Ok(())).unwrap());
    assert!(!super::super::lock_outcome(Err(Errno::WOULDBLOCK)).unwrap());
    assert!(!super::super::lock_outcome(Err(Errno::AGAIN)).unwrap());
    let error = super::super::lock_outcome(Err(Errno::NOLCK)).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(Errno::NOLCK.raw_os_error()));
}
