use super::*;

static TAKEOVER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serialize the takeover tests. A poisoned guard only means an earlier takeover test failed; that
/// failure is already reported, so it must not also fail every later test here.
fn serial_takeover_test() -> std::sync::MutexGuard<'static, ()> {
    TAKEOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn unexpected_confirm(_prompt: &str) -> Result<bool> {
    panic!("this path must not prompt")
}

fn unexpected_force(
    _registry: &Path,
    _socket: &str,
    _owner: &takeover::DashboardOwner,
) -> Result<Option<takeover::DashboardOwnership>> {
    panic!("this path must not force")
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "helper process for the production SIGTERM takeover test"]
fn dashboard_lock_holder_helper() {
    let Ok(registry) = std::env::var("PM_TAKEOVER_HELPER_REGISTRY") else {
        return;
    };
    let socket = std::env::var("PM_TAKEOVER_HELPER_SOCKET").expect("helper socket");
    let _ownership = takeover::claim(Path::new(&registry), &socket)
        .unwrap()
        .expect("helper owns the dashboard lock");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

#[test]
fn dashboard_ownership_and_cooperative_request_are_nonce_bound() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-unit";
    let first = takeover::claim(&registry, socket)
        .unwrap()
        .expect("first dashboard owns the lock");
    let owner = first.owner.clone();

    assert_eq!(
        takeover::read_owner(&registry, socket).unwrap(),
        Some(owner.clone())
    );
    assert!(takeover::claim(&registry, socket).unwrap().is_none());
    takeover::request_handoff(&registry, socket, &owner).unwrap();
    assert!(takeover::requested_for(&registry, socket, &owner.nonce));
    assert!(!takeover::requested_for(
        &registry,
        socket,
        "a-different-owner"
    ));
    let mut wrong = owner.clone();
    wrong.nonce = "a-different-owner".into();
    takeover::cancel_handoff(&registry, socket, &wrong).unwrap();
    assert!(takeover::requested_for(&registry, socket, &owner.nonce));
    takeover::cancel_handoff(&registry, socket, &owner).unwrap();
    assert!(!takeover::requested_for(&registry, socket, &owner.nonce));
    takeover::request_handoff(&registry, socket, &owner).unwrap();

    drop(first);
    let second = takeover::wait_for_handoff(&registry, socket)
        .unwrap()
        .expect("released lock transfers to the contender");
    assert_ne!(second.owner.nonce, owner.nonce);
    assert!(!takeover::requested_for(&registry, socket, &owner.nonce));
}

#[test]
fn dashboard_exits_only_for_its_own_takeover_request() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-app";
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the lock");
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.registry_path = registry.clone();
    app.socket = socket.into();
    app.dashboard_owner_nonce = Some(ownership.owner.nonce.clone());

    let mut wrong = ownership.owner.clone();
    wrong.nonce = "wrong-owner".into();
    takeover::request_handoff(&registry, socket, &wrong).unwrap();
    app.after_input(false);
    assert!(!app.should_quit);

    takeover::request_handoff(&registry, socket, &ownership.owner).unwrap();
    app.after_input(false);
    assert!(app.should_quit);
    assert!(app.status.contains("handing over"));
}

#[test]
fn force_takeover_revalidates_the_owner_before_signalling() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-force";
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the lock");
    let owner = ownership.owner.clone();
    let mut ownership = Some(ownership);
    let mut stale = owner.clone();
    stale.nonce = "stale-owner".into();

    let error = takeover::force_owner_and_wait(&registry, socket, &stale).unwrap_err();
    assert!(error.to_string().contains("owner changed"));

    let replacement = takeover::force_owner_and_wait_with(&registry, socket, &owner, |pid| {
        assert_eq!(pid, owner.pid);
        drop(ownership.take());
        Ok(())
    })
    .unwrap()
    .expect("force path acquires after the validated owner releases");
    assert_ne!(replacement.owner.nonce, owner.nonce);
}

#[test]
fn force_takeover_refuses_a_free_lock_and_propagates_signal_failure() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-force-errors";
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the lock");
    let owner = ownership.owner.clone();

    let signal_error = takeover::force_owner_and_wait_with(&registry, socket, &owner, |_| {
        anyhow::bail!("signal refused")
    })
    .unwrap_err();
    assert!(signal_error.to_string().contains("signal refused"));

    let lock = lease::pmtui_lock_path(&registry, socket);
    std::fs::write(&lock, "not-json").unwrap();
    assert!(takeover::force_owner_and_wait_with(&registry, socket, &owner, |_| Ok(())).is_err());
    std::fs::write(&lock, serde_json::to_vec(&owner).unwrap()).unwrap();

    drop(ownership);
    // The force path probes the lock once; a sibling test's fork→exec must not keep it held.
    wait_until_free(&lock);
    let free_error =
        takeover::force_owner_and_wait_with(&registry, socket, &owner, |_| Ok(())).unwrap_err();
    assert!(free_error.to_string().contains("no longer held"));
}

#[test]
fn production_force_takeover_terminates_the_revalidated_owner() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-real-force";
    let mut child = ChildGuard(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::takeover::dashboard_lock_holder_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("PM_TAKEOVER_HELPER_REGISTRY", &registry)
            .env("PM_TAKEOVER_HELPER_SOCKET", socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn dashboard lock holder"),
    );
    let owner = (0..100)
        .find_map(|_| {
            let owner = takeover::read_owner(&registry, socket).ok().flatten();
            if owner
                .as_ref()
                .is_some_and(|owner| owner.pid == child.0.id())
            {
                owner
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                None
            }
        })
        .expect("helper published its dashboard owner metadata");

    let replacement = takeover::force_owner_and_wait(&registry, socket, &owner)
        .unwrap()
        .expect("SIGTERM releases the old dashboard lock");

    let status = child.0.wait().expect("reap dashboard lock holder");
    assert!(!status.success(), "helper should exit from SIGTERM");
    assert_ne!(replacement.owner.nonce, owner.nonce);
}

#[test]
fn owner_metadata_rejects_empty_malformed_and_oversized_content() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-owner-errors";
    let lock = lease::pmtui_lock_path(&registry, socket);

    assert!(takeover::read_owner(&registry, socket).unwrap().is_none());
    std::fs::write(&lock, "").unwrap();
    assert!(takeover::read_owner(&registry, socket).unwrap().is_none());
    std::fs::write(&lock, "not-json").unwrap();
    assert!(takeover::read_owner(&registry, socket).is_err());
    std::fs::write(&lock, vec![b'x'; 4097]).unwrap();
    assert!(
        takeover::read_owner(&registry, socket)
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );

    let directory_socket = "takeover-owner-directory";
    let directory_lock = lease::pmtui_lock_path(&registry, directory_socket);
    std::fs::create_dir(&directory_lock).unwrap();
    assert!(
        takeover::read_owner(&registry, directory_socket)
            .unwrap_err()
            .to_string()
            .contains("read")
    );
}

#[test]
fn takeover_files_report_path_and_cleanup_errors_without_weakening_the_lock() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "file").unwrap();
    let invalid_registry = blocker.join("registry.json");
    assert!(takeover::read_owner(&invalid_registry, "broken").is_err());
    assert!(takeover::claim(&invalid_registry, "broken").is_err());
    assert!(takeover::wait_for_handoff(&invalid_registry, "broken").is_err());

    let registry = dir.path().join("registry.json");
    let socket = "takeover-cleanup-error";
    let request = lease::pmtui_takeover_path(&registry, socket);
    std::fs::create_dir(&request).unwrap();
    assert!(takeover::claim(&registry, socket).is_err());

    std::fs::remove_dir(&request).unwrap();
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the cleanup-test lock");
    std::fs::write(&request, "not-json").unwrap();
    assert!(!takeover::requested_for(&registry, socket, "owner"));
    assert!(takeover::cancel_handoff(&registry, socket, &ownership.owner).is_err());
}

#[test]
fn request_cleanup_accepts_present_and_missing_files_but_rejects_directories() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let request = dir.path().join("takeover.json");

    takeover::clear_request_path(&request).unwrap();
    std::fs::write(&request, "request").unwrap();
    takeover::clear_request_path(&request).unwrap();
    assert!(!request.exists());
    std::fs::create_dir(&request).unwrap();
    assert!(takeover::clear_request_path(&request).is_err());
}

#[test]
fn production_force_reports_a_nonexistent_recorded_pid() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-invalid-pid";
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the lock");
    let mut exited = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn short-lived child");
    let exited_pid = exited.id();
    assert!(exited.wait().unwrap().success());
    let mut invalid = ownership.owner.clone();
    invalid.pid = exited_pid;
    std::fs::write(
        lease::pmtui_lock_path(&registry, socket),
        serde_json::to_vec(&invalid).unwrap(),
    )
    .unwrap();

    let error = takeover::force_owner_and_wait(&registry, socket, &invalid).unwrap_err();

    assert!(error.to_string().contains("kill -TERM"));
}

#[test]
fn production_force_rejects_process_group_and_init_pid_values() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-unsafe-pid";
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the lock");
    for unsafe_pid in [0, 1, u32::MAX] {
        let mut owner = ownership.owner.clone();
        owner.pid = unsafe_pid;
        let error = takeover::force_owner_and_wait(&registry, socket, &owner).unwrap_err();
        assert!(error.to_string().contains("unsafe recorded"));
    }
}

#[test]
fn confirmed_startup_handoff_acquires_only_after_confirmation() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-confirm";
    let first = takeover::claim(&registry, socket)
        .unwrap()
        .expect("first dashboard owns the lock");
    let mut first = Some(first);
    let mut prompts = Vec::new();

    let second = confirmed_dashboard_takeover(
        &registry,
        socket,
        &mut |prompt| {
            prompts.push(prompt.to_string());
            drop(first.take());
            Ok(true)
        },
        &mut |_, _, _| unreachable!("clean handoff should not force"),
    )
    .unwrap()
    .expect("confirmed handoff acquires the released lock");

    assert_eq!(prompts, ["Stop it cleanly and take over? [y/N] "]);
    assert_eq!(
        takeover::read_owner(&registry, socket).unwrap(),
        Some(second.owner.clone())
    );
}

#[test]
fn declined_startup_handoff_keeps_the_current_owner() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-decline";
    let first = takeover::claim(&registry, socket)
        .unwrap()
        .expect("first dashboard owns the lock");

    let second =
        confirmed_dashboard_takeover(&registry, socket, &mut |_| Ok(false), &mut |_, _, _| {
            unreachable!("declined handoff should not force")
        })
        .unwrap();

    assert!(second.is_none());
    assert_eq!(
        takeover::read_owner(&registry, socket).unwrap(),
        Some(first.owner.clone())
    );
}

#[test]
fn startup_handoff_handles_missing_and_malformed_owner_metadata() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-ownerless-startup";
    let lock_path = lease::pmtui_lock_path(&registry, socket);
    let _lock = lease::try_acquire(&lock_path)
        .unwrap()
        .expect("raw dashboard lock is free");
    let mut confirm = unexpected_confirm;
    let mut force = unexpected_force;
    assert!(
        confirmed_dashboard_takeover(&registry, socket, &mut confirm, &mut force)
            .unwrap()
            .is_none()
    );

    std::fs::write(&lock_path, "not-json").unwrap();
    assert!(
        confirmed_dashboard_takeover(&registry, socket, &mut confirm, &mut force)
            .unwrap()
            .is_none()
    );
}

#[test]
fn startup_handoff_cancels_a_timed_out_clean_request_when_force_is_declined() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-force-decline";
    let first = takeover::claim(&registry, socket)
        .unwrap()
        .expect("first dashboard owns the lock");
    let mut answers = [true, false].into_iter();

    let second = confirmed_dashboard_takeover(
        &registry,
        socket,
        &mut |_| Ok(answers.next().expect("two prompts")),
        &mut |_, _, _| unreachable!("declined force must not signal"),
    )
    .unwrap();

    assert!(second.is_none());
    assert!(!takeover::requested_for(
        &registry,
        socket,
        &first.owner.nonce
    ));
}

#[test]
fn startup_handoff_rechecks_a_naturally_released_lock_before_force() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-late-clean-exit";
    let first = takeover::claim(&registry, socket)
        .unwrap()
        .expect("first dashboard owns the lock");
    let old_nonce = first.owner.nonce.clone();
    let mut first = Some(first);
    let mut prompt = 0;

    let second = confirmed_dashboard_takeover(
        &registry,
        socket,
        &mut |_| {
            prompt += 1;
            if prompt == 2 {
                drop(first.take());
            }
            Ok(true)
        },
        &mut |_, _, _| unreachable!("free lock should be claimed before force"),
    )
    .unwrap()
    .expect("late clean exit still hands over");

    assert_ne!(second.owner.nonce, old_nonce);
}

#[test]
fn startup_force_result_and_error_paths_preserve_single_ownership() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");

    let success_socket = "takeover-force-success";
    let first = takeover::claim(&registry, success_socket)
        .unwrap()
        .expect("first dashboard owns the lock");
    let mut first = Some(first);
    let mut answers = [true, true].into_iter();
    let replacement = confirmed_dashboard_takeover(
        &registry,
        success_socket,
        &mut |_| Ok(answers.next().expect("two prompts")),
        &mut |registry, socket, _| {
            drop(first.take());
            takeover::wait_for_handoff(registry, socket)
        },
    )
    .unwrap()
    .expect("injected force releases and transfers the lock");
    assert!(replacement.owner.pid > 1);

    let error_socket = "takeover-force-error";
    let first = takeover::claim(&registry, error_socket)
        .unwrap()
        .expect("first dashboard owns the error-path lock");
    let mut answers = [true, true].into_iter();
    let error = confirmed_dashboard_takeover(
        &registry,
        error_socket,
        &mut |_| Ok(answers.next().expect("two prompts")),
        &mut |_, _, _| anyhow::bail!("forced handoff failed"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("forced handoff failed"));
    assert!(!takeover::requested_for(
        &registry,
        error_socket,
        &first.owner.nonce
    ));

    let none_socket = "takeover-force-none";
    let first = takeover::claim(&registry, none_socket)
        .unwrap()
        .expect("first dashboard owns the no-release lock");
    let mut answers = [true, true].into_iter();
    let none = confirmed_dashboard_takeover(
        &registry,
        none_socket,
        &mut |_| Ok(answers.next().expect("two prompts")),
        &mut |_, _, _| Ok(None),
    )
    .unwrap();
    assert!(none.is_none());
    assert!(!takeover::requested_for(
        &registry,
        none_socket,
        &first.owner.nonce
    ));
}

#[test]
fn test_confirmation_adapter_is_noninteractive() {
    assert!(!confirm_terminal("ignored").unwrap());
}

#[test]
fn startup_wrapper_runs_the_dashboard_after_a_confirmed_clean_handoff() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-start-app";
    let first = takeover::claim(&registry, socket)
        .unwrap()
        .expect("first dashboard owns the lock");
    let mut first = Some(first);
    let mut dashboard_called = false;

    start_app_with_takeover(
        Args {
            registry: registry.clone(),
            socket: socket.into(),
            help: false,
        },
        &mut |app| {
            dashboard_called = true;
            assert!(app.dashboard_owner_nonce.is_some());
            Ok(())
        },
        &mut |_| {
            drop(first.take());
            Ok(true)
        },
        &mut |_, _, _| unreachable!("clean handoff should not force"),
    )
    .unwrap();

    assert!(dashboard_called);
}

#[test]
fn run_loop_exits_on_a_confirmed_takeover_idle_tick() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let socket = "takeover-run-loop";
    let ownership = takeover::claim(&registry, socket)
        .unwrap()
        .expect("dashboard owns the lock");
    let mut app = app_with(Vec::new(), UiMode::Normal);
    app.registry_path = registry.clone();
    app.socket = socket.into();
    app.dashboard_owner_nonce = Some(ownership.owner.nonce.clone());
    takeover::request_handoff(&registry, socket, &ownership.owner).unwrap();

    run_loop(&mut app, |_| Ok(false)).unwrap();

    assert!(app.should_quit);
}

#[test]
fn startup_confirmation_and_request_errors_preserve_the_owner() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");

    let confirm_socket = "takeover-confirm-error";
    let confirm_owner = takeover::claim(&registry, confirm_socket)
        .unwrap()
        .expect("dashboard owns the confirm-error lock");
    let error = confirmed_dashboard_takeover(
        &registry,
        confirm_socket,
        &mut |_| anyhow::bail!("confirmation failed"),
        &mut |_, _, _| unreachable!("confirmation error must not force"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("confirmation failed"));
    drop(confirm_owner);

    let request_socket = "takeover-request-error";
    let request_owner = takeover::claim(&registry, request_socket)
        .unwrap()
        .expect("dashboard owns the request-error lock");
    std::fs::create_dir(lease::pmtui_takeover_path(&registry, request_socket)).unwrap();
    assert!(
        confirmed_dashboard_takeover(
            &registry,
            request_socket,
            &mut |_| Ok(true),
            &mut |_, _, _| unreachable!("request error must not force"),
        )
        .is_err()
    );
    drop(request_owner);
}

#[test]
fn startup_force_prompt_and_recheck_errors_do_not_start_a_rival() {
    let _serial = serial_takeover_test();
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");

    let prompt_socket = "takeover-second-confirm-error";
    let prompt_owner = takeover::claim(&registry, prompt_socket)
        .unwrap()
        .expect("dashboard owns the prompt-error lock");
    let mut prompt = 0;
    let error = confirmed_dashboard_takeover(
        &registry,
        prompt_socket,
        &mut |_| {
            prompt += 1;
            if prompt == 1 {
                Ok(true)
            } else {
                anyhow::bail!("second confirmation failed")
            }
        },
        &mut |_, _, _| unreachable!("prompt error must not force"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("second confirmation failed"));
    assert!(!takeover::requested_for(
        &registry,
        prompt_socket,
        &prompt_owner.owner.nonce
    ));
    drop(prompt_owner);

    let recheck_socket = "takeover-recheck-error";
    let recheck_owner = takeover::claim(&registry, recheck_socket)
        .unwrap()
        .expect("dashboard owns the recheck-error lock");
    let lock_path = lease::pmtui_lock_path(&registry, recheck_socket);
    let mut prompt = 0;
    let error = confirmed_dashboard_takeover(
        &registry,
        recheck_socket,
        &mut |_| {
            prompt += 1;
            if prompt == 2 {
                std::fs::remove_file(&lock_path).unwrap();
                std::fs::create_dir(&lock_path).unwrap();
            }
            Ok(true)
        },
        &mut |_, _, _| unreachable!("failed recheck must not force"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("single-instance lock"));
    drop(recheck_owner);
}
