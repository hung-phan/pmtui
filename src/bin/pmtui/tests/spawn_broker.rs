//! The dashboard spawn broker: a request an agent published in its own session's state directory
//! becomes one Standard child session.
//!
//! Each `step_spawns` call advances a request by at most one phase and never waits, a request's
//! Message is launched at most once, and a dashboard that starts over a half-finished request
//! resumes it from the row's launch state instead of launching again.

use super::*;
use agent_manager::registry::{LaunchRecord, LaunchState, MAX_CHILDREN_PER_PARENT, SpawnOutcome};
use agent_manager::spawn::{
    self, ErrorCode, NextActionKind, ReceiptState, SpawnArgs, SpawnReceipt, SpawnRequest,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

const REQ: &str = "550e8400-e29b-41d4-a716-446655440000";
const MESSAGE: &str = "Fix the flaky fork test\n\nTarget: src/bin/pmtui/tests/forking.rs";

fn req_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn args(message: &str) -> SpawnArgs {
    SpawnArgs {
        message: message.into(),
        title: None,
        name: None,
        dir: None,
        agent: None,
        model: None,
    }
}

fn request(parent: &str, id: &str, args: SpawnArgs) -> SpawnRequest {
    SpawnRequest {
        schema_version: spawn::SCHEMA_VERSION,
        request_id: id.into(),
        parent_session: parent.into(),
        created_at: 1_000,
        args,
    }
}

/// The codex first-launch trust dialog, as captured: a mid-line question and a press-enter footer.
const CODEX_TRUST_DIALOG: &str = concat!(
    "  Do you trust the contents of this directory? Working with untrusted contents\n",
    "  comes with higher risk of prompt injection. Trusting the directory allows\n",
    "  project-local config, hooks, and exec policies to load.\n",
    "\n",
    "\u{203a} 1. Yes, continue\n",
    "  2. No, quit\n",
    "\n",
    "  Press enter to continue\n",
);

struct Broker {
    _dir: tempfile::TempDir,
    base: PathBuf,
    app: App,
    pane: FakePane,
    registry: PathBuf,
    /// The parent `service`'s root, canonical, and the child `service-2`'s too.
    root: PathBuf,
    clock: Rc<Cell<u128>>,
}

/// A dashboard over a registry whose one row, `service`, is a Claude parent in `<tmp>/service`.
/// `git -C <dir> <args…>`, which must succeed.
fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Make `root` a repository with one commit, independent of the machine's git identity.
fn init_repo(root: &Path) {
    run_git(root, &["init", "--quiet", "-b", "main"]);
    run_git(root, &["config", "user.email", "pm@example.invalid"]);
    run_git(root, &["config", "user.name", "pm tests"]);
    run_git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("README.md"), "start\n").expect("write");
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "--quiet", "-m", "start"]);
}

fn broker() -> Broker {
    broker_with(|_, _| {})
}

/// [`broker`] with the fake terminal adjusted before it is shared. `child` is the tmux session of
/// the first child a request in the parent's folder creates (`service-2`).
fn broker_with(adjust: impl FnOnce(&mut PaneInner, &str)) -> Broker {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (registry, root) = reg_with_agent_loop(&base, "service");
    let child = session_name("service-2", &root);
    let pane = FakePane::default().with(|inner| adjust(inner, &child));
    let (app, clock) = dashboard(&registry, &pane);
    Broker {
        _dir: dir,
        base,
        app,
        pane,
        registry,
        root,
        clock,
    }
}

/// A dashboard on `registry` that holds its dashboard singleton, whose broker reads a clock the
/// test moves by hand.
fn dashboard(registry: &Path, pane: &FakePane) -> (App, Rc<Cell<u128>>) {
    let mut app = app_with_driver(Vec::new(), UiMode::Normal, Box::new(pane.clone()));
    app.registry_path = registry.to_path_buf();
    app.dashboard_owner_nonce = Some("test-owner".into());
    let clock = Rc::new(Cell::new(10_000));
    let now = clock.clone();
    app.spawn.now_ms = Box::new(move || now.get());
    (app, clock)
}

impl Broker {
    fn requests(&self) -> PathBuf {
        spawn::requests_dir(&ProjectPaths::for_session(&self.root, "service").state_dir())
    }

    fn publish(&self, id: &str, args: SpawnArgs) {
        let published =
            spawn::publish_request(&self.requests(), &request("service", id, args)).unwrap();
        assert_eq!(published, spawn::Publish::Published);
    }

    fn receipt(&self, id: &str) -> Option<SpawnReceipt> {
        spawn::read_receipt(&self.requests(), id).unwrap()
    }

    fn state(&self, id: &str) -> Option<ReceiptState> {
        self.receipt(id).map(|receipt| receipt.state)
    }

    fn error_code(&self, id: &str) -> Option<ErrorCode> {
        self.receipt(id)
            .and_then(|receipt| receipt.error)
            .map(|error| error.code)
    }

    fn rows(&self) -> Vec<ProjectEntry> {
        Registry::load(&self.registry).unwrap().projects
    }

    fn row(&self, id: &str) -> Option<ProjectEntry> {
        self.rows().into_iter().find(|entry| entry.id == id)
    }

    fn update_row(&self, id: &str, change: impl FnOnce(&mut ProjectEntry)) {
        Registry::update(&self.registry, |registry| {
            change(
                registry
                    .projects
                    .iter_mut()
                    .find(|entry| entry.id == id)
                    .expect("row present"),
            )
        })
        .unwrap();
    }

    fn child_session(&self) -> String {
        session_name("service-2", &self.root)
    }

    fn child_state_dir(&self) -> PathBuf {
        ProjectPaths::for_session(&self.root, "service-2").state_dir()
    }

    /// One dashboard frame's broker step, 150 ms after the previous one.
    fn step(&mut self) {
        self.clock.set(self.clock.get() + 150);
        self.app.step_spawns();
    }

    /// Step until no request is in flight.
    fn settle(&mut self) {
        for _ in 0..60 {
            self.step();
            if self.app.spawn.jobs.is_empty() {
                return;
            }
        }
        panic!("the broker never settled; status: {}", self.app.status);
    }

    /// What `refresh` does with the registry it just loaded.
    fn discover(&mut self) {
        let registry = Registry::load(&self.registry).unwrap();
        self.app.discover_spawn_requests(&registry);
    }

    /// A new dashboard over the same registry, as after a crash or a takeover. The old one is
    /// gone before the new one steps, so the new one takes the spawn-broker lease on its first
    /// frame (once a sibling test's fork has let go of the old descriptor).
    fn restart(&mut self, pane: FakePane) {
        let (app, clock) = dashboard(&self.registry, &pane);
        drop(std::mem::replace(&mut self.app, app));
        wait_until_free(&lease::spawn_broker_lock_path(&self.registry));
        self.clock = clock;
        self.pane = pane;
    }
}

/// Remove a request and its receipt by hand, as nothing in the product does: the row is then the
/// only record left of what the request id created.
fn remove_both(b: &Broker, id: &str) {
    std::fs::remove_file(spawn::request_path(&b.requests(), id)).unwrap();
    std::fs::remove_file(spawn::receipt_path(&b.requests(), id)).unwrap();
}

fn assert_attach(receipt: &SpawnReceipt, session: &str) {
    let action = receipt.next_action.as_ref().expect("an attach action");
    assert_eq!(action.kind, NextActionKind::Attach);
    assert!(
        action.argv[0].ends_with("tmux"),
        "an exact tmux executable: {:?}",
        action.argv
    );
    assert_eq!(
        action.argv[1..],
        [
            "-L".to_string(),
            "pm-test".into(),
            "attach-session".into(),
            "-t".into(),
            format!("={session}"),
        ]
    );
}

#[test]
fn a_valid_request_stages_a_disabled_row_then_launches_once_and_ends_ready() {
    let mut b = broker();
    let spawn_args = SpawnArgs {
        title: Some("Fix flaky fork test".into()),
        ..args(MESSAGE)
    };
    b.publish(REQ, spawn_args.clone());

    b.step();
    let claimed = b.receipt(REQ).expect("claimed before anything else");
    assert_eq!(claimed.state, ReceiptState::Claimed);
    assert_eq!(
        claimed.claimed_by,
        Some(format!("pid:{}", std::process::id()))
    );
    assert!(b.row("service-2").is_none(), "a claim stages nothing");
    assert!(!b.child_state_dir().exists());

    b.step();
    let staged = b.row("service-2").expect("a staged row");
    assert!(!staged.enabled && staged.is_staged_spawn());
    assert_eq!(staged.spawned_by.as_deref(), Some("service"));
    assert_eq!(staged.task_title.as_deref(), Some("Fix flaky fork test"));
    assert_eq!(staged.initial_prompt.as_deref(), Some(MESSAGE));
    assert_eq!(staged.engine, Some(Engine::Claude));
    assert_eq!(staged.root, b.root);
    assert_eq!(staged.conversation_id, None);
    assert_eq!(
        staged.launch,
        Some(LaunchRecord {
            request_id: REQ.into(),
            args_hash: spawn_args.args_hash(),
            state: LaunchState::Pending,
            outcome: None,
            kind: agent_manager::registry::LaunchKind::Job,
            branch: None,
            base_commit: None,
        })
    );
    let config: Config =
        state::read_json(&ProjectPaths::for_session(&b.root, "service-2").config()).unwrap();
    assert_eq!(config.autonomy, Tier::Standard, "children are Standard");
    let receipt = b.receipt(REQ).unwrap();
    assert_eq!(receipt.state, ReceiptState::Staged);
    assert_eq!(receipt.launch_state, Some(LaunchState::Pending));
    assert!(b.pane.steps().is_empty(), "staging launches nothing");

    b.step();
    let launches = b.pane.steps(); // a job child's one-shot launch
    assert_eq!(launches.len(), 1);
    let (session, cwd, argv) = &launches[0];
    assert_eq!(session, &b.child_session());
    assert_eq!(cwd, &b.root.display().to_string());
    // The prompt is the Message UNDER the job preamble, which is what tells an unattended child that
    // nobody will answer it and what to end with. The Message itself must survive intact.
    let prompt = argv.last().expect("a trailing prompt");
    assert!(prompt.ends_with(&format!("{MESSAGE}\n")), "{prompt}");
    assert!(
        prompt.contains("Nobody is at this terminal"),
        "the preamble is there: {prompt}"
    );
    let cid = argv
        .iter()
        .position(|arg| arg == "--session-id")
        .map(|at| argv[at + 1].clone())
        .expect("a fresh Claude conversation");
    // No `ManagedEnv`: a job is launched as a step and is given no way to identify itself to
    // `pmtui spawn` — see `a_job_child_is_given_no_way_to_spawn`.
    assert!(b.pane.launched_env().is_empty());
    let started = b.row("service-2").unwrap();
    assert!(started.enabled);
    assert_eq!(started.conversation_id, Some(cid));
    assert_eq!(
        started.launch.as_ref().map(|launch| launch.state),
        Some(LaunchState::Started)
    );
    assert_eq!(b.state(REQ), Some(ReceiptState::Launching));

    b.step();
    assert_eq!(
        b.state(REQ),
        Some(ReceiptState::Launching),
        "one observation is not readiness"
    );
    b.step();
    let ready = b.receipt(REQ).unwrap();
    assert_eq!(ready.state, ReceiptState::Ready);
    assert_eq!(ready.launch_state, Some(LaunchState::Started));
    assert_eq!(ready.args_hash, Some(spawn_args.args_hash()));
    assert_eq!(ready.error, None);
    assert_eq!(ready.next_action, None);
    let session = ready.session.as_ref().unwrap();
    assert_eq!(session.id, "service-2");
    assert_eq!(session.title.as_deref(), Some("Fix flaky fork test"));
    assert_eq!(session.spawned_by, "service");
    assert_eq!(session.agent, Engine::Claude);
    assert_eq!(session.root, b.root);
    assert_eq!(session.tmux_session, b.child_session());
    assert_eq!(session.state_dir, b.child_state_dir());
    let json = serde_json::to_string(&ready).unwrap();
    assert!(
        !json.contains("Target: src/bin"),
        "never echoes the Message"
    );
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().outcome,
        Some(SpawnOutcome::Ready)
    );
    assert_eq!(b.app.status, "dashboard spawned service-2 from service");
    assert!(b.app.spawn.jobs.is_empty());

    for _ in 0..5 {
        b.discover();
        b.step();
    }
    assert_eq!(
        b.pane.steps().len(),
        1,
        "a finished request never relaunches"
    );
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn a_child_inherits_dir_agent_and_model_from_its_parent() {
    let mut b = broker();
    b.update_row("service", |parent| {
        parent.engine = Some(Engine::Codex);
        parent.worker_model = Some("gpt-parent".into());
    });
    b.publish(&req_id(1), args(MESSAGE));
    b.publish(
        &req_id(2),
        SpawnArgs {
            agent: Some(Engine::Claude),
            ..args("second task")
        },
    );
    let elsewhere = b.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    b.publish(
        &req_id(3),
        SpawnArgs {
            dir: Some(elsewhere.clone()),
            model: Some("gpt-chosen".into()),
            name: Some("  Helper  ".into()),
            ..args("third task")
        },
    );
    b.settle();

    let codex = b.row("service-2").unwrap();
    assert_eq!(codex.engine, Some(Engine::Codex));
    assert_eq!(codex.worker_model.as_deref(), Some("gpt-parent"));
    assert_eq!(codex.root, b.root);
    assert_eq!(
        codex.task_title.as_deref(),
        Some("Fix the flaky fork test"),
        "an absent title is the Message's first line"
    );
    assert_eq!(codex.conversation_id, None, "codex picks its own id");
    let claude = b.row("service-3").unwrap();
    assert_eq!(claude.engine, Some(Engine::Claude));
    assert_eq!(
        claude.worker_model, None,
        "the parent's model is for its engine"
    );
    let other = b.row("elsewhere").unwrap();
    assert_eq!(other.root, elsewhere);
    assert_eq!(other.engine, Some(Engine::Codex));
    assert_eq!(other.worker_model.as_deref(), Some("gpt-chosen"));
    assert_eq!(other.display_name.as_deref(), Some("Helper"));

    let launches = b.pane.steps(); // a job child's one-shot launch
    assert_eq!(launches.len(), 3);
    assert!(launches[0].2.iter().any(|arg| arg == "codex"));
    assert!(launches[0].2.iter().any(|arg| arg == "gpt-parent"));
    assert_eq!(launches[2].1, elsewhere.display().to_string());
    for id in [req_id(1), req_id(2), req_id(3)] {
        assert_eq!(b.state(&id), Some(ReceiptState::Ready), "{id}");
    }
}

#[test]
fn a_child_may_run_in_the_exact_root_of_another_session() {
    let mut b = broker();
    let other = b.base.join("other");
    std::fs::create_dir_all(&other).unwrap();
    Registry::update(&b.registry, |registry| {
        let mut row = registry.projects[0].clone();
        row.id = "other".into();
        // Spelled with a `..` detour: the broker compares canonical roots.
        row.root = b.root.join("..").join("other");
        registry.projects.push(row);
    })
    .unwrap();
    b.publish(
        REQ,
        SpawnArgs {
            dir: Some(other.clone()),
            ..args(MESSAGE)
        },
    );
    b.settle();

    assert_eq!(b.state(REQ), Some(ReceiptState::Ready), "{}", b.app.status);
    let child = b
        .rows()
        .into_iter()
        .find(|row| row.spawned_by.as_deref() == Some("service"))
        .expect("the child row");
    assert_eq!(child.root, other);
}

/// One refused request: how to break it, and the code its receipt must carry.
struct Refused {
    what: &'static str,
    setup: fn(&Broker) -> SpawnArgs,
    between: fn(&Broker),
    code: ErrorCode,
}

fn untouched(_: &Broker) {}

/// How one case arms the fake terminal, given the child's tmux session.
type PaneAdjust = fn(&mut PaneInner, &str);

#[test]
fn validation_failures_write_failed_receipts_and_stage_nothing() {
    let cases = [
        Refused {
            what: "missing parent",
            setup: |_| args(MESSAGE),
            between: |b| {
                Registry::update(&b.registry, |registry| {
                    registry.projects.retain(|entry| entry.id != "service")
                })
                .unwrap()
            },
            code: ErrorCode::ParentNotFound,
        },
        Refused {
            what: "nested parent",
            setup: |b| {
                b.update_row("service", |parent| parent.spawned_by = Some("root".into()));
                args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::NestedSpawnRefused,
        },
        Refused {
            what: "missing dir",
            setup: |b| SpawnArgs {
                dir: Some(b.base.join("missing")),
                ..args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::DirNotFound,
        },
        Refused {
            what: "a file as dir",
            setup: |b| {
                let file = b.base.join("file");
                std::fs::write(&file, "x").unwrap();
                SpawnArgs {
                    dir: Some(file),
                    ..args(MESSAGE)
                }
            },
            between: untouched,
            code: ErrorCode::DirNotFound,
        },
        Refused {
            what: "a dir outside the parent's folder",
            setup: |b| {
                let outside = b.base.join("elsewhere");
                std::fs::create_dir_all(&outside).unwrap();
                SpawnArgs {
                    dir: Some(outside),
                    ..args(MESSAGE)
                }
            },
            between: untouched,
            code: ErrorCode::DirNotAllowed,
        },
        Refused {
            what: "the filesystem root",
            setup: |_| SpawnArgs {
                dir: Some(PathBuf::from("/")),
                ..args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::DirNotAllowed,
        },
        Refused {
            what: "a symlink in the parent's folder that leads out of it",
            setup: |b| {
                let outside = b.base.join("outside");
                std::fs::create_dir_all(&outside).unwrap();
                let escape = b.root.join("escape");
                std::os::unix::fs::symlink(&outside, &escape).unwrap();
                SpawnArgs {
                    dir: Some(escape),
                    ..args(MESSAGE)
                }
            },
            between: untouched,
            code: ErrorCode::DirNotAllowed,
        },
        Refused {
            what: "a folder inside another session's root",
            setup: |b| {
                let other = b.base.join("other");
                std::fs::create_dir_all(other.join("sub")).unwrap();
                Registry::update(&b.registry, |registry| {
                    let mut row = registry.projects[0].clone();
                    row.id = "other".into();
                    row.root = other.clone();
                    registry.projects.push(row);
                })
                .unwrap();
                SpawnArgs {
                    dir: Some(other.join("sub")),
                    ..args(MESSAGE)
                }
            },
            between: untouched,
            code: ErrorCode::DirNotAllowed,
        },
        Refused {
            what: "relative dir",
            setup: |_| SpawnArgs {
                dir: Some(PathBuf::from("service")),
                ..args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::InvalidArgument,
        },
        Refused {
            what: "bad name",
            setup: |_| SpawnArgs {
                name: Some("bad\u{7}name".into()),
                ..args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::InvalidArgument,
        },
        Refused {
            what: "long title",
            setup: |_| SpawnArgs {
                title: Some("t".repeat(spawn::TITLE_MAX_CHARS + 1)),
                ..args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::InvalidArgument,
        },
        Refused {
            what: "control title",
            setup: |_| SpawnArgs {
                title: Some("tab\there".into()),
                ..args(MESSAGE)
            },
            between: untouched,
            code: ErrorCode::InvalidArgument,
        },
        Refused {
            what: "empty message",
            setup: |_| args(" \n\t "),
            between: untouched,
            code: ErrorCode::InvalidArgument,
        },
        Refused {
            what: "over-budget message",
            setup: |_| args(&"x".repeat(tmux::LAUNCH_COMMAND_MAX_BYTES)),
            between: untouched,
            code: ErrorCode::MessageTooLong,
        },
        Refused {
            what: "invisible message",
            setup: |_| args("\u{7}"),
            between: untouched,
            code: ErrorCode::InvalidArgument,
        },
        Refused {
            what: "unwritable dir",
            setup: |b| {
                use std::os::unix::fs::PermissionsExt;
                let locked = b.root.join("locked");
                std::fs::create_dir_all(&locked).unwrap();
                std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
                SpawnArgs {
                    dir: Some(locked),
                    ..args(MESSAGE)
                }
            },
            between: untouched,
            code: ErrorCode::LaunchFailed,
        },
        Refused {
            what: "unreadable session list",
            setup: |_| args(MESSAGE),
            between: |b| std::fs::write(&b.registry, "{").unwrap(),
            code: ErrorCode::RegistryUnreadable,
        },
    ];
    for case in cases {
        let mut b = broker();
        let spawn_args = (case.setup)(&b);
        b.publish(REQ, spawn_args);
        b.step();
        assert_eq!(b.state(REQ), Some(ReceiptState::Claimed), "{}", case.what);
        (case.between)(&b);
        let between = std::fs::read(&b.registry).unwrap();
        b.settle();

        let receipt = b.receipt(REQ).unwrap();
        assert_eq!(receipt.state, ReceiptState::Failed, "{}", case.what);
        assert_eq!(
            receipt.error.as_ref().map(|error| error.code),
            Some(case.code),
            "{}: {:?}",
            case.what,
            receipt.error
        );
        assert_eq!(receipt.session, None, "{}", case.what);
        assert_eq!(
            std::fs::read(&b.registry).unwrap(),
            between,
            "{}: the session list is untouched",
            case.what
        );
        assert!(b.pane.steps().is_empty(), "{}", case.what);
        assert!(
            !b.child_state_dir().exists(),
            "{}: nothing reserved",
            case.what
        );
        assert!(
            b.app
                .status
                .starts_with("spawn request from service failed:"),
            "{}: {}",
            case.what,
            b.app.status
        );
    }
}

#[test]
fn invalid_and_skipped_entries_never_stage() {
    use std::os::unix::fs::symlink;

    let mut b = broker();
    let requests = b.requests();
    std::fs::create_dir_all(&requests).unwrap();
    let body = serde_json::to_vec(&request("service", &req_id(1), args(MESSAGE))).unwrap();
    let outside = b.base.join("outside.json");
    std::fs::write(&outside, &body).unwrap();
    symlink(&outside, spawn::request_path(&requests, &req_id(1))).unwrap();
    let mut oversize = serde_json::to_vec(&request("service", &req_id(2), args(MESSAGE))).unwrap();
    oversize.extend(std::iter::repeat_n(b' ', spawn::MAX_REQUEST_BYTES as usize));
    std::fs::write(spawn::request_path(&requests, &req_id(2)), oversize).unwrap();
    spawn::publish_request(&requests, &request("other", &req_id(3), args(MESSAGE))).unwrap();
    std::fs::write(spawn::request_path(&requests, &req_id(4)), "{").unwrap();
    std::fs::write(requests.join("not-a-uuid.request.json"), &body).unwrap();

    b.settle();
    b.discover();
    b.settle();

    for n in 1..=4 {
        assert_eq!(b.state(&req_id(n)), Some(ReceiptState::Failed), "{n}");
        assert_eq!(
            b.receipt(&req_id(n)).unwrap().args_hash,
            None,
            "{n}: unreadable content has no hash"
        );
        assert_eq!(
            b.error_code(&req_id(n)),
            Some(ErrorCode::InvalidRequest),
            "{n}"
        );
    }
    assert!(!requests.join("not-a-uuid.receipt.json").exists());
    assert_eq!(b.rows().len(), 1, "no row appears");
    assert!(b.pane.steps().is_empty());
    let skipped: Vec<String> = log_lines(&b.app)
        .into_iter()
        .filter(|line| line.contains("not-a-uuid.request.json"))
        .collect();
    assert_eq!(skipped.len(), 1, "logged once: {:?}", log_lines(&b.app));
}

#[test]
fn an_invalid_entry_never_overwrites_the_receipt_of_a_row_it_created() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.settle();
    std::fs::remove_file(spawn::receipt_path(&b.requests(), REQ)).unwrap();
    std::fs::write(spawn::request_path(&b.requests(), REQ), "{").unwrap();
    b.restart(FakePane::default());
    b.settle();
    assert_eq!(
        b.receipt(REQ),
        None,
        "a live child is never reported as failed"
    );
    assert!(b.pane.steps().is_empty());
}

/// A JOB'S LAUNCH FAILURE IS AMBIGUOUS, whatever its cause. `spawn_step` hands tmux a wrapper script,
/// so the dashboard cannot prove from a failure that nothing read the Message — and a Message must
/// never be delivered twice. "Nothing ran" is proven LATER instead, by the exit code the wrapper
/// records: 127 is a missing executable, named as such by the retirement sweep
/// (`spawn_jobs::a_missing_agent_executable_is_named`).
#[test]
fn a_job_launch_failure_is_ambiguous_and_never_relaunched() {
    for error in [
        tmux::LaunchError::NotOnPath("claude".into()),
        tmux::LaunchError::CommandTooLong {
            session: "service-2".into(),
            bytes: 20_000,
        },
    ] {
        let mut b = broker();
        b.pane.arm_launch_error(&b.child_session(), error.clone());
        b.publish(REQ, args(MESSAGE));
        b.settle();

        let receipt = b.receipt(REQ).unwrap();
        assert_eq!(receipt.state, ReceiptState::OutcomeUnknown, "{error:?}");
        assert_eq!(receipt.launch_state, Some(LaunchState::Attempted));
        let failure = receipt.error.unwrap();
        assert_eq!(failure.code, ErrorCode::LaunchFailed);
        assert!(
            b.row("service-2").is_some_and(|row| row.enabled),
            "{error:?}: the row is kept and enabled for a human, never relaunched"
        );
        assert!(
            b.child_state_dir().exists(),
            "{error:?}: an ambiguous launch keeps its reservation"
        );
        assert_eq!(b.pane.steps().len(), 1, "one attempt, never retried");

        b.publish(&req_id(9), args(MESSAGE));
        b.discover();
        b.settle();
        assert_eq!(b.state(&req_id(9)), Some(ReceiptState::Ready));
        assert!(
            b.row("service-2").is_some(),
            "a new request id may try again"
        );
        assert_eq!(b.pane.steps().len(), 2);
    }
}

#[test]
fn ambiguous_launch_keeps_an_enabled_row_as_outcome_unknown_and_never_relaunches() {
    let cases: [(&str, PaneAdjust); 3] = [
        ("new-session failed", |inner, child| {
            inner.armed_launch_errors.get_mut().unwrap().insert(
                child.into(),
                tmux::LaunchError::NewSessionFailed("tmux new-session failed".into()),
            );
        }),
        ("exited after start", |inner, child| {
            inner.armed_launch_errors.get_mut().unwrap().insert(
                child.into(),
                tmux::LaunchError::ExitedAfterStart("the agent exited at startup".into()),
            );
        }),
        ("already alive", |inner, child| {
            inner.alive.get_mut().unwrap().insert(child.into());
        }),
    ];
    for (what, adjust) in cases {
        let mut b = broker_with(adjust);
        b.publish(REQ, args(MESSAGE));
        b.settle();

        let receipt = b.receipt(REQ).unwrap();
        assert_eq!(receipt.state, ReceiptState::OutcomeUnknown, "{what}");
        assert_eq!(receipt.launch_state, Some(LaunchState::Attempted), "{what}");
        assert_eq!(
            receipt.error.as_ref().map(|error| error.code),
            Some(ErrorCode::LaunchFailed),
            "{what}"
        );
        assert_attach(&receipt, &b.child_session());
        let row = b.row("service-2").expect("the row stays for a human");
        assert!(row.enabled && !row.is_staged_spawn(), "{what}");
        let launch = row.launch.unwrap();
        assert_eq!(launch.state, LaunchState::Attempted, "{what}");
        assert_eq!(launch.outcome, Some(SpawnOutcome::OutcomeUnknown), "{what}");
        assert_eq!(b.pane.steps().len(), 1, "{what}");

        for _ in 0..3 {
            b.discover();
            b.step();
        }
        assert_eq!(b.pane.steps().len(), 1, "{what}: never relaunched");
        b.restart(FakePane::default());
        b.settle();
        assert!(b.pane.steps().is_empty(), "{what}: nor by a new dashboard");
        assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown), "{what}");
    }
}

/// Run a fresh request to the staged row, then drop that dashboard as if it crashed.
fn staged_then_crashed() -> Broker {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.step();
    b.step();
    assert_eq!(b.state(REQ), Some(ReceiptState::Staged));
    b
}

fn set_launch_state(b: &Broker, state: LaunchState) {
    b.update_row("service-2", |row| {
        row.launch.as_mut().unwrap().state = state
    });
}

#[test]
fn recovery_from_attempted_without_terminal_is_outcome_unknown_without_a_second_launch() {
    let mut b = staged_then_crashed();
    set_launch_state(&b, LaunchState::Attempted);
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty(), "the Message is never resent");
    let receipt = b.receipt(REQ).unwrap();
    assert_eq!(receipt.state, ReceiptState::OutcomeUnknown);
    assert_eq!(
        receipt.error.map(|error| error.code),
        Some(ErrorCode::ReadinessUnknown)
    );
    let row = b.row("service-2").unwrap();
    assert!(row.enabled, "a human can see and inspect it");
    let launch = row.launch.unwrap();
    assert_eq!(launch.state, LaunchState::Attempted);
    assert_eq!(launch.outcome, Some(SpawnOutcome::OutcomeUnknown));
}

#[test]
fn recovery_from_attempted_with_a_live_terminal_promotes_without_launching() {
    let mut b = staged_then_crashed();
    set_launch_state(&b, LaunchState::Attempted);
    let child = b.child_session();
    b.restart(FakePane::live_quiet(&child));
    b.settle();

    assert!(b.pane.steps().is_empty());
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    let row = b.row("service-2").unwrap();
    assert!(row.enabled);
    assert_eq!(row.launch.unwrap().state, LaunchState::Started);
}

#[test]
fn recovery_from_pending_launches_exactly_once() {
    let mut b = staged_then_crashed();
    let crashed = b.pane.clone();
    b.restart(FakePane::default());
    b.settle();

    assert!(crashed.launches().is_empty());
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert_eq!(b.rows().len(), 2, "the staged row is reused, not restaged");
}

#[test]
fn recovery_from_failed_before_start_discards() {
    let mut b = staged_then_crashed();
    set_launch_state(&b, LaunchState::FailedBeforeStart);
    assert!(b.child_state_dir().exists());
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty());
    assert!(b.row("service-2").is_none());
    assert!(!b.child_state_dir().exists());
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::LaunchFailed));
}

#[test]
fn recovery_from_started_finishes_readiness_without_launching() {
    let mut b = staged_then_crashed();
    b.step();
    assert_eq!(b.pane.steps().len(), 1);
    let child = b.child_session();
    b.restart(FakePane::live_quiet(&child));
    b.settle();

    assert!(b.pane.steps().is_empty());
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().outcome,
        Some(SpawnOutcome::Ready)
    );
}

#[test]
fn recovery_without_a_row_restarts_at_validate() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.step();
    assert_eq!(b.state(REQ), Some(ReceiptState::Claimed));
    b.restart(FakePane::default());
    b.settle();

    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn recovery_of_a_launched_request_whose_row_is_gone_never_relaunches() {
    let mut b = staged_then_crashed();
    b.step();
    assert_eq!(b.pane.steps().len(), 1);
    Registry::update(&b.registry, |registry| {
        registry.projects.retain(|entry| entry.id != "service-2")
    })
    .unwrap();
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty(), "its Message may have been read");
    assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown));
    assert!(b.row("service-2").is_none());
}

#[test]
fn a_staged_row_removed_before_its_launch_fails_without_launching() {
    let mut b = staged_then_crashed();
    Registry::update(&b.registry, |registry| {
        registry.projects.retain(|entry| entry.id != "service-2")
    })
    .unwrap();
    b.settle();

    assert!(b.pane.steps().is_empty());
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::LaunchFailed));
}

#[test]
fn replay_after_receipt_cleanup_rebuilds_the_receipt_from_the_row() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));

    // A dashboard that settled the request never looks at it again; a later one rebuilds a
    // receipt that was lost.
    std::fs::remove_file(spawn::receipt_path(&b.requests(), REQ)).unwrap();
    b.restart(FakePane::default());
    b.settle();
    let rebuilt = b.receipt(REQ).unwrap();
    assert_eq!(rebuilt.state, ReceiptState::Ready);
    assert_eq!(rebuilt.session.unwrap().id, "service-2");

    remove_both(&b, REQ);
    b.publish(REQ, args(MESSAGE));
    b.restart(FakePane::default());
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert!(b.pane.steps().is_empty(), "the replay launches nothing");
    assert_eq!(b.rows().len(), 2, "one row for one request id");
}

#[test]
fn replayed_final_outcomes_keep_their_attach_action() {
    for (outcome, state) in [
        (SpawnOutcome::NeedsAttention, ReceiptState::NeedsAttention),
        (SpawnOutcome::OutcomeUnknown, ReceiptState::OutcomeUnknown),
    ] {
        let mut b = broker();
        b.publish(REQ, args(MESSAGE));
        b.settle();
        b.update_row("service-2", |row| {
            row.launch.as_mut().unwrap().outcome = Some(outcome)
        });
        std::fs::remove_file(spawn::receipt_path(&b.requests(), REQ)).unwrap();
        b.restart(FakePane::default());
        b.settle();
        let receipt = b.receipt(REQ).unwrap();
        assert_eq!(receipt.state, state);
        assert_attach(&receipt, &b.child_session());
    }
}

#[test]
fn a_matching_row_with_a_different_hash_is_request_conflict() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.settle();
    let before = b.row("service-2").unwrap();

    remove_both(&b, REQ);
    b.publish(REQ, args("a different task under the same id"));
    b.restart(FakePane::default());
    b.settle();

    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::RequestConflict));
    assert_eq!(
        b.receipt(REQ).unwrap().args_hash,
        before
            .launch
            .as_ref()
            .map(|launch| launch.args_hash.clone()),
        "the id stays bound to the arguments that created its child"
    );
    assert_eq!(
        b.row("service-2").unwrap(),
        before,
        "the child is untouched"
    );
    assert_eq!(b.rows().len(), 2);
    assert!(b.pane.steps().is_empty());
}

#[test]
fn readiness_spans_steps_and_step_spawns_never_sleeps() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    for _ in 0..3 {
        b.step();
    }
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.state(REQ), Some(ReceiptState::Launching));

    // The clock stands still: however many frames pass, a second observation 100 ms after the
    // first never happens, so the broker cannot be waiting for time inside a frame.
    let frozen = Instant::now();
    for _ in 0..50 {
        let frame = Instant::now();
        b.app.step_spawns();
        assert!(frame.elapsed() < Duration::from_millis(250));
    }
    assert!(frozen.elapsed() < Duration::from_secs(3));
    assert_eq!(b.state(REQ), Some(ReceiptState::Launching));

    b.clock.set(b.clock.get() + 99);
    b.app.step_spawns();
    assert_eq!(b.state(REQ), Some(ReceiptState::Launching));
    b.clock.set(b.clock.get() + 1);
    b.app.step_spawns();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn a_child_that_never_comes_up_is_unknown_to_readiness_and_left_no_result() {
    let mut b = broker_with(|inner, _| inner.skip_launch_alive = true);
    b.publish(REQ, args(MESSAGE));
    for _ in 0..3 {
        b.step();
    }
    assert_eq!(b.pane.steps().len(), 1);
    for _ in 0..3 {
        b.step();
        assert_eq!(b.state(REQ), Some(ReceiptState::Launching));
    }
    b.clock.set(b.clock.get() + 3_000);
    b.app.step_spawns();

    // READINESS'S OWN RECORD is the subject here, and it is on the row: it could not prove the child
    // came up.
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().outcome,
        Some(SpawnOutcome::OutcomeUnknown)
    );
    // The receipt is then REFINED in the same frame, because for a JOB "never seen alive" is not a
    // verdict about the work: the retirement sweep reads what the run left and answers with that. A
    // child that never came up left nothing, so the parent is told exactly that.
    let receipt = b.receipt(REQ).unwrap();
    assert_eq!(receipt.state, ReceiptState::EndedWithoutResult);
    let message = receipt.error.expect("a reason").message;
    assert!(
        message.contains("left no result"),
        "the parent is told why: {message}"
    );
    assert_eq!(b.pane.steps().len(), 1, "and nothing is relaunched");
}

#[test]
fn a_readiness_probe_error_is_outcome_unknown() {
    let mut b = broker_with(|inner, _| inner.fail_capture = true);
    b.publish(REQ, args(MESSAGE));
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::ReadinessUnknown));
    assert_eq!(b.pane.steps().len(), 1);
}

#[test]
fn trust_dialog_is_needs_attention_with_attach_argv() {
    let mut b = broker_with(|inner, child| {
        inner.tails.insert(child.into(), CODEX_TRUST_DIALOG.into());
    });
    b.update_row("service", |parent| parent.engine = Some(Engine::Codex));
    b.publish(REQ, args(MESSAGE));
    b.settle();

    let receipt = b.receipt(REQ).unwrap();
    assert_eq!(receipt.state, ReceiptState::NeedsAttention);
    assert_attach(&receipt, &b.child_session());
    let row = b.row("service-2").unwrap();
    assert!(row.enabled);
    assert_eq!(
        row.launch.unwrap().outcome,
        Some(SpawnOutcome::NeedsAttention)
    );
    assert!(
        b.pane.sends().is_empty(),
        "a trust dialog is never answered"
    );
    assert_eq!(b.pane.steps().len(), 1);
    assert!(b.app.status.contains("needs you"), "{}", b.app.status);
}

#[test]
fn pmd_holding_driver_lock_defers_launch_to_a_later_step() {
    let mut b = staged_then_crashed();
    let lock = ProjectPaths::for_session(&b.root, "service-2")
        .daemon_dir()
        .join("driver.lock");
    let held = acquire_free_lease(&lock).expect("the child's driver lock is free");
    for _ in 0..3 {
        b.step();
    }
    assert!(b.pane.steps().is_empty(), "never launched under pmd's lock");
    assert_eq!(b.state(REQ), Some(ReceiptState::Staged));
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().state,
        LaunchState::Pending
    );

    drop(held);
    wait_until_free(&lock);
    b.settle();
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn sixteen_unfinished_requests_per_parent_are_processed_and_the_rest_wait() {
    let mut b = broker();
    for n in 1..=17 {
        b.publish(&req_id(n), args(&format!("task {n}")));
    }
    b.discover();
    assert_eq!(b.app.spawn.jobs.len(), spawn::MAX_UNFINISHED_PER_PARENT);
    b.step();
    for n in 1..=16 {
        assert_eq!(b.state(&req_id(n)), Some(ReceiptState::Claimed), "{n}");
    }
    assert_eq!(b.state(&req_id(17)), None, "the seventeenth waits");

    // Not `settle`: the ones over the concurrency limit stay in flight on purpose, so there is nothing
    // to settle to while five children are alive.
    for _ in 0..30 {
        b.step();
    }
    b.discover();
    for _ in 0..30 {
        b.step();
    }
    let ready = (1..=17)
        .filter(|n| b.state(&req_id(*n)) == Some(ReceiptState::Ready))
        .count();
    assert_eq!(ready, MAX_CHILDREN_PER_PARENT);
    assert_eq!(b.pane.steps().len(), MAX_CHILDREN_PER_PARENT);
    // THE LIMIT IS HOW MANY RUN AT ONCE, so the ones over it WAIT rather than being refused: no error
    // code, and a receipt that still reads as queued to the parent. Refusing them made an agent choose
    // between doing that work itself and bothering a human, when the answer is "shortly".
    // THE LIMIT IS HOW MANY RUN AT ONCE, so everything over it WAITS rather than being refused: no error
    // code anywhere, and a `claimed` receipt that still reads as queued to the parent. Refusing them made
    // an agent choose between doing that work itself and bothering a human, when the answer is "shortly".
    // (The seventeenth is read on the later discovery, because a `ready` receipt is settled and frees a
    // slot in the per-parent request window — it then queues like the rest.)
    for n in 1..=17 {
        let Some(state) = b.state(&req_id(n)) else {
            continue;
        };
        assert!(
            state == ReceiptState::Ready || state == ReceiptState::Claimed,
            "{n} is running or waiting, not refused: {state:?}"
        );
        assert_eq!(b.error_code(&req_id(n)), None, "{n} is not an error");
    }
    assert!(
        !b.app.spawn.jobs.is_empty(),
        "the queued ones are still in flight, waiting for a slot"
    );
}

/// A JOB IN A REPOSITORY RUNS IN ITS OWN CHECKOUT. The launch creates the worktree, the row records the
/// branch and the commit it started from, the run's cwd IS that worktree, and the prompt names the branch
/// the child must commit on. Without each of those, two children in one project overwrite each other.
#[test]
fn a_job_in_a_repository_launches_in_its_own_worktree() {
    let mut b = broker();
    init_repo(&b.root);
    b.publish(&req_id(1), args(MESSAGE));
    b.discover();
    for _ in 0..10 {
        b.step();
    }

    let launch = Registry::load(&b.registry)
        .unwrap()
        .projects
        .iter()
        .find(|row| row.spawned_by.is_some())
        .and_then(|row| row.launch.clone())
        .expect("the child row");
    let branch = launch.branch.expect("the row names its branch");
    assert_eq!(branch, format!("pm/service-2-{}", &req_id(1)[..8]));
    assert!(
        launch.base_commit.is_some_and(|base| base.len() == 40),
        "and the commit it started from"
    );

    let (_, cwd, argv) = b
        .pane
        .steps()
        .into_iter()
        .next()
        .expect("the job was launched");
    assert!(
        cwd.ends_with("worktree") && cwd.contains("service-2"),
        "the run's cwd is its own worktree: {cwd}"
    );
    assert!(
        std::path::Path::new(&cwd).join(".git").exists(),
        "which is a real checkout: {cwd}"
    );
    let prompt = argv.last().expect("the prompt is last");
    assert!(
        prompt.contains(&format!("branch {branch}")) && prompt.contains("Commit your work"),
        "the child is told where to commit: {prompt}"
    );
}

/// A REPOSITORY IT CANNOT BRANCH FROM does not stop the work: the job runs in the project directory, with
/// one line saying why, and its row names no branch. Isolation is worth having, not worth refusing over.
#[test]
fn a_job_runs_in_the_project_directory_when_no_worktree_can_be_made() {
    let mut b = broker();
    // A repository with no commit: there is no HEAD to branch from.
    run_git(&b.root, &["init", "--quiet", "-b", "main"]);
    b.publish(&req_id(1), args(MESSAGE));
    b.discover();
    for _ in 0..10 {
        b.step();
    }

    let launch = Registry::load(&b.registry)
        .unwrap()
        .projects
        .iter()
        .find(|row| row.spawned_by.is_some())
        .and_then(|row| row.launch.clone())
        .expect("the child row");
    assert!(launch.branch.is_none(), "no branch was made");
    let (_, cwd, _) = b
        .pane
        .steps()
        .into_iter()
        .next()
        .expect("it was launched anyway");
    assert!(
        !cwd.ends_with("worktree"),
        "in the project directory: {cwd}"
    );
    let log = std::fs::read_to_string(&b.app.status_log).unwrap_or_default();
    assert!(
        log.contains("project directory"),
        "and the status log says why: {log}"
    );
}

/// THE SIXTH CHILD IS NOT A FAILURE. It used to be answered `child_limit_reached`, which left an agent
/// choosing between doing that work itself and bothering a human; now it holds its `claimed` receipt —
/// `queued` to the parent — and stages when a sibling leaves. Nothing is written that an agent must act on.
#[test]
fn a_sixth_child_is_queued_rather_than_refused() {
    let mut b = broker();
    Registry::update(&b.registry, |registry| {
        let template = registry.projects[0].clone();
        for n in 0..MAX_CHILDREN_PER_PARENT {
            let mut child = template.clone();
            child.id = format!("kid-{n}");
            child.spawned_by = Some("service".into());
            registry.projects.push(child);
        }
    })
    .unwrap();
    b.publish(&req_id(1), args(MESSAGE));
    b.discover();
    for _ in 0..10 {
        b.step();
    }

    assert_eq!(b.state(&req_id(1)), Some(ReceiptState::Claimed), "queued");
    assert_eq!(b.error_code(&req_id(1)), None, "and not an error");
    assert!(b.pane.steps().is_empty(), "nothing was launched");
    assert_eq!(
        Registry::load(&b.registry)
            .unwrap()
            .projects
            .iter()
            .filter(|row| row.spawned_by.is_some())
            .count(),
        MAX_CHILDREN_PER_PARENT,
        "and no sixth row was staged"
    );
}

/// A FREED SLOT STARTS THE NEXT ONE, with nothing re-asked. The queue is only useful if it drains: when
/// a child leaves the session list, the request that was waiting stages on the next frame under the id it
/// already had.
#[test]
fn a_queued_request_starts_as_soon_as_a_sibling_leaves() {
    let mut b = broker();
    for n in 1..=MAX_CHILDREN_PER_PARENT as u32 + 1 {
        b.publish(&req_id(n), args(&format!("task {n}")));
    }
    b.discover();
    for _ in 0..30 {
        b.step();
    }
    let running: Vec<u32> = (1..=MAX_CHILDREN_PER_PARENT as u32 + 1)
        .filter(|n| b.state(&req_id(*n)) == Some(ReceiptState::Ready))
        .collect();
    assert_eq!(running.len(), MAX_CHILDREN_PER_PARENT, "{running:?}");
    let waiting = (1..=MAX_CHILDREN_PER_PARENT as u32 + 1)
        .find(|n| b.state(&req_id(*n)) == Some(ReceiptState::Claimed))
        .expect("one is queued");

    // Retire one child the way the sweep does: its row leaves the registry.
    let mut reg = Registry::load(&b.registry).unwrap();
    let child = reg
        .projects
        .iter()
        .find(|row| row.spawned_by.is_some())
        .map(|row| row.id.clone())
        .expect("a child row");
    reg.projects.retain(|row| row.id != child);
    reg.save(&b.registry).unwrap();

    for _ in 0..30 {
        b.step();
    }
    assert_eq!(
        b.state(&req_id(waiting)),
        Some(ReceiptState::Ready),
        "the queued request started once there was room"
    );
    assert_eq!(
        b.error_code(&req_id(waiting)),
        None,
        "and was never refused"
    );
}

#[test]
fn retention_removes_only_old_final_requests_and_keeps_every_receipt() {
    let mut b = broker();
    let now = SystemClock.now();
    let old = now - spawn::RETENTION_SECS - 60;
    let receipt = |id: &str, state: ReceiptState, updated_at: Epoch| SpawnReceipt {
        schema_version: spawn::RECEIPT_SCHEMA_VERSION,
        request_id: id.into(),
        state,
        claimed_by: Some("pid:1".into()),
        session: None,
        launch_state: None,
        error: None,
        next_action: None,
        args_hash: None,
        result: None,
        updated_at,
        work: None,
    };
    let cases = [
        (req_id(1), ReceiptState::Ready, old, false),
        (req_id(2), ReceiptState::Failed, now - 3_600, true),
        (req_id(3), ReceiptState::Claimed, old, true),
    ];
    for (id, state, at, _) in &cases {
        b.publish(id, args(MESSAGE));
        spawn::write_receipt(&b.requests(), &receipt(id, *state, *at)).unwrap();
    }
    std::fs::write(spawn::request_path(&b.requests(), &req_id(4)), "{").unwrap();
    spawn::write_receipt(
        &b.requests(),
        &receipt(&req_id(4), ReceiptState::Failed, old),
    )
    .unwrap();

    b.discover();
    for (id, _, _, kept) in &cases {
        assert_eq!(
            spawn::request_path(&b.requests(), id).exists(),
            *kept,
            "{id}"
        );
        assert!(
            spawn::receipt_path(&b.requests(), id).exists(),
            "{id}: a receipt is a permanent tombstone"
        );
    }
    assert!(!spawn::request_path(&b.requests(), &req_id(4)).exists());
    assert!(spawn::receipt_path(&b.requests(), &req_id(4)).exists());
    assert_eq!(b.app.spawn.jobs.len(), 1, "only the unfinished one resumes");
}

#[test]
fn an_unreadable_session_list_defers_the_pre_check_and_a_staged_launch() {
    let mut b = broker();
    let good = std::fs::read(&b.registry).unwrap();
    b.publish(REQ, args(MESSAGE));
    b.discover();
    std::fs::write(&b.registry, "{").unwrap();
    for _ in 0..3 {
        b.step();
    }
    assert_eq!(b.receipt(REQ), None, "no claim without a pre-check");
    assert_eq!(b.app.spawn.jobs.len(), 1, "the request keeps its place");
    assert!(b.app.status.contains("will retry"), "{}", b.app.status);

    std::fs::write(&b.registry, &good).unwrap();
    b.step();
    assert_eq!(b.state(REQ), Some(ReceiptState::Claimed));
    b.step();
    assert_eq!(b.state(REQ), Some(ReceiptState::Staged));
    let staged = std::fs::read(&b.registry).unwrap();
    std::fs::write(&b.registry, "{").unwrap();
    b.app.status.clear();
    for _ in 0..3 {
        b.step();
    }
    assert!(b.pane.steps().is_empty(), "no launch without its row");
    assert!(b.app.status.contains("will retry"), "{}", b.app.status);

    std::fs::write(&b.registry, staged).unwrap();
    b.settle();
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn a_session_list_that_cannot_be_saved_fails_the_stage() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.step();
    let writable = std::fs::metadata(&b.base).unwrap().permissions();
    std::fs::set_permissions(&b.base, std::fs::Permissions::from_mode(0o555)).unwrap();
    b.step();
    std::fs::set_permissions(&b.base, writable).unwrap();
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::RegistryUnreadable));
    assert!(b.row("service-2").is_none());
    assert!(b.pane.steps().is_empty());
}

#[test]
fn a_promotion_that_cannot_be_saved_is_finished_from_the_live_terminal() {
    let mut b = staged_then_crashed();
    let writable = std::fs::metadata(&b.base).unwrap().permissions();
    let base = b.base.clone();
    b.restart(FakePane::default().with(|inner| inner.freeze_dir_on_launch = Some(base)));
    b.step();
    b.step();
    std::fs::set_permissions(&b.base, writable).unwrap();
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().state,
        LaunchState::Attempted,
        "the promotion could not be saved"
    );

    b.settle();
    assert_eq!(
        b.pane.steps().len(),
        1,
        "the live terminal is promoted, not relaunched"
    );
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().state,
        LaunchState::Started
    );
}

#[test]
fn a_claim_that_cannot_be_written_stages_nothing() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    let requests = b.requests();
    let writable = std::fs::metadata(&requests).unwrap().permissions();
    std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o500)).unwrap();
    b.settle();
    std::fs::set_permissions(&requests, writable).unwrap();
    assert_eq!(b.receipt(REQ), None);
    assert!(b.row("service-2").is_none());
    assert!(b.app.status.contains("could not claim"), "{}", b.app.status);

    b.discover();
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn a_child_spawn_request_is_refused_as_nested() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.settle();
    let child_requests = spawn::requests_dir(&b.child_state_dir());
    spawn::publish_request(
        &child_requests,
        &request("service-2", &req_id(5), args("grandchild")),
    )
    .unwrap();
    b.discover();
    b.settle();
    let receipt = spawn::read_receipt(&child_requests, &req_id(5))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state, ReceiptState::Failed);
    assert_eq!(
        receipt.error.map(|error| error.code),
        Some(ErrorCode::NestedSpawnRefused)
    );
    assert_eq!(b.rows().len(), 2);
}

#[test]
fn refresh_discovers_and_the_frame_steps_the_broker() {
    let mut b = broker();
    b.app.step_spawns();
    b.publish(REQ, args(MESSAGE));
    assert!(b.app.spawn.jobs.is_empty());
    b.app.refresh();
    assert_eq!(b.app.spawn.jobs.len(), 1, "refresh discovers new requests");
    b.app.refresh();
    assert_eq!(b.app.spawn.jobs.len(), 1, "and never enqueues one twice");

    let mut last_refresh = Instant::now();
    finish_frame(&mut b.app, true, &mut last_refresh);
    assert_eq!(b.state(REQ), Some(ReceiptState::Claimed));
    for _ in 0..8 {
        b.clock.set(b.clock.get() + 150);
        finish_frame(&mut b.app, true, &mut last_refresh);
    }
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert!(
        log_lines(&b.app)
            .iter()
            .any(|line| line == "dashboard spawned service-2 from service"),
        "{:?}",
        log_lines(&b.app)
    );
    assert!(
        b.app.projects.iter().any(|view| view.id == "service-2"),
        "the child is on screen without a restart"
    );
}

#[test]
fn a_driver_lock_that_cannot_be_opened_is_a_proven_pre_start_failure() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = staged_then_crashed();
    let state_dir = b.child_state_dir();
    let writable = std::fs::metadata(&state_dir).unwrap().permissions();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    b.step();
    assert!(
        b.pane.steps().is_empty(),
        "nothing launches without the lock"
    );
    assert!(
        b.row("service-2").is_none(),
        "the proven failure removed its row"
    );
    assert!(
        b.app.status.contains("could not discard"),
        "{}",
        b.app.status
    );
    std::fs::set_permissions(&state_dir, writable).unwrap();
    b.settle();

    assert!(b.pane.steps().is_empty());
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::LaunchFailed));
}

#[test]
fn a_launch_waits_while_its_attempt_cannot_be_recorded() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = staged_then_crashed();
    let writable = std::fs::metadata(&b.base).unwrap().permissions();
    std::fs::set_permissions(&b.base, std::fs::Permissions::from_mode(0o555)).unwrap();
    b.step();
    std::fs::set_permissions(&b.base, writable).unwrap();
    assert!(
        b.pane.steps().is_empty(),
        "no launch before `attempted` is on disk"
    );
    assert!(b.app.status.contains("will retry"), "{}", b.app.status);
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().state,
        LaunchState::Pending
    );

    // The step released the child's driver lock; a sibling test's fork may briefly hold its fd.
    wait_until_free(
        &ProjectPaths::for_session(&b.root, "service-2")
            .daemon_dir()
            .join("driver.lock"),
    );
    b.settle();
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn a_proven_failure_that_cannot_be_recorded_is_never_retried() {
    let mut b = staged_then_crashed();
    let writable = std::fs::metadata(&b.base).unwrap().permissions();
    let base = b.base.clone();
    let child = b.child_session();
    let pane = FakePane::default().with(|inner| {
        inner.freeze_dir_on_launch = Some(base);
        inner
            .armed_launch_errors
            .get_mut()
            .unwrap()
            .insert(child, tmux::LaunchError::NotOnPath("claude".into()));
    });
    b.restart(pane);
    b.step();
    b.step();
    std::fs::set_permissions(&b.base, writable).unwrap();
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().state,
        LaunchState::Attempted,
        "the proof could not be saved"
    );

    b.settle();
    assert_eq!(
        b.pane.steps().len(),
        1,
        "an unproven attempt is never repeated"
    );
    assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown));
}

#[test]
fn receipts_that_cannot_be_written_never_stop_the_child() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.step();
    let requests = b.requests();
    let writable = std::fs::metadata(&requests).unwrap().permissions();
    std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o500)).unwrap();
    b.step();
    assert!(
        b.app.status.contains("could not write the receipt"),
        "{}",
        b.app.status
    );
    b.settle();
    std::fs::set_permissions(&requests, writable).unwrap();
    assert_eq!(
        b.state(REQ),
        Some(ReceiptState::Claimed),
        "only the claim landed"
    );
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().outcome,
        Some(SpawnOutcome::Ready),
        "the row is the truth"
    );

    b.discover();
    b.settle();
    assert_eq!(
        b.state(REQ),
        Some(ReceiptState::Ready),
        "rebuilt from the row"
    );
    assert_eq!(b.pane.steps().len(), 1);
}

#[test]
fn an_unwritable_requests_dir_is_noted_once() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = broker();
    b.publish(&req_id(1), args(MESSAGE));
    spawn::write_receipt(
        &b.requests(),
        &SpawnReceipt {
            schema_version: spawn::RECEIPT_SCHEMA_VERSION,
            request_id: req_id(1),
            state: ReceiptState::Ready,
            claimed_by: None,
            session: None,
            launch_state: None,
            error: None,
            next_action: None,
            args_hash: None,
            result: None,
            updated_at: SystemClock.now() - spawn::RETENTION_SECS - 60,
            work: None,
        },
    )
    .unwrap();
    std::fs::write(spawn::request_path(&b.requests(), &req_id(2)), "{").unwrap();
    let requests = b.requests();
    let writable = std::fs::metadata(&requests).unwrap().permissions();
    std::fs::set_permissions(&requests, std::fs::Permissions::from_mode(0o500)).unwrap();
    b.discover();
    b.discover();
    std::fs::set_permissions(&requests, writable).unwrap();

    let noted = |needle: &str| {
        log_lines(&b.app)
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    };
    assert_eq!(
        noted("could not remove the expired request"),
        1,
        "{:?}",
        log_lines(&b.app)
    );
    assert_eq!(
        noted("could not answer invalid request"),
        1,
        "{:?}",
        log_lines(&b.app)
    );
    assert!(spawn::request_path(&b.requests(), &req_id(1)).exists());
    assert_eq!(b.receipt(&req_id(2)), None);
}

#[test]
fn a_readiness_probe_error_on_a_dead_pane_check_is_outcome_unknown() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    for _ in 0..3 {
        b.step();
    }
    let child = b.child_session();
    // The same live terminal, observed by a driver whose pane probe now fails.
    b.restart(FakePane::live_quiet(&child).with(|inner| inner.fail_pane_dead = true));
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown));
    assert!(b.pane.steps().is_empty());
}

#[test]
fn the_attach_argv_names_the_first_executable_tmux_on_path() {
    use std::os::unix::fs::PermissionsExt;

    assert_eq!(tmux_program(None), "tmux");
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let not_executable = empty.join("tmux");
    std::fs::write(&not_executable, "").unwrap();
    let tmux = bin.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([&empty, &bin]).unwrap();
    assert_eq!(tmux_program(Some(&path)), tmux.display().to_string());
    let path = std::env::join_paths([&empty]).unwrap();
    assert_eq!(tmux_program(Some(&path)), "tmux");
}

#[test]
fn the_broker_clock_is_monotonic_milliseconds() {
    let broker = SpawnBroker::default();
    let first = (broker.now_ms)();
    std::thread::sleep(Duration::from_millis(5));
    assert!((broker.now_ms)() >= first + 5);
}

/// A JOB CHILD GETS NO SPAWN SKILL, and no `ManagedEnv` either. "A spawned session cannot spawn" was a
/// rule a child had to be told and trusted to follow; a job is simply never given the procedure or the
/// environment variables the command needs, so the depth limit holds by construction.
#[test]
fn a_job_child_is_given_no_way_to_spawn() {
    let mut b = broker();
    b.app.pmtui_bin = Some(PathBuf::from("/opt/am/pmtui"));
    b.publish(REQ, args(MESSAGE));

    b.settle();

    assert_eq!(b.state(REQ), Some(ReceiptState::Ready), "{}", b.app.status);
    let paths = ProjectPaths::new(&b.root);
    assert!(
        std::fs::read_to_string(paths.canonical_spawn_skill_file()).is_err(),
        "a job must not be handed the spawn procedure"
    );
    assert!(
        b.pane.launched_env().is_empty(),
        "a job is launched as a step, with no ManagedEnv to identify itself by"
    );
}

/// Frames of a dashboard that is not the broker: it refreshes (which is where discovery runs) and
/// steps, as its event loop would.
fn idle_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.refresh();
        app.step_spawns();
    }
}

#[test]
fn two_dashboards_on_one_registry_only_the_spawn_broker_lease_holder_steps() {
    let mut b = broker();
    b.step();
    // A second dashboard on the same session list, under another tmux socket: it holds its own
    // dashboard singleton (that lock is per registry and socket), but not the spawn-broker lease.
    let second_pane = FakePane::default();
    let (mut second, second_clock) = dashboard(&b.registry, &second_pane);
    second.socket = "pm-other".into();
    b.publish(REQ, args(MESSAGE));

    idle_frames(&mut second, 5);
    assert_eq!(b.receipt(REQ), None, "the second dashboard never claims");
    assert!(second.spawn.jobs.is_empty());
    let noted = log_lines(&second)
        .into_iter()
        .filter(|line| line.contains("another dashboard is brokering spawn requests"))
        .count();
    assert_eq!(noted, 1, "noted once: {:?}", log_lines(&second));

    b.discover();
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    idle_frames(&mut second, 5);
    assert!(second_pane.steps().is_empty(), "only the holder launches");
    assert_eq!(b.pane.steps().len(), 1);
    assert_eq!(b.rows().len(), 2, "one child for one request");

    // Once the broker's dashboard is gone, the other one takes the lease and the next request.
    let registry = b.registry.clone();
    b.app = app_with_driver(Vec::new(), UiMode::Normal, Box::new(FakePane::default()));
    wait_until_free(&lease::spawn_broker_lock_path(&registry));
    b.publish(&req_id(2), args("second task"));
    for _ in 0..20 {
        second_clock.set(second_clock.get() + 150);
        second.step_spawns();
    }
    assert_eq!(b.state(&req_id(2)), Some(ReceiptState::Ready));
    assert_eq!(second_pane.steps().len(), 1);
}

#[test]
fn a_dashboard_without_the_singleton_never_brokers() {
    let mut b = broker();
    b.app.dashboard_owner_nonce = None;
    b.publish(REQ, args(MESSAGE));
    idle_frames(&mut b.app, 5);
    assert_eq!(b.receipt(REQ), None);
    assert!(b.app.spawn.jobs.is_empty());
    assert!(b.pane.steps().is_empty());
    let noted = log_lines(&b.app)
        .into_iter()
        .filter(|line| line.contains("leaves spawn requests to another dashboard"))
        .count();
    assert_eq!(noted, 1, "noted once: {:?}", log_lines(&b.app));
    assert_eq!(
        lease::is_held(&lease::spawn_broker_lock_path(&b.registry)).unwrap(),
        None,
        "it never even takes the lease"
    );
}

#[test]
fn a_spawn_broker_lock_that_cannot_open_keeps_the_broker_idle_until_it_can() {
    use std::os::unix::fs::PermissionsExt;

    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    let writable = std::fs::metadata(&b.base).unwrap().permissions();
    std::fs::set_permissions(&b.base, std::fs::Permissions::from_mode(0o555)).unwrap();
    for _ in 0..3 {
        b.step();
    }
    std::fs::set_permissions(&b.base, writable).unwrap();
    assert_eq!(b.receipt(REQ), None, "no lease, no claim");
    let noted = log_lines(&b.app)
        .into_iter()
        .filter(|line| line.contains("could not open the spawn-broker lock"))
        .count();
    assert_eq!(noted, 1, "noted once: {:?}", log_lines(&b.app));

    b.settle();
    assert_eq!(
        b.state(REQ),
        Some(ReceiptState::Ready),
        "a later frame takes it"
    );
}

#[test]
fn a_stale_job_never_stages_a_duplicate_row() {
    let mut b = broker();
    let spawn_args = args(MESSAGE);
    b.publish(REQ, spawn_args.clone());
    b.step();
    assert_eq!(b.state(REQ), Some(ReceiptState::Claimed));
    // Between this job's pre-check and its stage, a row for the same request appears.
    Registry::update(&b.registry, |registry| {
        let mut row = registry.projects[0].clone();
        row.id = "service-9".into();
        row.enabled = false;
        row.spawned_by = Some("service".into());
        row.launch = Some(LaunchRecord {
            request_id: REQ.into(),
            args_hash: spawn_args.args_hash(),
            state: LaunchState::Pending,
            outcome: None,
            kind: agent_manager::registry::LaunchKind::Job,
            branch: None,
            base_commit: None,
        });
        registry.projects.push(row);
    })
    .unwrap();

    b.step();
    assert_eq!(b.rows().len(), 2, "no second row for the request");
    assert!(b.row("service-2").is_none());
    assert!(
        !b.child_state_dir().exists(),
        "the reservation it made is released"
    );

    b.settle();
    assert_eq!(b.rows().len(), 2);
    assert_eq!(b.pane.steps().len(), 1, "the existing row launches once");
    assert_eq!(b.pane.steps()[0].0, session_name("service-9", &b.root));
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn readiness_after_a_stalled_frame_still_proves_a_live_child() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    for _ in 0..3 {
        b.step();
    }
    assert_eq!(b.pane.steps().len(), 1);
    // The dashboard stalls past the whole readiness window before its first observation.
    b.clock.set(b.clock.get() + 5_000);
    b.app.step_spawns();
    assert_eq!(
        b.state(REQ),
        Some(ReceiptState::Launching),
        "one live observation after a stall is not an unknown outcome"
    );
    b.step();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert_eq!(b.pane.steps().len(), 1);
}

#[test]
fn a_child_whose_second_observation_never_comes_is_unknown_a_second_after_its_first() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    for _ in 0..3 {
        b.step();
    }
    b.clock.set(b.clock.get() + 5_000);
    b.app.step_spawns();
    assert_eq!(b.state(REQ), Some(ReceiptState::Launching));
    // The terminal goes away after that first live look; the extended window still closes.
    b.pane.0.alive.lock().unwrap().clear();
    b.clock.set(b.clock.get() + 999);
    b.app.step_spawns();
    assert_eq!(b.state(REQ), Some(ReceiptState::Launching));
    b.clock.set(b.clock.get() + 1);
    b.app.step_spawns();
    assert_eq!(
        b.row("service-2").unwrap().launch.unwrap().outcome,
        Some(SpawnOutcome::OutcomeUnknown),
        "readiness closed its window without a second look"
    );
    // And the sweep refines that into what the job actually left: nothing.
    assert_eq!(b.state(REQ), Some(ReceiptState::EndedWithoutResult));
}

#[test]
fn finishing_never_re_enables_a_child_a_human_paused() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    for _ in 0..3 {
        b.step();
    }
    assert!(b.row("service-2").unwrap().enabled, "started and enabled");
    b.update_row("service-2", |row| row.enabled = false);

    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    let row = b.row("service-2").unwrap();
    assert!(!row.enabled, "the human's pause stands");
    assert_eq!(row.launch.unwrap().outcome, Some(SpawnOutcome::Ready));
}

#[test]
fn recovering_a_started_child_a_human_paused_keeps_it_paused() {
    let mut b = staged_then_crashed();
    b.step();
    b.update_row("service-2", |row| row.enabled = false);
    let child = b.child_session();
    b.restart(FakePane::live_quiet(&child));
    b.settle();

    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert!(!b.row("service-2").unwrap().enabled);
    assert!(b.pane.steps().is_empty());
}

#[test]
fn the_claude_conversation_id_is_saved_with_the_attempt_before_the_launch() {
    let mut b = staged_then_crashed();
    let writable = std::fs::metadata(&b.base).unwrap().permissions();
    let base = b.base.clone();
    b.restart(FakePane::default().with(|inner| inner.freeze_dir_on_launch = Some(base)));
    b.step();
    b.step();
    std::fs::set_permissions(&b.base, writable).unwrap();
    let launches = b.pane.steps(); // a job child's one-shot launch
    assert_eq!(launches.len(), 1);
    let argv = &launches[0].2;
    let cid = argv
        .iter()
        .position(|arg| arg == "--session-id")
        .map(|at| argv[at + 1].clone())
        .expect("a fresh Claude conversation");
    let attempted = b.row("service-2").unwrap();
    assert_eq!(
        attempted.launch.as_ref().unwrap().state,
        LaunchState::Attempted,
        "the promotion could not be saved"
    );
    assert_eq!(
        attempted.conversation_id,
        Some(cid.clone()),
        "the conversation the launch created is on the row before anything else"
    );

    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    assert_eq!(b.row("service-2").unwrap().conversation_id, Some(cid));
    assert_eq!(b.pane.steps().len(), 1);
}

#[test]
fn an_ambiguous_claude_launch_keeps_its_conversation_id() {
    let mut b = broker_with(|inner, child| {
        inner.armed_launch_errors.get_mut().unwrap().insert(
            child.into(),
            tmux::LaunchError::ExitedAfterStart("the agent exited at startup".into()),
        );
    });
    b.publish(REQ, args(MESSAGE));
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown));
    let argv = &b.pane.steps()[0].2;
    let cid = argv
        .iter()
        .position(|arg| arg == "--session-id")
        .map(|at| argv[at + 1].clone());
    assert!(cid.is_some());
    assert_eq!(b.row("service-2").unwrap().conversation_id, cid);
}

#[test]
fn deleting_a_staged_row_never_reopens_its_request() {
    let mut b = staged_then_crashed();
    Registry::update(&b.registry, |registry| {
        registry.projects.retain(|entry| entry.id != "service-2")
    })
    .unwrap();
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty(), "never re-claimed or relaunched");
    assert_eq!(b.rows().len(), 1, "no second row");
    let receipt = b.receipt(REQ).unwrap();
    assert_eq!(receipt.state, ReceiptState::Failed);
    let error = receipt.error.unwrap();
    assert_eq!(error.code, ErrorCode::LaunchFailed);
    assert!(
        error.message.contains("its staged row was removed"),
        "{}",
        error.message
    );

    for _ in 0..3 {
        b.discover();
        b.step();
    }
    assert!(b.pane.steps().is_empty());
    assert_eq!(b.rows().len(), 1);
}

#[test]
fn a_receipt_past_claimed_proves_its_request_staged_a_row() {
    let staged_session = || {
        let b = broker();
        let paths = ProjectPaths::for_session(&b.root, "service-2");
        spawn::ReceiptSession {
            id: "service-2".into(),
            title: None,
            display_name: None,
            root: b.root.clone(),
            agent: Engine::Claude,
            model: None,
            spawned_by: "service".into(),
            tmux_session: b.child_session(),
            state_dir: paths.state_dir(),
        }
    };
    let cases: [(&str, ReceiptState, Option<LaunchState>, bool, ReceiptState); 7] = [
        (
            "staged",
            ReceiptState::Staged,
            None,
            false,
            ReceiptState::Failed,
        ),
        (
            "staged, pending",
            ReceiptState::Staged,
            Some(LaunchState::Pending),
            true,
            ReceiptState::Failed,
        ),
        (
            "failed before start, removal interrupted",
            ReceiptState::Launching,
            Some(LaunchState::FailedBeforeStart),
            true,
            ReceiptState::Failed,
        ),
        (
            "claimed with a launch state",
            ReceiptState::Claimed,
            Some(LaunchState::Pending),
            false,
            ReceiptState::Failed,
        ),
        (
            "claimed with a session",
            ReceiptState::Claimed,
            None,
            true,
            ReceiptState::Failed,
        ),
        (
            "attempted",
            ReceiptState::Launching,
            Some(LaunchState::Attempted),
            true,
            ReceiptState::OutcomeUnknown,
        ),
        (
            "started",
            ReceiptState::Launching,
            Some(LaunchState::Started),
            true,
            ReceiptState::OutcomeUnknown,
        ),
    ];
    for (what, state, launch_state, with_session, answered) in cases {
        let mut b = broker();
        b.publish(REQ, args(MESSAGE));
        spawn::write_receipt(
            &b.requests(),
            &SpawnReceipt {
                schema_version: spawn::RECEIPT_SCHEMA_VERSION,
                request_id: REQ.into(),
                state,
                claimed_by: Some("pid:1".into()),
                session: with_session.then(staged_session),
                launch_state,
                error: None,
                next_action: None,
                args_hash: Some(args(MESSAGE).args_hash()),
                result: None,
                updated_at: SystemClock.now(),
                work: None,
            },
        )
        .unwrap();
        b.settle();

        assert!(b.pane.steps().is_empty(), "{what}");
        assert_eq!(b.rows().len(), 1, "{what}: nothing staged again");
        let receipt = b.receipt(REQ).unwrap();
        assert_eq!(receipt.state, answered, "{what}");
        if answered == ReceiptState::Failed {
            let message = receipt.error.unwrap().message;
            assert!(
                message.contains("its staged row was removed"),
                "{what}: {message}"
            );
        }
    }
}

/// The command's only notion of time in these tests: it never waits, and time stands still.
struct NoWait;

impl crate::spawn_cli::Waiter for NoWait {
    fn now_ms(&mut self) -> u128 {
        0
    }

    fn sleep_ms(&mut self, _: u64) {}
}

#[test]
fn a_replay_after_retention_never_creates_a_second_child() {
    use crate::spawn_cli::{CliState, SpawnCli, run_spawn};

    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
    // A human deletes the child, and a week passes.
    Registry::update(&b.registry, |registry| {
        registry.projects.retain(|entry| entry.id != "service-2")
    })
    .unwrap();
    let mut receipt = b.receipt(REQ).unwrap();
    receipt.updated_at -= spawn::RETENTION_SECS + 60;
    spawn::write_receipt(&b.requests(), &receipt).unwrap();
    b.restart(FakePane::default());
    b.step();
    assert!(!spawn::request_path(&b.requests(), REQ).exists());
    assert_eq!(b.receipt(REQ), Some(receipt.clone()), "the tombstone stays");

    // The agent checks on its request again, with the same id and arguments.
    let state_dir = ProjectPaths::for_session(&b.root, "service").state_dir();
    let env = |key: &str| match key {
        "PMTUI_SESSION" => Some("service".to_string()),
        "PMTUI_STATE_DIR" => Some(state_dir.display().to_string()),
        _ => None,
    };
    let replay = SpawnCli {
        args: args(MESSAGE),
        request_id: Some(REQ.into()),
        wait_s: 0,
        json: true,
        cancel: false,
        status: false,
    };
    let out = run_spawn(&replay, &env, &SystemClock, &mut NoWait);
    // `ready` is the job RUNNING, so the command says check again rather than inventing an answer —
    // and either way it publishes NOTHING, which is what stops a second child from existing.
    assert_eq!(out.state, CliState::InProgress);
    assert_eq!(out.receipt, Some(receipt));
    assert!(
        !spawn::request_path(&b.requests(), REQ).exists(),
        "the id is already answered, so nothing is published again"
    );

    // Even a request published again under that id is answered by its tombstone.
    b.publish(REQ, args(MESSAGE));
    b.discover();
    b.settle();
    b.restart(FakePane::default());
    b.settle();
    assert_eq!(b.rows().len(), 1, "no second child");
    assert!(b.pane.steps().is_empty());
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn a_settled_request_is_skipped_without_opening_anything() {
    let mut b = broker();
    // A request that fails validation leaves no row: only its receipt answers it.
    b.publish(
        REQ,
        SpawnArgs {
            dir: Some(b.base.join("missing")),
            ..args(MESSAGE)
        },
    );
    b.settle();
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    b.discover();
    // Were discovery to open either file again, the unreadable request would now be answered
    // with a fresh `invalid_request` receipt.
    std::fs::remove_file(spawn::receipt_path(&b.requests(), REQ)).unwrap();
    std::fs::write(spawn::request_path(&b.requests(), REQ), "{").unwrap();
    for _ in 0..3 {
        b.discover();
        b.step();
    }
    assert_eq!(b.receipt(REQ), None, "a settled request is never reopened");
    assert!(b.app.spawn.jobs.is_empty());
}

#[test]
fn a_settled_request_republished_after_retention_is_removed_again_unopened() {
    let mut b = broker();
    b.publish(REQ, args(MESSAGE));
    b.settle();
    let mut receipt = b.receipt(REQ).unwrap();
    receipt.updated_at -= spawn::RETENTION_SECS + 60;
    spawn::write_receipt(&b.requests(), &receipt).unwrap();
    // A week later, a dashboard settles it and removes the request.
    b.restart(FakePane::default());
    b.step();
    assert!(!spawn::request_path(&b.requests(), REQ).exists());

    b.publish(REQ, args(MESSAGE));
    b.discover();
    assert!(
        !spawn::request_path(&b.requests(), REQ).exists(),
        "its receipt was final long ago"
    );
    assert_eq!(b.receipt(REQ), Some(receipt));
    assert!(b.pane.steps().is_empty());
}

#[test]
fn discovery_opens_at_most_64_entries_per_parent_per_refresh_and_notes_it_once() {
    let mut b = broker();
    let requests = b.requests();
    std::fs::create_dir_all(&requests).unwrap();
    // Validly named, unreadable requests: each one opened is answered with `invalid_request`.
    for n in 1..=70 {
        std::fs::write(spawn::request_path(&requests, &req_id(n)), "{").unwrap();
    }
    let answered = |b: &Broker| {
        (1..=70)
            .filter(|n| b.receipt(&req_id(*n)).is_some())
            .count()
    };

    b.discover();
    assert_eq!(answered(&b), 64, "the first 64 are opened; the rest wait");
    b.discover();
    b.discover();
    assert_eq!(answered(&b), 70, "later refreshes reach the rest");
    let capped = log_lines(&b.app)
        .into_iter()
        .filter(|line| line.contains("more than 64"))
        .count();
    assert_eq!(capped, 1, "noted once: {:?}", log_lines(&b.app));
    assert_eq!(b.rows().len(), 1);
}

/// [`staged_then_crashed`], then the request file is gone (cleaned up by hand, or never
/// reachable), leaving a staged row no request will ever resume.
fn orphaned(state: LaunchState) -> Broker {
    let b = staged_then_crashed();
    set_launch_state(&b, state);
    std::fs::remove_file(spawn::request_path(&b.requests(), REQ)).unwrap();
    b
}

#[test]
fn a_pending_staged_row_whose_request_is_gone_is_discarded_at_recovery() {
    let mut b = orphaned(LaunchState::Pending);
    assert!(b.child_state_dir().exists());
    b.restart(FakePane::default());
    b.settle();

    assert!(
        b.pane.steps().is_empty(),
        "nothing launched it, nothing will"
    );
    assert!(b.row("service-2").is_none(), "the stuck row is gone");
    assert!(!b.child_state_dir().exists(), "and so is its reservation");
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
    assert_eq!(b.error_code(REQ), Some(ErrorCode::LaunchFailed));
}

#[test]
fn a_failed_before_start_row_whose_request_is_gone_is_discarded_at_recovery() {
    let mut b = orphaned(LaunchState::FailedBeforeStart);
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty());
    assert!(b.row("service-2").is_none());
    assert!(!b.child_state_dir().exists());
    assert_eq!(b.state(REQ), Some(ReceiptState::Failed));
}

#[test]
fn an_attempted_row_whose_request_is_gone_is_promoted_when_its_terminal_lives() {
    let mut b = orphaned(LaunchState::Attempted);
    let child = b.child_session();
    b.restart(FakePane::live_quiet(&child));
    b.settle();

    assert!(b.pane.steps().is_empty());
    let row = b.row("service-2").unwrap();
    assert!(row.enabled);
    let launch = row.launch.unwrap();
    assert_eq!(launch.state, LaunchState::Started);
    assert_eq!(launch.outcome, Some(SpawnOutcome::Ready));
    assert_eq!(b.state(REQ), Some(ReceiptState::Ready));
}

#[test]
fn an_attempted_row_whose_request_is_gone_is_enabled_as_unknown_when_its_terminal_is_absent() {
    let mut b = orphaned(LaunchState::Attempted);
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty(), "never relaunched");
    let row = b.row("service-2").unwrap();
    assert!(row.enabled && !row.is_staged_spawn(), "a human can see it");
    let launch = row.launch.unwrap();
    assert_eq!(launch.state, LaunchState::Attempted);
    assert_eq!(launch.outcome, Some(SpawnOutcome::OutcomeUnknown));
    assert_eq!(b.state(REQ), Some(ReceiptState::OutcomeUnknown));
}

#[test]
fn a_staged_row_whose_parent_is_gone_is_recovered_without_a_requests_dir() {
    let mut b = staged_then_crashed();
    Registry::update(&b.registry, |registry| {
        registry.projects.retain(|entry| entry.id != "service")
    })
    .unwrap();
    b.restart(FakePane::default());
    b.settle();

    assert!(
        b.pane.steps().is_empty(),
        "a removed session's request never runs"
    );
    assert!(b.rows().is_empty(), "its staged child is discarded");
    assert_eq!(
        b.state(REQ),
        Some(ReceiptState::Staged),
        "nothing answers into a removed session's folder"
    );
}

#[test]
fn a_staged_row_whose_receipt_is_final_is_recovered_too() {
    // The final receipt landed, but the row's own update could not: it is still `attempted`.
    let mut b = staged_then_crashed();
    set_launch_state(&b, LaunchState::Attempted);
    let mut receipt = b.receipt(REQ).unwrap();
    receipt.state = ReceiptState::OutcomeUnknown;
    spawn::write_receipt(&b.requests(), &receipt).unwrap();
    b.restart(FakePane::default());
    b.settle();

    assert!(b.pane.steps().is_empty());
    let row = b.row("service-2").unwrap();
    assert!(row.enabled);
    assert_eq!(
        row.launch.unwrap().outcome,
        Some(SpawnOutcome::OutcomeUnknown)
    );
}

#[test]
fn a_pending_row_whose_request_waits_behind_the_cap_is_left_for_discovery() {
    let mut b = staged_then_crashed();
    // Seventeen other requests with unfinished receipts fill the parent's slots first.
    for n in 1..=17 {
        b.publish(&req_id(n), args(&format!("task {n}")));
        spawn::write_receipt(
            &b.requests(),
            &SpawnReceipt {
                schema_version: spawn::RECEIPT_SCHEMA_VERSION,
                request_id: req_id(n),
                state: ReceiptState::Claimed,
                claimed_by: Some("pid:1".into()),
                session: None,
                launch_state: None,
                error: None,
                next_action: None,
                args_hash: None,
                result: None,
                updated_at: SystemClock.now(),
                work: None,
            },
        )
        .unwrap();
    }
    b.restart(FakePane::default());
    b.step();
    assert!(
        b.row("service-2").is_some(),
        "its request is still there, so it is not an orphan"
    );
}

#[test]
fn a_staged_row_that_names_no_parent_is_not_the_brokers_to_finish() {
    let mut b = broker();
    Registry::update(&b.registry, |registry| {
        let mut row = registry.projects[0].clone();
        row.id = "loose".into();
        row.enabled = false;
        row.launch = Some(LaunchRecord {
            request_id: REQ.into(),
            args_hash: "h".into(),
            state: LaunchState::Pending,
            outcome: None,
            kind: agent_manager::registry::LaunchKind::Job,
            branch: None,
            base_commit: None,
        });
        registry.projects.push(row);
    })
    .unwrap();
    b.settle();
    let row = b.row("loose").expect("left as it is");
    assert!(row.is_staged_spawn());
    assert!(b.pane.steps().is_empty());
}
