//! `pmtui message`: one agent reaches another agent's pane.
//!
//! The pane itself is proven in `tests/integration/message_cli.rs` against real tmux. These
//! pin the parts that decide WHETHER to write and WHAT the recipient reads — the argument
//! grammar, the envelope, the hop bound, and every refusal that must not reach a pane.

use std::cell::RefCell;
use std::path::PathBuf;

use agent_manager::clock::{Clock, Epoch};
use agent_manager::registry::{Engine, Mode, ProjectEntry, Registry};
use agent_manager::state::{ProjectPaths, Tier};
use agent_manager::tmux;

use agent_manager::message::{HOP_MAX, HOP_WINDOW_S, Hops};

use super::FakePane;
use crate::message_cli::*;

const NOW: Epoch = 1_700_000_000;

struct At(Epoch);
impl Clock for At {
    fn now(&self) -> Epoch {
        self.0
    }
}

fn argv(line: &[&str]) -> Vec<String> {
    line.iter().map(|s| (*s).to_string()).collect()
}

fn cli(line: &[&str]) -> MessageCli {
    parse(&argv(line))
        .unwrap_or_else(|e| panic!("parse {line:?}: {}", e.message))
        .expect("not a help request")
}

// --- the argument grammar ------------------------------------------------

#[test]
fn a_send_needs_both_a_peer_and_something_to_say() {
    let parsed = cli(&["--to", "peer", "--message", "hello"]);
    assert_eq!(parsed.to.as_deref(), Some("peer"));
    assert_eq!(parsed.message.as_deref(), Some("hello"));
    assert!(!parsed.list && !parsed.json);

    for (line, why) in [
        (vec!["--message", "hello"], "--to is required"),
        (vec!["--to", "peer"], "--message is required"),
        (vec!["--to", "  "], "--to needs a session id"),
        (vec!["--to", "a", "--to", "b"], "given twice"),
        (vec!["--to"], "needs a value"),
        (vec!["--nope"], "unknown argument"),
        // `--list` is a question, not a send; mixing them hides which one was meant.
        (vec!["--list", "--to", "peer"], "takes no other argument"),
    ] {
        let error = parse(&argv(&line)).expect_err(&format!("{line:?} must not parse"));
        assert!(
            error.message.contains(why),
            "{line:?}: wanted {why:?}, got {:?}",
            error.message
        );
    }
}

#[test]
fn help_is_asked_for_not_errored_on_and_json_survives_a_usage_error() {
    assert!(parse(&argv(&["--help"])).expect("help parses").is_none());
    assert!(parse(&argv(&["-h"])).expect("help parses").is_none());
    // An agent that asked for JSON must get JSON even when its arguments were wrong, or it
    // cannot parse the reason it failed.
    let error = parse(&argv(&["--json", "--nope"])).expect_err("bad flag");
    assert!(error.json, "a usage error keeps --json: {error:?}");
    assert!(cli(&["--list", "--json"]).json);
    assert_eq!(
        cli(&["--list", "--registry", "/tmp/r.json"]).registry,
        Some(PathBuf::from("/tmp/r.json"))
    );
}

// --- what the caller reads ----------------------------------------------

#[test]
fn every_outcome_maps_to_its_documented_exit_code_and_renders_both_ways() {
    let out = |status| MessageOutput {
        status,
        to: "peer".into(),
        from: "me".into(),
        hop: 1,
        detail: "because".into(),
    };
    assert_eq!(exit_code(&out(Status::Delivered)), 0);
    // 3 is the "ask again under the same arguments" family, as it is for a spawn receipt.
    assert_eq!(exit_code(&out(Status::Refused)), 3);
    assert_eq!(exit_code(&out(Status::Failed)), 1);

    assert!(render_plain(&out(Status::Delivered)).starts_with("delivered → peer"));
    assert!(render_plain(&out(Status::Refused)).contains("try again"));
    assert!(render_plain(&out(Status::Failed)).starts_with("failed → peer"));

    let json: serde_json::Value =
        serde_json::from_str(&render_json(&out(Status::Refused))).expect("valid JSON");
    assert_eq!(json["status"], "refused");
    assert_eq!(json["exit_code"], 3);
    assert_eq!(json["ok"], false);
    assert_eq!(json["to"], "peer");
    assert_eq!(json["from"], "me");
    assert_eq!(json["hop"], 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&render_json(&out(Status::Delivered))).unwrap()["ok"],
        true
    );
}

#[test]
fn the_socket_comes_from_the_tmux_variable_every_pane_already_has() {
    assert_eq!(
        socket_from_tmux_env("/tmp/tmux-14017018/pmd,3242192,9").as_deref(),
        Some("pmd")
    );
    assert_eq!(
        socket_from_tmux_env("/tmp/tmux-0/am-check,1,0").as_deref(),
        Some("am-check")
    );
    // Not inside tmux, or a shape this cannot read: there is no pane to write to, and guessing
    // a socket would write to somebody else's server.
    assert_eq!(socket_from_tmux_env(""), None);
    assert_eq!(socket_from_tmux_env(",1,0"), None);
}

// --- the refusals that never reach a pane -------------------------------

/// Everything `run_message` reads, with a driver that records whether it was used at all.
struct Fx {
    dir: tempfile::TempDir,
    registry: PathBuf,
    state: PathBuf,
    driver_asked: RefCell<bool>,
}

impl Fx {
    fn new(rows: &[(&str, &str)]) -> Fx {
        Fx::named(
            &rows
                .iter()
                .map(|(id, root)| (*id, *root, None))
                .collect::<Vec<_>>(),
        )
    }

    fn named(rows: &[(&str, &str, Option<&str>)]) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry.json");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let mut reg = Registry::default();
        for (id, root, name) in rows {
            reg.projects.push(ProjectEntry {
                id: (*id).to_string(),
                display_name: name.map(str::to_string),
                root: dir.path().join(root),
                enabled: true,
                mode: Mode::AgentLoop,
                engine: Some(Engine::Claude),
                worker_model: None,
                initial_prompt: None,
                task_title: None,
                forked_from: None,
                spawned_by: None,
                launch: None,
                conversation_id: None,
                cadence_s: None,
            });
        }
        reg.save(&registry).unwrap();
        Fx {
            dir,
            registry,
            state,
            driver_asked: RefCell::new(false),
        }
    }

    fn cli(&self, to: &str, message: &str) -> MessageCli {
        MessageCli {
            list: false,
            to: Some(to.into()),
            message: Some(message.into()),
            registry: Some(self.registry.clone()),
            json: false,
        }
    }

    /// `env` answers as a managed pane would, minus whatever `absent` names.
    fn run(&self, cli: &MessageCli, from: &str, absent: &[&str], now: Epoch) -> MessageOutput {
        let state = self.state.clone();
        let env = move |key: &str| {
            if absent.contains(&key) {
                return None;
            }
            match key {
                "PMTUI_SESSION" => Some(from.to_string()),
                "PMTUI_STATE_DIR" => Some(state.to_string_lossy().into_owned()),
                "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
                _ => None,
            }
        };
        run_message(
            cli,
            &MessageDeps {
                env: &env,
                clock: &At(now),
                driver: &|_| {
                    *self.driver_asked.borrow_mut() = true;
                    Some(Box::new(FakePane::default()) as Box<dyn tmux::Driver>)
                },
                // A DEAD END on purpose: a test that fell through to the real default would
                // read the developer's own registry and could message a live session.
                default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
            },
        )
    }
}

/// Each of these is refused BEFORE a driver is built, so none of them can touch any pane.
#[test]
fn the_refusals_that_never_reach_a_pane() {
    let fx = Fx::new(&[("peer", "peer"), ("me", "mine")]);
    for (cli, from, absent, why) in [
        // SELF-SEND IS A LOOP: the message becomes this agent's own next turn, which nudges it
        // to act, which can send again.
        (fx.cli("me", "hi"), "me", &[][..], "that is this session"),
        (
            fx.cli("nobody", "hi"),
            "me",
            &[][..],
            "no session with that id",
        ),
        (fx.cli("peer", "   "), "me", &[][..], "nothing typed"),
        (
            fx.cli("peer", "hi"),
            "me",
            &["PMTUI_SESSION"][..],
            "PMTUI_SESSION is not set",
        ),
        (
            fx.cli("peer", "hi"),
            "me",
            &["PMTUI_STATE_DIR"][..],
            "PMTUI_STATE_DIR is not set",
        ),
        // Not inside tmux: there is no pane, and guessing a socket would write to another server.
        (fx.cli("peer", "hi"), "me", &["TMUX"][..], "not inside tmux"),
    ] {
        let out = fx.run(&cli, from, absent, NOW);
        assert_eq!(out.status, Status::Failed, "{why}: {out:?}");
        assert_eq!(exit_code(&out), 1, "{why}: retrying will not help");
        assert!(
            out.detail.contains(why),
            "wanted {why:?}, got {:?}",
            out.detail
        );
    }
    assert!(
        !*fx.driver_asked.borrow(),
        "a refused send must not even build a driver"
    );
}

/// `Registry::load` reads a MISSING registry as an empty one, which is right for the dashboard's
/// first run and misleading here: "no session with that id" would send an agent to a `--list`
/// that is also empty. The path has to be in the words.
#[test]
fn an_unreadable_registry_is_not_reported_as_a_missing_peer() {
    let fx = Fx::new(&[]);
    let out = fx.run(&fx.cli("peer", "hi"), "me", &[], NOW);
    assert_eq!(out.status, Status::Failed);
    assert!(
        out.detail.contains("no sessions in") && out.detail.contains("is --registry right?"),
        "{:?}",
        out.detail
    );
}

/// The ledger is consulted before the pane is touched, and a refused hop says how long to wait.
#[test]
fn the_hop_limit_refuses_without_touching_the_pane() {
    let fx = Fx::new(&[("peer", "peer")]);
    let exhausted = Hops {
        count: HOP_MAX,
        opened_at: NOW,
    };
    let path = fx.state.join("message-hops/peer.json");
    agent_manager::state::write_json_atomic(&path, &exhausted).unwrap();

    let out = fx.run(&fx.cli("peer", "hi"), "me", &[], NOW + 1);
    assert_eq!(out.status, Status::Failed);
    assert_eq!(out.hop, HOP_MAX);
    assert!(
        out.detail.contains("already in the last 10 minutes"),
        "{:?}",
        out.detail
    );
    assert!(
        !*fx.driver_asked.borrow(),
        "the bound must cost nothing at the pane"
    );

    // And after the window it works again — the bound is a rate limit, not a permanent block.
    let out = fx.run(&fx.cli("peer", "hi"), "me", &[], NOW + HOP_WINDOW_S);
    assert_ne!(out.hop, HOP_MAX, "a new window starts over: {out:?}");
    assert!(*fx.driver_asked.borrow(), "it got as far as the pane");
}

/// WHO WILL START IT decides the exit code, and the tier on disk is the only honest source.
///
/// An Autopilot row's agent is absent only until pmd's next sweep, so that is a 3 — ask again.
/// A Standard row's is absent until a human presses Enter, so no retry of THIS command helps,
/// and reporting it as transient would make an agent poll forever. The tier therefore cannot be
/// assumed: it is read from the same per-session `config.json` the dial writes.
#[test]
fn whether_a_missing_agent_is_retryable_comes_from_the_tier_on_disk() {
    for (tier, status, code, why) in [
        (Tier::Autopilot, Status::Refused, 3, "pmd starts it"),
        (Tier::Standard, Status::Failed, 1, "no agent running"),
    ] {
        let fx = Fx::new(&[("peer", "peer")]);
        let paths = ProjectPaths::for_session(fx.dir.path().join("peer"), "peer");
        agent_manager::state::write_json_atomic(
            &paths.config(),
            &serde_json::json!({ "autonomy": tier }),
        )
        .unwrap();

        // `FakePane::default()` knows no sessions, so the peer's pane is not alive.
        let out = fx.run(&fx.cli("peer", "hi"), "me", &[], NOW);
        assert_eq!(out.status, status, "{tier:?}: {out:?}");
        assert_eq!(exit_code(&out), code, "{tier:?}");
        assert!(out.detail.contains(why), "{tier:?}: {:?}", out.detail);
        assert_eq!(out.hop, 1, "{tier:?}: the attempt still counted");
    }
}

// --- listing -------------------------------------------------------------

#[test]
fn the_listing_marks_this_session_and_names_an_empty_registry() {
    let fx = Fx::new(&[("peer", "peer"), ("me", "mine")]);
    let listing = MessageCli {
        list: true,
        to: None,
        message: None,
        registry: Some(fx.registry.clone()),
        json: false,
    };
    let env = |key: &str| match key {
        "PMTUI_SESSION" => Some("me".to_string()),
        "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
        _ => None,
    };
    let deps = MessageDeps {
        env: &env,
        clock: &At(NOW),
        driver: &|_| Some(Box::new(FakePane::default()) as Box<dyn tmux::Driver>),
        default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
    };
    let (path, peers) = run_list(&listing, &deps).expect("the registry is readable");
    assert_eq!(path, fx.registry);
    assert_eq!(peers.len(), 2);
    let me = peers.iter().find(|p| p.id == "me").unwrap();
    assert!(me.is_self, "the caller's own row is marked");
    assert!(!peers.iter().find(|p| p.id == "peer").unwrap().is_self);
    // No session is alive on a fresh FakePane, which a listing reports rather than failing on.
    assert!(peers.iter().all(|p| !p.live && !p.attached));

    let plain = render_list_plain(&path, &peers);
    assert!(
        plain.contains("this session") && plain.contains("no agent running"),
        "{plain}"
    );
    let json: serde_json::Value = serde_json::from_str(&render_list_json(&path, &peers)).unwrap();
    assert_eq!(json["sessions"][0]["id"], "peer");
    assert_eq!(json["registry"], path.to_string_lossy().as_ref());

    // An EMPTY listing names the file, for the same reason the send path does.
    let empty = render_list_plain(&PathBuf::from("/tmp/nope.json"), &[]);
    assert!(
        empty.contains("/tmp/nope.json") && empty.contains("is --registry right?"),
        "{empty}"
    );
}

/// A claude pane idle at its composer: what `pane_refusal` must accept.
const COMPOSER: &str = "did some work\n────────\n❯ \n────────\n";

impl Fx {
    /// Like [`Fx::run`], but with the driver this test wants — `None` stands for a tmux server
    /// that cannot be reached at all.
    fn run_with(
        &self,
        cli: &MessageCli,
        from: &str,
        driver: &dyn Fn(&str) -> Option<Box<dyn tmux::Driver>>,
        now: Epoch,
    ) -> MessageOutput {
        let state = self.state.clone();
        let env = move |key: &str| match key {
            "PMTUI_SESSION" => Some(from.to_string()),
            "PMTUI_STATE_DIR" => Some(state.to_string_lossy().into_owned()),
            "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
            _ => None,
        };
        run_message(
            cli,
            &MessageDeps {
                env: &env,
                clock: &At(now),
                driver,
                default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
            },
        )
    }

    /// The tmux session name of a row, as the CLI must derive it.
    fn session_of(&self, id: &str) -> String {
        tmux::session_name(id, &self.dir.path().join(id))
    }
}

/// THE DELIVERY. What lands is the envelope, it lands on THAT peer's pane and nowhere else, and
/// the agent is told it will be read as a turn rather than answered.
#[test]
fn a_delivered_message_carries_the_envelope_to_that_peers_pane_only() {
    let fx = Fx::new(&[("peer", "peer"), ("other", "other")]);
    let session = fx.session_of("peer");
    let pane = FakePane::live(&session, COMPOSER);
    let handle = pane.clone();

    let out = fx.run_with(
        &fx.cli("peer", "rerun the GPU pair"),
        "me",
        &|_| Some(Box::new(handle.clone()) as Box<dyn tmux::Driver>),
        NOW,
    );

    assert_eq!(out.status, Status::Delivered, "{out:?}");
    assert_eq!(exit_code(&out), 0);
    assert_eq!(out.hop, 1);
    assert!(out.detail.contains("their next turn"), "{:?}", out.detail);
    let sends = pane.sends();
    assert_eq!(sends.len(), 1, "exactly one write: {sends:?}");
    assert_eq!(sends[0].0, session, "to the peer's own pane: {sends:?}");
    assert!(sends[0].1.contains("message from session me"), "{sends:?}");
    assert!(
        sends[0].1.contains("--to me"),
        "the reply address: {sends:?}"
    );
    assert!(sends[0].1.ends_with("rerun the GPU pair"), "{sends:?}");
}

/// A WRITE THAT FAILED IS NOT A DELIVERY. The error is reported as retryable rather than
/// swallowed, because `send_keys` failing mid-paste is exactly the transient case.
#[test]
fn a_pane_write_that_fails_is_reported_and_retryable() {
    let fx = Fx::new(&[("peer", "peer")]);
    let session = fx.session_of("peer");
    let pane = FakePane::live_send_fails(&session, COMPOSER);
    let handle = pane.clone();

    let out = fx.run_with(
        &fx.cli("peer", "hi"),
        "me",
        &|_| Some(Box::new(handle.clone()) as Box<dyn tmux::Driver>),
        NOW,
    );

    assert_eq!(out.status, Status::Refused, "{out:?}");
    assert_eq!(exit_code(&out), 3);
    assert!(!out.detail.is_empty(), "the failure is reported verbatim");
}

/// A HUMAN IN THAT PANE OWNS IT. Their half-typed draft is invisible to us, so our text would be
/// glued onto theirs and submitted — and they will stop being attached, so it is a retry.
#[test]
fn a_human_attached_to_the_peer_refuses_the_send_without_writing() {
    let fx = Fx::new(&[("peer", "peer")]);
    let session = fx.session_of("peer");
    let pane = FakePane::live_attached(&session, COMPOSER);
    let handle = pane.clone();

    let out = fx.run_with(
        &fx.cli("peer", "hi"),
        "me",
        &|_| Some(Box::new(handle.clone()) as Box<dyn tmux::Driver>),
        NOW,
    );

    assert_eq!(out.status, Status::Refused, "{out:?}");
    assert_eq!(exit_code(&out), 3);
    assert!(out.detail.contains("attached"), "{:?}", out.detail);
    assert!(pane.sends().is_empty(), "nothing may be written there");
}

#[test]
fn a_tmux_server_that_cannot_be_reached_is_named() {
    let fx = Fx::new(&[("peer", "peer")]);
    let out = fx.run_with(&fx.cli("peer", "hi"), "me", &|_| None, NOW);
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("cannot reach the tmux server scratch"),
        "{:?}",
        out.detail
    );
}

/// An id is a FILENAME in the hop ledger. A dot is ordinary in a session id, and anything that is
/// not plainly a name is replaced rather than trusted — a `..` would escape the session's own dir.
#[test]
fn a_peer_id_that_is_not_plainly_a_name_still_gets_a_ledger_inside_the_session() {
    let fx = Fx::new(&[("peer.two", "peer.two")]);
    let session = fx.session_of("peer.two");
    let handle = FakePane::live(&session, COMPOSER);

    let out = fx.run_with(
        &fx.cli("peer.two", "hi"),
        "me",
        &|_| Some(Box::new(handle.clone()) as Box<dyn tmux::Driver>),
        NOW,
    );

    assert_eq!(out.status, Status::Delivered, "{out:?}");
    assert!(
        fx.state.join("message-hops/peer_two.json").exists(),
        "the ledger is a plain name inside this session: {:?}",
        std::fs::read_dir(fx.state.join("message-hops")).map(|d| d
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect::<Vec<_>>())
    );
}

/// A REGISTRY THAT EXISTS BUT CANNOT BE PARSED is a different answer from one with no sessions,
/// and both must name the file instead of blaming the id.
#[test]
fn an_unparseable_registry_says_so_and_names_the_path() {
    let fx = Fx::new(&[("peer", "peer")]);
    std::fs::write(&fx.registry, b"{ not json").unwrap();
    let out = fx.run(&fx.cli("peer", "hi"), "me", &[], NOW);
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("cannot read the registry at")
            && out.detail.contains(fx.registry.to_str().unwrap()),
        "{:?}",
        out.detail
    );
}

/// The ledger must be writable before anything is sent, or a send would not be counted and the
/// bound would not hold. A ledger that cannot be written is reported, not ignored.
#[test]
fn a_hop_ledger_that_cannot_be_written_stops_the_send() {
    let fx = Fx::new(&[("peer", "peer")]);
    // A FILE where the ledger directory must go, so creating it cannot succeed.
    std::fs::write(fx.state.join("message-hops"), b"not a directory").unwrap();
    let out = fx.run(&fx.cli("peer", "hi"), "me", &[], NOW);
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("cannot record the hop"),
        "{:?}",
        out.detail
    );
}

/// The listing's whole job is saying which peers a message could reach, so each state it
/// distinguishes has to be visible in the line.
#[test]
fn the_listing_separates_reachable_from_attached() {
    let fx = Fx::new(&[("peer", "peer"), ("busy", "busy")]);
    let listing = MessageCli {
        list: true,
        to: None,
        message: None,
        registry: Some(fx.registry.clone()),
        json: false,
    };
    let reachable = fx.session_of("peer");
    let env = |key: &str| match key {
        "PMTUI_SESSION" => Some("me".to_string()),
        "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
        _ => None,
    };

    // One pane alive and free.
    let free = FakePane::live(&reachable, COMPOSER);
    let (path, peers) = run_list(
        &listing,
        &MessageDeps {
            env: &env,
            clock: &At(NOW),
            driver: &|_| Some(Box::new(free.clone()) as Box<dyn tmux::Driver>),
            default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
        },
    )
    .expect("readable");
    let peer = peers.iter().find(|p| p.id == "peer").unwrap();
    assert!(peer.live && !peer.attached, "{peer:?}");
    assert!(render_list_plain(&path, &peers).contains("reachable"));

    // The same pane with a human in it.
    let taken = FakePane::live_attached(&reachable, COMPOSER);
    let (path, peers) = run_list(
        &listing,
        &MessageDeps {
            env: &env,
            clock: &At(NOW),
            driver: &|_| Some(Box::new(taken.clone()) as Box<dyn tmux::Driver>),
            default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
        },
    )
    .expect("readable");
    let peer = peers.iter().find(|p| p.id == "peer").unwrap();
    assert!(peer.live && peer.attached, "{peer:?}");
    assert!(
        render_list_plain(&path, &peers).contains("a human is attached"),
        "the line must say why it is not reachable"
    );
}

/// THE WHOLE COMMAND LINE, through `main`'s own dispatch — the arms that pick a renderer and an
/// exit code, which no `run_message` test reaches.
fn dispatch_message(
    line: &[&str],
    fx: &Fx,
    driver_live: Option<&FakePane>,
) -> (i32, String, String) {
    let state = fx.state.clone();
    let env = move |key: &str| match key {
        "PMTUI_SESSION" => Some("me".to_string()),
        "PMTUI_STATE_DIR" => Some(state.to_string_lossy().into_owned()),
        "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
        _ => None,
    };
    let pane = driver_live.cloned();
    let deps = MessageDeps {
        env: &env,
        clock: &At(NOW),
        driver: &|_| pane.clone().map(|p| Box::new(p) as Box<dyn tmux::Driver>),
        default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
    };
    let mut waiter = crate::spawn_cli::ThreadWaiter::new();
    let mut spawn = crate::spawn_cli::SpawnDeps {
        env: &|_| None,
        clock: &At(NOW),
        waiter: &mut waiter,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = crate::dispatch(
        crate::parse_action(&argv(line)),
        &mut |_| panic!("a message command must never open the dashboard"),
        &mut spawn,
        &deps,
        &mut out,
        &mut err,
    )
    .unwrap();
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn the_command_line_renders_a_send_both_ways_and_exits_as_documented() {
    let fx = Fx::new(&[("peer", "peer")]);
    let reg = fx.registry.to_str().unwrap().to_string();
    let pane = FakePane::live(&fx.session_of("peer"), COMPOSER);

    let (code, out, err) = dispatch_message(
        &[
            "message",
            "--to",
            "peer",
            "--message",
            "hi",
            "--registry",
            &reg,
        ],
        &fx,
        Some(&pane),
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.starts_with("delivered → peer"), "{out:?}");
    assert!(err.is_empty(), "{err:?}");

    // --json on the SAME command is a document, and the message text is never echoed back.
    let (code, out, _) = dispatch_message(
        &[
            "message",
            "--to",
            "peer",
            "--message",
            "hi",
            "--json",
            "--registry",
            &reg,
        ],
        &fx,
        Some(&pane),
    );
    let json: serde_json::Value = serde_json::from_str(&out).expect("one JSON document");
    assert_eq!((code, json["exit_code"].as_i64()), (0, Some(0)));
    assert_eq!(json["hop"], 2, "the second send of this window");
    assert!(!out.contains("\"hi\""), "the message is not echoed: {out}");
}

#[test]
fn the_command_line_renders_a_listing_both_ways_and_reports_an_unreadable_one() {
    let fx = Fx::new(&[("peer", "peer")]);
    let reg = fx.registry.to_str().unwrap().to_string();

    let (code, out, _) = dispatch_message(&["message", "--list", "--registry", &reg], &fx, None);
    assert_eq!(code, 0, "a listing is not an outcome: {out}");
    assert!(out.contains("peer"), "{out}");

    let (code, out, _) = dispatch_message(
        &["message", "--list", "--json", "--registry", &reg],
        &fx,
        None,
    );
    let json: serde_json::Value = serde_json::from_str(&out).expect("one JSON document");
    assert_eq!(
        (code, json["sessions"][0]["id"].as_str()),
        (0, Some("peer"))
    );

    // A registry that cannot be PARSED fails the listing and says so on stderr.
    std::fs::write(&fx.registry, b"{ not json").unwrap();
    let (code, out, err) = dispatch_message(&["message", "--list", "--registry", &reg], &fx, None);
    assert_eq!(code, 1, "{out}{err}");
    assert!(err.contains("message --list:"), "{err:?}");
    assert!(err.contains("cannot read the registry at"), "{err:?}");
}

#[test]
fn asking_for_help_prints_the_message_help_and_exits_zero() {
    let fx = Fx::new(&[]);
    for line in [vec!["message", "--help"], vec!["message", "-h"]] {
        let (code, out, err) = dispatch_message(&line, &fx, None);
        assert_eq!(code, 0, "{line:?}");
        assert!(
            out.is_empty(),
            "help goes to stderr like the others: {out:?}"
        );
        assert!(err.starts_with("pmtui message —"), "{line:?}: {err:?}");
        assert!(err.contains("Exit codes:"), "{line:?}");
    }
}

/// WITHOUT `--registry` the dashboard's own path is used. Asserted through a fixture whose default
/// is a path that CANNOT exist, which is the point: a test that fell through to the real default
/// would read the developer's own registry and could message a live session.
#[test]
fn omitting_the_registry_falls_back_to_the_dashboards_path() {
    let fx = Fx::new(&[("peer", "peer")]);
    let no_registry = MessageCli {
        list: false,
        to: Some("peer".into()),
        message: Some("hi".into()),
        registry: None,
        json: false,
    };
    let out = fx.run(&no_registry, "me", &[], NOW);
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("/nonexistent/pmtui-test/registry.json"),
        "the fallback path must be the one it reports: {:?}",
        out.detail
    );

    // And the listing takes the same fallback.
    let listing = MessageCli {
        list: true,
        to: None,
        message: None,
        registry: None,
        json: false,
    };
    let env = |key: &str| match key {
        "PMTUI_SESSION" => Some("me".to_string()),
        "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
        _ => None,
    };
    let (path, peers) = run_list(
        &listing,
        &MessageDeps {
            env: &env,
            clock: &At(NOW),
            driver: &|_| None,
            default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
        },
    )
    .expect("a missing registry reads as an empty one");
    assert_eq!(path, PathBuf::from("/nonexistent/pmtui-test/registry.json"));
    assert!(peers.is_empty());
}

/// TWO SESSIONS CAN SHARE A FOLDER AND A NAME; only the id addresses one.
///
/// `normalize_display_name` enforces no uniqueness, and two sessions legitimately live in one
/// project folder (the user's own registry has a pair). So the listing must show both rows, name
/// them without pretending the name is an address, and a send must reach the pane of the id it was
/// given — the pane name is keyed on `(id, root)`, so the sibling's pane is a different one.
#[test]
fn same_folder_same_name_sessions_are_still_addressed_by_id() {
    let fx = Fx::named(&[
        ("proj", "shared", Some("The Build")),
        ("proj-2", "shared", Some("The Build")),
        ("plain", "plain", None),
    ]);
    // Both rows point at ONE folder, as two sessions in one project do.
    let first = tmux::session_name("proj", &fx.dir.path().join("shared"));
    let second = tmux::session_name("proj-2", &fx.dir.path().join("shared"));
    assert_ne!(
        first, second,
        "a shared folder must not collapse two sessions onto one pane"
    );

    let listing = MessageCli {
        list: true,
        to: None,
        message: None,
        registry: Some(fx.registry.clone()),
        json: false,
    };
    let env = |key: &str| match key {
        "PMTUI_SESSION" => Some("me".to_string()),
        "TMUX" => Some("/tmp/tmux-0/scratch,1,0".to_string()),
        _ => None,
    };
    let live = FakePane::live(&second, COMPOSER);
    let handle = live.clone();
    let (path, peers) = run_list(
        &listing,
        &MessageDeps {
            env: &env,
            clock: &At(NOW),
            driver: &|_| Some(Box::new(handle.clone()) as Box<dyn tmux::Driver>),
            default_registry: &|| PathBuf::from("/nonexistent/pmtui-test/registry.json"),
        },
    )
    .expect("readable");

    assert_eq!(peers.len(), 3, "both same-folder sessions are listed");
    assert_eq!(peers[0].name.as_deref(), Some("The Build"));
    assert_eq!(peers[1].name.as_deref(), Some("The Build"));
    assert_eq!(peers[2].name, None, "an unnamed session shows no name");
    // Only the one whose pane is up is reachable, which is the point of listing them apart.
    assert!(!peers[0].live && peers[1].live, "{peers:?}");

    let plain = render_list_plain(&path, &peers);
    assert!(
        plain.contains("proj (The Build)") && plain.contains("proj-2 (The Build)"),
        "each line leads with its own id: {plain}"
    );
    assert!(
        plain.lines().any(|l| l.starts_with("plain  ")),
        "an unnamed session is just its id: {plain}"
    );
    let json: serde_json::Value = serde_json::from_str(&render_list_json(&path, &peers)).unwrap();
    assert_eq!(json["sessions"][1]["id"], "proj-2");
    assert_eq!(json["sessions"][1]["name"], "The Build");
    assert!(
        json["sessions"][2].get("name").is_none(),
        "no name is absent, not null"
    );

    // AND THE SEND GOES BY ID: addressed to `proj-2`, it lands on `proj-2`'s pane only.
    let out = fx.run_with(
        &fx.cli("proj-2", "hi"),
        "me",
        &|_| Some(Box::new(live.clone()) as Box<dyn tmux::Driver>),
        NOW,
    );
    assert_eq!(out.status, Status::Delivered, "{out:?}");
    let sends = live.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, second, "the sibling's pane is never written");

    // A NAME IS NOT AN ADDRESS: sending to the shared display name reaches nobody.
    let out = fx.run_with(
        &fx.cli("The Build", "hi"),
        "me",
        &|_| Some(Box::new(live.clone()) as Box<dyn tmux::Driver>),
        NOW,
    );
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("no session with that id"),
        "{:?}",
        out.detail
    );
    assert_eq!(live.sends().len(), 1, "and still nothing new was written");
}

/// A NAME IS NOT AN ADDRESS, but it is what a human says: the user's `start-up` session is called
/// `gpubox`. So a name never delivers — it resolves to the id to use instead, and when the name is
/// ambiguous it refuses with every candidate rather than picking one.
#[test]
fn a_display_name_resolves_to_an_id_and_never_delivers_on_its_own() {
    let fx = Fx::named(&[
        ("start-up", "start-up", Some("gpubox")),
        ("proj", "shared", Some("The Build")),
        ("proj-2", "shared", Some("The Build")),
    ]);
    let live = FakePane::live(&fx.session_of("start-up"), COMPOSER);
    let handle = live.clone();
    let send = |to: &str| {
        fx.run_with(
            &fx.cli(to, "hi"),
            "me",
            &|_| Some(Box::new(handle.clone()) as Box<dyn tmux::Driver>),
            NOW,
        )
    };

    // ONE session has this name: say which id to use.
    let out = send("gpubox");
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("start-up is the one named gpubox"),
        "{:?}",
        out.detail
    );

    // SEVERAL share it: refuse with all of them, because guessing could reach the wrong agent.
    let out = send("The Build");
    assert_eq!(out.status, Status::Failed, "{out:?}");
    assert!(
        out.detail.contains("2 sessions are named The Build")
            && out.detail.contains("proj")
            && out.detail.contains("proj-2"),
        "{:?}",
        out.detail
    );

    // A word that is neither an id nor a name keeps the plain answer.
    assert!(
        send("nothing-at-all")
            .detail
            .contains("no session with that id — see --list")
    );

    // NOTHING was written on any of those paths.
    assert!(
        live.sends().is_empty(),
        "a name must not deliver: {:?}",
        live.sends()
    );

    // And the id still works.
    let out = send("start-up");
    assert_eq!(out.status, Status::Delivered, "{out:?}");
    assert_eq!(live.sends().len(), 1);
}
