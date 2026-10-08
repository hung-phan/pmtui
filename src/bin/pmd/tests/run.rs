use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use agent_manager::lease::{
    acquire_with_retry, daemon_lock_path, daemon_stop_path, is_held, try_acquire,
};
use agent_manager::registry::Registry;
use agent_manager::tmux::{Driver, StepHandle};

use super::*;

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

struct TestSocket {
    name: String,
    owner_lock: PathBuf,
}

#[derive(Clone, Copy)]
struct TestDriver;

impl Driver for TestDriver {
    fn spawn_step(
        &self,
        _session: &str,
        _cwd: &std::path::Path,
        _command: &[String],
        _done_signal: &std::path::Path,
        _log: &std::path::Path,
    ) -> Result<StepHandle> {
        anyhow::bail!("unexpected spawn_step call")
    }

    fn is_alive(&self, _session: &str) -> Result<bool> {
        anyhow::bail!("unexpected is_alive call")
    }

    fn capture_tail(&self, _session: &str, _lines: usize) -> Result<String> {
        anyhow::bail!("unexpected capture_tail call")
    }

    fn terminate(&self, _session: &str) -> Result<()> {
        anyhow::bail!("unexpected terminate call")
    }
}

fn reap_no_sessions(_driver: &TestDriver) -> usize {
    0
}

fn sweep_no_orphans(_driver: &TestDriver, _registry: &Registry) -> usize {
    0
}

impl TestSocket {
    fn new(label: &str, lock_root: &std::path::Path) -> Self {
        Self::with_name(
            format!(
                "pmd-{label}-{}-{}",
                std::process::id(),
                NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
            ),
            lock_root,
        )
    }

    fn with_name(name: String, lock_root: &std::path::Path) -> Self {
        Self {
            name,
            owner_lock: lock_root.join("socket-owner.lock"),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn owner_lock(&self) -> &std::path::Path {
        &self.owner_lock
    }
}

fn args(registry: PathBuf, socket: &TestSocket, once: bool) -> Args {
    Args {
        registry,
        socket: socket.name().to_string(),
        tick_ms: 1,
        once,
    }
}

fn run_for_test(args: Args, socket: &TestSocket, deadline: Option<Instant>) -> Result<()> {
    run_with_control(
        args,
        socket.owner_lock().to_path_buf(),
        deadline,
        RunControl {
            driver: TestDriver,
            reap_owned_sessions: reap_no_sessions,
            sweep_orphan_loops: sweep_no_orphans,
        },
    )
}

fn lock_is_free(path: &std::path::Path) -> bool {
    acquire_with_retry(path, 50, Duration::from_millis(10))
        .unwrap()
        .is_some()
}

#[test]
fn one_shot_run_creates_nested_lock_directory_and_releases_leases() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("nested/registry.json");
    let socket = TestSocket::new("once", dir.path());
    let args = args(registry.clone(), &socket, true);
    let daemon_lock = daemon_lock_path(&registry, socket.name());
    let owner_lock = socket.owner_lock();

    run_for_test(args, &socket, None).unwrap();

    assert!(lock_is_free(&daemon_lock));
    assert!(lock_is_free(owner_lock));
}

#[test]
fn one_shot_run_leaves_a_stop_request_for_a_continuous_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    Registry::default().save(&registry).unwrap();
    let socket = TestSocket::new("once-stop", dir.path());
    let stop = daemon_stop_path(&registry, socket.name());
    std::fs::write(&stop, "stop").unwrap();

    run_for_test(args(registry, &socket, true), &socket, None).unwrap();

    assert!(stop.exists());
}

#[test]
fn corrupt_initial_registry_reports_the_path_and_does_not_take_leases() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    std::fs::write(&registry, "{broken").unwrap();
    let socket = TestSocket::new("corrupt", dir.path());

    let error = run_for_test(args(registry.clone(), &socket, true), &socket, None).unwrap_err();

    assert!(format!("{error:#}").contains(&format!("load registry {}", registry.display())));
    assert!(!daemon_lock_path(&registry, socket.name()).exists());
}

#[test]
fn dispatch_run_propagates_startup_errors() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    std::fs::write(&registry, "{broken").unwrap();
    let socket = TestSocket::new("dispatch-error", dir.path());

    let error = dispatch(CliAction::Run(args(registry.clone(), &socket, true))).unwrap_err();

    assert!(format!("{error:#}").contains(&format!("load registry {}", registry.display())));
}

#[test]
fn daemon_lock_open_error_is_propagated() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    Registry::default().save(&registry).unwrap();
    let socket = TestSocket::with_name("x".repeat(300), dir.path());

    let error = run_for_test(args(registry, &socket, true), &socket, None).unwrap_err();

    assert!(
        matches!(
            error.downcast_ref::<std::io::Error>(),
            Some(io_error) if io_error.kind() == std::io::ErrorKind::InvalidFilename
        ),
        "unexpected lock-open error: {error:#}"
    );
}

#[test]
fn socket_owner_lock_open_error_is_propagated_and_releases_the_daemon_lock() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    Registry::default().save(&registry).unwrap();
    let socket = TestSocket::with_name("valid-socket".to_string(), dir.path());
    let socket = TestSocket {
        owner_lock: dir.path().join("x".repeat(300)),
        ..socket
    };
    let daemon_lock = daemon_lock_path(&registry, socket.name());

    let error = run_for_test(args(registry, &socket, true), &socket, None).unwrap_err();

    assert!(
        matches!(
            error.downcast_ref::<std::io::Error>(),
            Some(io_error) if io_error.kind() == std::io::ErrorKind::InvalidFilename
        ),
        "unexpected owner-lock error: {error:#}"
    );
    assert!(lock_is_free(&daemon_lock));
}

#[test]
fn existing_daemon_singleton_makes_a_second_run_exit_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    Registry::default().save(&registry).unwrap();
    let socket = TestSocket::new("singleton", dir.path());
    let lock_path = daemon_lock_path(&registry, socket.name());
    let held = try_acquire(&lock_path).unwrap().unwrap();

    run_for_test(args(registry, &socket, true), &socket, None).unwrap();

    assert_eq!(is_held(&lock_path).unwrap(), Some(true));
    drop(held);
}

#[test]
fn socket_owned_by_another_registry_makes_the_run_exit_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    Registry::default().save(&registry).unwrap();
    let socket = TestSocket::new("owner", dir.path());
    let owner_path = socket.owner_lock();
    let held = try_acquire(owner_path).unwrap().unwrap();
    let daemon_path = daemon_lock_path(&registry, socket.name());

    run_for_test(args(registry, &socket, true), &socket, None).unwrap();

    assert_eq!(is_held(owner_path).unwrap(), Some(true));
    assert_eq!(is_held(&daemon_path).unwrap(), Some(false));
    drop(held);
}

#[test]
fn reload_replaces_the_registry_only_after_a_successful_parse() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    let mut current = Registry::default();
    std::fs::write(
        &registry,
        r#"{"projects":[{"id":"fresh","root":"/tmp/fresh","enabled":false}]}"#,
    )
    .unwrap();

    reload_registry(&registry, &mut current);
    assert_eq!(current.projects[0].id, "fresh");

    std::fs::write(&registry, "{broken").unwrap();
    reload_registry(&registry, &mut current);
    assert_eq!(current.projects[0].id, "fresh");
}

#[test]
fn startup_cleanup_messages_name_only_work_that_was_performed() {
    assert_eq!(
        startup_cleanup_message(StartupCleanup::StaleSupervisors, 2),
        Some("pmd: startup reaped 2 stale supervisor session(s)".to_string())
    );
    assert_eq!(
        startup_cleanup_message(StartupCleanup::OrphanProjects, 3),
        Some("pmd: startup swept 3 orphan project terminal(s)".to_string())
    );
    assert_eq!(
        startup_cleanup_message(StartupCleanup::StaleSupervisors, 0),
        None
    );

    assert!(report_startup_cleanup(StartupCleanup::StaleSupervisors, 1));
    assert!(report_startup_cleanup(StartupCleanup::OrphanProjects, 1));
    assert!(!report_startup_cleanup(StartupCleanup::StaleSupervisors, 0));
}

#[test]
fn continuous_run_removes_a_stale_stop_then_consumes_a_new_request() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry.json");
    Registry::default().save(&registry).unwrap();
    let socket = TestSocket::new("continuous", dir.path());
    let stop = daemon_stop_path(&registry, socket.name());
    std::fs::write(&stop, "stale").unwrap();
    let writer_stop = stop.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    let writer = std::thread::spawn(move || {
        while writer_stop.exists() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(writer_stop, "new").unwrap();
        true
    });

    let result = run_for_test(args(registry, &socket, false), &socket, Some(deadline));
    let stale_stop_was_removed = writer.join().unwrap();

    assert!(
        stale_stop_was_removed,
        "continuous pmd did not remove the stale stop request before the deadline"
    );
    result.unwrap();
    assert!(!stop.exists());
}
