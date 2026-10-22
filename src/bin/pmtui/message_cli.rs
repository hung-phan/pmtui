//! `pmtui message`: how an agent inside a managed session reaches another session's agent.
//!
//! One send, delivered synchronously, creating NO state that outlives it. That is the whole
//! difference from `pmtui spawn`, and it is why there is no request/receipt protocol here:
//! spawn needs the dashboard to broker because only the dashboard may turn a request into a
//! registry row, whereas a message writes no ledger, no `answers.json` and no registry row —
//! it becomes one ordinary user turn in the recipient's own transcript. So the command does the
//! delivery itself, through the same [`send`] gate and the same `input.lock` the dashboard's `s`
//! key uses, and the dashboard need not even be running.
//!
//! A REPLY IS JUST A SEND BACK, which is what makes this a conversation without a mailbox: the
//! envelope tells the recipient who asked and what command answers them, and their answer arrives
//! in the sender's pane as an ordinary turn. Nothing polls, nothing blocks, and no agent can
//! deadlock waiting on another's turn.
//!
//! Everything it needs is already in a managed pane's environment, so `ManagedEnv` is untouched:
//! `PMTUI_SESSION` is the sender, `PMTUI_STATE_DIR` yields the sender's root and houses the hop
//! ledger, and `$TMUX` names the tmux socket. The registry is the one thing not in the
//! environment — it maps an id to its project root, which a tmux session name only hashes — so it
//! falls back to the dashboard's default path and `--registry` overrides it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use agent_manager::clock::Clock;
use agent_manager::registry::{Mode, Registry};
use agent_manager::state::{Config, ProjectPaths};
use agent_manager::tmux::{self, ENV_SESSION, ENV_STATE_DIR};

use agent_manager::message::{HOP_MAX, HOP_WINDOW_S, Hops, envelope, next_hop};

use crate::send::{self, SendPlan, SendTarget, send_decision, send_precheck};
use crate::spawn_cli::{UsageError, wants_json};

pub(crate) const HELP: &str = concat!(
    "pmtui message — send one message to another session's agent\n\n",
    "Usage: pmtui message --to <session-id> --message <text> [--json] [--registry <path>]\n",
    "       pmtui message --list [--json] [--registry <path>]\n\n",
    "Run it inside a pmtui-managed session. The message arrives in the other agent's pane as\n",
    "one ordinary user turn, labelled with this session's id and how to reply.\n\n",
    "--to <session-id>   who to send to, as shown by --list\n",
    "--message <text>    what to say (required with --to)\n",
    "--list              every session this one can reach, one line each\n",
    "--registry <path>   registry JSON (default: ~/.config/pmd/registry.json)\n",
    "--json              print one JSON document instead of one line\n\n",
    "A reply is just a send back: the recipient runs `pmtui message --to <this session>`.\n",
    "Nothing blocks and nothing is queued — their answer arrives as a turn in this pane.\n\n",
    "Exit codes: 0 delivered (or listed) · 3 try again shortly (a human is attached there, a\n",
    "dialog is up, the pane has an unsent draft, or another input is mid-delivery) · 1 failed\n",
    "(no such session, its agent is not running, or the hop limit) · 2 usage error\n",
);

/// What one `pmtui message` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageCli {
    pub(crate) list: bool,
    pub(crate) to: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) registry: Option<PathBuf>,
    pub(crate) json: bool,
}

const VALUE_FLAGS: &[&str] = &["--to", "--message", "--registry"];

/// Parse a whole `message` command line. `Ok(None)` asks for help.
pub(crate) fn parse(argv: &[String]) -> Result<Option<MessageCli>, UsageError> {
    let usage = |message: String| UsageError {
        message: format!("message: {message}"),
        json: wants_json(argv),
    };
    let mut values: Vec<(&str, String)> = Vec::new();
    let mut json = false;
    let mut list = false;
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        match flag {
            "-h" | "--help" => return Ok(None),
            "--json" => json = true,
            "--list" => list = true,
            _ if VALUE_FLAGS.contains(&flag) => {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| usage(format!("{flag} needs a value")))?;
                if values.iter().any(|(seen, _)| *seen == flag) {
                    return Err(usage(format!("{flag} was given twice")));
                }
                values.push((flag, value.clone()));
                i += 1;
            }
            other => return Err(usage(format!("unknown argument {other:?}"))),
        }
        i += 1;
    }
    let take = |name: &str| {
        values
            .iter()
            .find(|(flag, _)| *flag == name)
            .map(|(_, value)| value.clone())
    };
    let to = take("--to");
    let message = take("--message");
    if list {
        if to.is_some() || message.is_some() {
            return Err(usage("--list takes no other argument".into()));
        }
        return Ok(Some(MessageCli {
            list: true,
            to: None,
            message: None,
            registry: take("--registry").map(PathBuf::from),
            json,
        }));
    }
    let to = to.ok_or_else(|| usage("--to is required (or --list)".into()))?;
    if to.trim().is_empty() {
        return Err(usage("--to needs a session id".into()));
    }
    let message = message.ok_or_else(|| usage("--message is required with --to".into()))?;
    Ok(Some(MessageCli {
        list: false,
        to: Some(to),
        message: Some(message),
        registry: take("--registry").map(PathBuf::from),
        json,
    }))
}

/// What became of one send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    /// The bytes reached the pane.
    Delivered,
    /// The pane could not take it right now; the same command will work shortly.
    Refused,
    /// It will not work by retrying.
    Failed,
}

/// One send's outcome, rendered as one line or one JSON document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MessageOutput {
    pub(crate) status: Status,
    /// Who it was for, as given.
    pub(crate) to: String,
    /// The sender's own id, so a JSON consumer need not re-read the environment.
    pub(crate) from: String,
    /// Which send to this peer this was, inside the current window.
    pub(crate) hop: u32,
    /// What happened, in the words a human (or an agent) should act on. Never the message text.
    pub(crate) detail: String,
}

/// `0` delivered · `3` try again shortly · `1` failed.
pub(crate) fn exit_code(out: &MessageOutput) -> i32 {
    match out.status {
        Status::Delivered => 0,
        // The 3 family means "ask again under the same arguments", exactly as it does for a
        // spawn receipt that is still `in_progress`: the pane state that refused is transient.
        Status::Refused => 3,
        Status::Failed => 1,
    }
}

pub(crate) fn render_json(out: &MessageOutput) -> String {
    let mut value = serde_json::to_value(out).unwrap_or(serde_json::Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert("exit_code".into(), exit_code(out).into());
        map.insert("ok".into(), (out.status == Status::Delivered).into());
    }
    // Every field is a string, a number or a bool, so this cannot fail.
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

pub(crate) fn render_plain(out: &MessageOutput) -> String {
    match out.status {
        Status::Delivered => format!("delivered → {} ({})", out.to, out.detail),
        Status::Refused => format!("not delivered → {}: {} — try again", out.to, out.detail),
        Status::Failed => format!("failed → {}: {}", out.to, out.detail),
    }
}

/// One reachable session, as `--list` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Peer {
    /// The ADDRESS. Unique by construction, and what `--to` matches.
    pub(crate) id: String,
    /// The label a human gave it, when they gave one. Shown because a human thinks of a session by
    /// its name, and NOT an address: `normalize_display_name` enforces no uniqueness, so two
    /// sessions can share one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) root: String,
    pub(crate) engine: String,
    /// Its agent's pane is up, so a message can land.
    pub(crate) live: bool,
    /// A human is in that pane; a send would be refused rather than type over them.
    pub(crate) attached: bool,
    /// This is the session running the command — sending to it is refused.
    #[serde(rename = "self")]
    pub(crate) is_self: bool,
}

pub(crate) fn render_list_json(path: &Path, peers: &[Peer]) -> String {
    // Strings and bools only, so this cannot fail.
    serde_json::to_string_pretty(
        &serde_json::json!({ "registry": path.to_string_lossy(), "sessions": peers }),
    )
    .unwrap_or_default()
}

pub(crate) fn render_list_plain(path: &Path, peers: &[Peer]) -> String {
    if peers.is_empty() {
        return format!(
            "no sessions in {} — is --registry right?",
            path.to_string_lossy()
        );
    }
    peers
        .iter()
        .map(|p| {
            let mut tags = Vec::new();
            if p.is_self {
                tags.push("this session");
            }
            if !p.live {
                tags.push("no agent running");
            }
            if p.attached {
                tags.push("a human is attached");
            }
            let note = if tags.is_empty() {
                "reachable".to_string()
            } else {
                tags.join(", ")
            };
            // The id comes FIRST and alone, because it is the only field `--to` accepts.
            let named = match &p.name {
                Some(name) => format!("{} ({name})", p.id),
                None => p.id.clone(),
            };
            format!("{named}  {}  {}  — {}", p.engine, p.root, note)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tmux socket NAME this pane's server is on, from `$TMUX`
/// (`<socket-path>,<pid>,<window>`), or `None` outside tmux.
pub(crate) fn socket_from_tmux_env(tmux_var: &str) -> Option<String> {
    let path = tmux_var.split(',').next()?;
    if path.is_empty() {
        return None;
    }
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Where this session records its sends to `peer`.
///
/// The id comes from the registry, but it still becomes a FILENAME here, so anything that is
/// not plainly a name is replaced rather than trusted — a `..` segment would otherwise escape
/// the session's own directory.
fn hop_path(state_dir: &Path, peer: &str) -> PathBuf {
    let stem: String = peer
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    state_dir.join("message-hops").join(format!("{stem}.json"))
}

/// Everything the command reads from outside itself, injected so every arm is testable.
pub(crate) struct MessageDeps<'a> {
    /// `PMTUI_SESSION`, `PMTUI_STATE_DIR`, `TMUX`.
    pub(crate) env: &'a dyn Fn(&str) -> Option<String>,
    pub(crate) clock: &'a dyn Clock,
    /// Built from the resolved socket; `None` when a driver could not be made.
    pub(crate) driver: &'a dyn Fn(&str) -> Option<Box<dyn tmux::Driver>>,
    /// `~/.config/pmd/registry.json`.
    pub(crate) default_registry: &'a dyn Fn() -> PathBuf,
}

fn failed(to: &str, from: &str, hop: u32, detail: impl Into<String>) -> MessageOutput {
    MessageOutput {
        status: Status::Failed,
        to: to.into(),
        from: from.into(),
        hop,
        detail: detail.into(),
    }
}

/// Resolve the registry path, then load it.
fn load_registry(cli: &MessageCli, deps: &MessageDeps<'_>) -> Result<(PathBuf, Registry), String> {
    let path = cli
        .registry
        .clone()
        .unwrap_or_else(|| (deps.default_registry)());
    let registry = Registry::load(&path).map_err(|error| {
        format!(
            "cannot read the registry at {}: {error}",
            path.to_string_lossy()
        )
    })?;
    Ok((path, registry))
}

/// The tmux session that carries a row's agent — the same name pmd and pmtui use, never
/// re-derived, so all three can never disagree about which pane belongs to a session.
fn target_of(entry: &agent_manager::registry::ProjectEntry) -> SendTarget {
    // The TIER IS READ, never assumed. It is the difference between "pmd will start it"
    // (retryable) and "nothing will" (a dead end), and it is the dial pmtui's `m` writes — so it
    // comes from the same per-session `config.json` the daemon reads, on every invocation.
    let paths = ProjectPaths::for_session(&entry.root, &entry.id);
    let tier = agent_manager::state::read_json::<Config>(&paths.config())
        .ok()
        .map(|config| config.autonomy);
    SendTarget {
        id: entry.id.clone(),
        root: entry.root.clone(),
        session: tmux::session_name(&entry.id, &entry.root),
        agent_loop: entry.mode == Mode::AgentLoop,
        driven: agent_manager::daemon::pmd_drives_row(entry.mode, tier),
        // shortcut: always the loop pane, so a session whose agent is in a detached `pmchat-`
        // REPL instead reads as "no agent running" rather than being reached there; upgrade by
        // mirroring `App::resolve_send_pane` if messaging a paused-with-open-chat session matters.
        in_chat: false,
    }
}

/// `--list`: every session in the registry, with whether a message could land there.
pub(crate) fn run_list(
    cli: &MessageCli,
    deps: &MessageDeps<'_>,
) -> Result<(PathBuf, Vec<Peer>), String> {
    let (path, registry) = load_registry(cli, deps)?;
    let me = (deps.env)(ENV_SESSION).unwrap_or_default();
    let driver = (deps.env)("TMUX")
        .as_deref()
        .and_then(socket_from_tmux_env)
        .and_then(|socket| (deps.driver)(&socket));
    let peers = registry
        .projects
        .iter()
        .map(|entry| {
            let session = tmux::session_name(&entry.id, &entry.root);
            // No driver (not inside tmux) is reported as "not live" rather than failing the
            // listing: a listing is not an outcome, and the ids are still what the caller needs.
            let live = driver
                .as_ref()
                .and_then(|d| d.is_alive(&session).ok())
                .unwrap_or(false);
            Peer {
                id: entry.id.clone(),
                name: entry.display_name.clone(),
                root: entry.root.to_string_lossy().into_owned(),
                engine: entry.engine.map_or("default", |e| e.label()).to_string(),
                live,
                attached: live
                    && driver
                        .as_ref()
                        .and_then(|d| d.has_clients(&session).ok())
                        .unwrap_or(false),
                is_self: entry.id == me,
            }
        })
        .collect();
    Ok((path, peers))
}

/// Deliver one message. Every refusal names what to do about it.
pub(crate) fn run_message(cli: &MessageCli, deps: &MessageDeps<'_>) -> MessageOutput {
    let to = cli.to.clone().unwrap_or_default();
    let text = cli.message.clone().unwrap_or_default();
    let from = (deps.env)(ENV_SESSION).unwrap_or_default();
    if from.is_empty() {
        return failed(
            &to,
            &from,
            0,
            "PMTUI_SESSION is not set — run this inside a pmtui-managed session",
        );
    }
    // SELF-SEND IS A LOOP, not a no-op: the message lands as this agent's own next turn, which
    // nudges it to act, which can send again.
    if to == from {
        return failed(
            &to,
            &from,
            0,
            "that is this session — pick another from --list",
        );
    }
    if let Some(SendPlan::Refuse(why)) = send_precheck(&text) {
        return failed(&to, &from, 0, why);
    }
    let Some(state_dir) = (deps.env)(ENV_STATE_DIR).map(PathBuf::from) else {
        return failed(
            &to,
            &from,
            0,
            "PMTUI_STATE_DIR is not set — run this inside a pmtui-managed session",
        );
    };
    let (registry_path, registry) = match load_registry(cli, deps) {
        Ok(loaded) => loaded,
        Err(message) => return failed(&to, &from, 0, message),
    };
    if registry.projects.is_empty() {
        return failed(
            &to,
            &from,
            0,
            format!(
                "no sessions in {} — is --registry right?",
                registry_path.to_string_lossy()
            ),
        );
    }
    let Some(entry) = registry.projects.iter().find(|entry| entry.id == to) else {
        // A NAME IS NOT AN ADDRESS, and a human's own word for a session usually is its name:
        // `start-up` is called `gpubox`. Names are not unique (`normalize_display_name` enforces
        // nothing), so this NEVER delivers on one — it only turns a dead end into the id to use.
        let by_name: Vec<&str> = registry
            .projects
            .iter()
            .filter(|entry| entry.display_name.as_deref() == Some(to.as_str()))
            .map(|entry| entry.id.as_str())
            .collect();
        let hint = match by_name.as_slice() {
            [] => "no session with that id — see --list".to_string(),
            [only] => format!("no session with that id; {only} is the one named {to} — use that"),
            several => format!(
                "no session with that id, and {} sessions are named {to} ({}) — a name is not an \
                 address, so pick one id",
                several.len(),
                several.join(", ")
            ),
        };
        return failed(&to, &from, 0, hint);
    };
    let target = target_of(entry);
    // The hop ledger is consulted BEFORE the pane is touched, so a refused send costs nothing,
    // and advanced only once the limit allows it — a send the pane then refuses still counts,
    // because a retry loop against a busy peer is the same runaway this bounds.
    let now = deps.clock.now();
    let path = hop_path(&state_dir, &to);
    let previous = agent_manager::state::read_json::<Hops>(&path).ok();
    let Some(hops) = next_hop(previous, now) else {
        return failed(
            &to,
            &from,
            HOP_MAX,
            format!(
                "{HOP_MAX} messages to {to} already in the last {} minutes — \
                 a reply loop is bounded here; wait it out or tell a human",
                HOP_WINDOW_S / 60
            ),
        );
    };
    if let Err(error) = agent_manager::state::write_json_atomic(&path, &hops) {
        return failed(
            &to,
            &from,
            hops.count,
            format!("cannot record the hop: {error}"),
        );
    }

    let Some(socket) = (deps.env)("TMUX").as_deref().and_then(socket_from_tmux_env) else {
        return failed(
            &to,
            &from,
            0,
            "not inside tmux ($TMUX is unset), so there is no pane to write to",
        );
    };
    let Some(driver) = (deps.driver)(&socket) else {
        return failed(
            &to,
            &from,
            0,
            format!("cannot reach the tmux server {socket}"),
        );
    };

    let plan = send_decision(&target, &send::probe(&*driver, &target));
    if let SendPlan::Refuse(why) = plan {
        return MessageOutput {
            // A pane that is not up at all will not become reachable by retrying this command;
            // every other refusal is the transient pane state the 3 family is for.
            status: if why.contains("nothing to type into") || why.contains("no agent running") {
                Status::Failed
            } else {
                Status::Refused
            },
            to,
            from,
            hop: hops.count,
            detail: why,
        };
    }
    match send::write_to_pane(&*driver, &target, &envelope(&from, hops.count, &text)) {
        Ok(()) => MessageOutput {
            status: Status::Delivered,
            detail: "it arrives as their next turn".into(),
            to,
            from,
            hop: hops.count,
        },
        // The write path's own refusals (the lock was busy, a client attached, the pane changed
        // while we waited) are all transient by construction.
        Err(error) => MessageOutput {
            status: Status::Refused,
            detail: format!("{error:#}"),
            to,
            from,
            hop: hops.count,
        },
    }
}
