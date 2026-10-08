//! Real-pmtui acceptance for native conversation forks. Engine binaries are
//! inert local stubs; tmux, registry publication, identity capture, source
//! stability checks, and UI dispatch are production paths.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_manager::job;
use agent_manager::registry::{Engine, Registry};
use agent_manager::state::{self, Config, ProjectPaths, Tier};
use agent_manager::tmux::{Driver, session_name};

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{EnterFixture, PmdSibling, enter_fixture_with_env};
use crate::probe::{
    list_clients, probe_pane_pid, tmux_available, wait_for_pane_text_within, wait_until,
};
use crate::seed::seed_standard_loop_session_at;

const SOURCE_ID: &str = "11111111-aaaa-4bbb-8ccc-555555555555";
const CHILD_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

/// How the source stub behaves when it is launched as the source session.
enum SourceStub {
    /// Draws one answer and waits at its prompt.
    Idle,
    /// Keeps appending transcript above a bare prompt, the frame Claude draws mid-stream.
    Streaming,
}

/// One real dashboard plus the scratch engine state its stubs read. `fx` is declared first so
/// its servers are torn down before the scratch Claude config is removed.
struct ForkRig {
    fx: EnterFixture,
    _claude_config: tempfile::TempDir,
    engine: Engine,
    launches: PathBuf,
}

impl ForkRig {
    fn source_session(&self) -> String {
        session_name("bot", &self.fx.proj)
    }

    fn child_session(&self) -> String {
        session_name("bot-fork", &self.fx.proj)
    }

    fn launch_log(&self) -> String {
        std::fs::read_to_string(&self.launches).unwrap_or_default()
    }

    /// Press `f` on the selected source and wait for the child row to be published.
    fn fork_and_wait_for_child(&self) {
        let fx = &self.fx;
        assert!(send_literal(&fx.host_socket, &fx.host_session, "f"));
        let promoted = wait_until(Duration::from_secs(20), || {
            Registry::load(&fx.reg_path).ok().is_some_and(|registry| {
                registry.projects.iter().any(|entry| {
                    entry.id == "bot-fork"
                        && entry.enabled
                        && entry.conversation_id.as_deref() == Some(CHILD_ID)
                        && entry.forked_from.as_deref() == Some("bot")
                })
            })
        });
        assert!(
            promoted,
            "{:?} fork not promoted; registry={:?}; dashboard={:?}; launches={:?}",
            self.engine,
            Registry::load(&fx.reg_path).map(|registry| registry.projects),
            fx.host.capture_tail(&fx.host_session, 80),
            self.launch_log(),
        );
        assert!(wait_for_pane_text_within(
            &fx.host,
            &fx.host_session,
            "bot-fork",
            Duration::from_secs(10)
        ));
    }

    /// Resume the paused source through Enter, which starts it detached in its real `pm-`
    /// terminal exactly as a human would, then wait for its first frame and for the dashboard's
    /// own idle gate to see it settle.
    fn start_source(&self, first_frame: &str) {
        let fx = &self.fx;
        assert!(send_key(&fx.host_socket, &fx.host_session, "Enter"));
        assert!(
            wait_for_pane_text_within(
                &fx.agent,
                &self.source_session(),
                first_frame,
                Duration::from_secs(10)
            ),
            "source never drew {first_frame:?}; dashboard={:?}",
            fx.host.capture_tail(&fx.host_session, 80),
        );
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Seed a Standard `bot` session with copied human context and install one stub that plays
/// both the source and the fork child. `saved` records `SOURCE_ID` in the registry and ledger;
/// `paused` leaves the row disabled so Enter starts its terminal.
fn rig(tag: &str, engine: Engine, saved: bool, paused: bool, source: SourceStub) -> ForkRig {
    let claude_config = tempfile::tempdir().unwrap();
    let fx = enter_fixture_with_env(
        tag,
        PmdSibling::Missing,
        300,
        50,
        &[("CLAUDE_CONFIG_DIR", claude_config.path())],
    );
    assert!(fx.up, "pmtui fixture did not start for {engine:?}");
    // The source transcript exists, so the dashboard treats SOURCE_ID as a started conversation.
    let slug: String = fx
        .proj
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let transcripts = claude_config.path().join("projects").join(slug);
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join(format!("{SOURCE_ID}.jsonl")), "{}\n").unwrap();

    seed_standard_loop_session_at(&fx.reg_path, &fx.proj, "bot", 300);
    let mut registry = Registry::load(&fx.reg_path).unwrap();
    let entry = &mut registry.projects[0];
    entry.engine = Some(engine);
    entry.enabled = !paused;
    entry.conversation_id = saved.then(|| SOURCE_ID.to_string());
    registry.save(&fx.reg_path).unwrap();
    let source_paths = ProjectPaths::for_session(&fx.proj, "bot");
    let mut ledger = job::load(&source_paths).unwrap().unwrap();
    ledger.engine = engine;
    ledger.conversation_id = saved.then(|| SOURCE_ID.to_string());
    job::save(&source_paths, &ledger).unwrap();
    state::write_text_atomic(&source_paths.brief(), "fork acceptance goal").unwrap();
    state::write_text_atomic(&source_paths.directive(), "do not publish").unwrap();

    let launches = fx.dir.path().join(format!("{tag}-launches.log"));
    install_stub(&fx, engine, &launches, source);
    assert!(wait_for_pane_text_within(
        &fx.host,
        &fx.host_session,
        "bot",
        Duration::from_secs(10)
    ));
    ForkRig {
        fx,
        _claude_config: claude_config,
        engine,
        launches,
    }
}

/// The published child is Standard, carries the copied goal and directive, runs in its own live
/// terminal, and the source registry row is exactly what it was before the fork.
fn assert_child_published(rig: &ForkRig, source_before: &agent_manager::registry::ProjectEntry) {
    let registry = Registry::load(&rig.fx.reg_path).unwrap();
    let source = registry
        .projects
        .iter()
        .find(|entry| entry.id == "bot")
        .unwrap();
    assert_eq!(source.conversation_id, source_before.conversation_id);
    assert_eq!(source.enabled, source_before.enabled);
    assert!(source.forked_from.is_none());
    let child_paths = ProjectPaths::for_session(&rig.fx.proj, "bot-fork");
    let config: Config = state::read_json(&child_paths.config()).unwrap();
    assert_eq!(config.autonomy, Tier::Standard);
    assert_eq!(
        std::fs::read_to_string(child_paths.brief()).unwrap(),
        "fork acceptance goal"
    );
    assert_eq!(
        std::fs::read_to_string(child_paths.directive()).unwrap(),
        "do not publish"
    );
    assert!(
        rig.fx.agent.is_alive(&rig.child_session()).unwrap_or(false),
        "the published child must be running in its own terminal"
    );
    let args = rig.launch_log();
    match rig.engine {
        Engine::Claude => {
            assert!(args.contains(&format!("--resume {SOURCE_ID}")), "{args}");
            assert!(args.contains("--fork-session"), "{args}");
        }
        Engine::Codex => assert!(args.contains(&format!("fork {SOURCE_ID}")), "{args}"),
    }
}

fn source_entry(rig: &ForkRig) -> agent_manager::registry::ProjectEntry {
    Registry::load(&rig.fx.reg_path)
        .unwrap()
        .projects
        .into_iter()
        .find(|entry| entry.id == "bot")
        .unwrap()
}

#[test]
#[ignore]
fn native_conversation_fork_creates_a_standard_child_for_both_engines() {
    if !tmux_available() {
        eprintln!("skipping conversation fork test: tmux not available");
        return;
    }
    for (tag, engine) in [("forkcl", Engine::Claude), ("forkcx", Engine::Codex)] {
        let rig = rig(tag, engine, true, false, SourceStub::Idle);
        let before = source_entry(&rig);
        rig.fork_and_wait_for_child();
        assert_child_published(&rig, &before);
        assert!(
            !rig.fx
                .agent
                .is_alive(&rig.source_session())
                .unwrap_or(false),
            "forking a source with no terminal must not start one"
        );
    }
}

#[test]
#[ignore]
fn native_fork_of_a_live_idle_source_leaves_it_running_for_both_engines() {
    if !tmux_available() {
        eprintln!("skipping live-source fork test: tmux not available");
        return;
    }
    // Claude carries its saved id. Codex was started by pmtui as a Standard session, so it has
    // never saved one and the fork must identify it from the live process.
    for (tag, engine, saved) in [
        ("forklcl", Engine::Claude, true),
        ("forklcx", Engine::Codex, false),
    ] {
        let rig = rig(tag, engine, saved, true, SourceStub::Idle);
        rig.start_source("earlier answer");
        let before = source_entry(&rig);
        let source_pid = probe_pane_pid(&rig.fx.agent_socket, &rig.source_session());
        assert!(source_pid.is_some(), "{engine:?} source has no pane");

        rig.fork_and_wait_for_child();

        assert_child_published(&rig, &before);
        assert_eq!(
            probe_pane_pid(&rig.fx.agent_socket, &rig.source_session()),
            source_pid,
            "{engine:?}: the fork must leave the live source process untouched"
        );
        assert!(
            list_clients(&rig.fx.agent_socket, &rig.source_session())
                .trim()
                .is_empty(),
            "{engine:?}: the fork must not attach to its source"
        );
    }
}

#[test]
#[ignore]
fn native_fork_refuses_a_live_source_that_is_still_streaming() {
    // The stub streams before `f`, so the dashboard's own idle gate refuses it. The under-lock
    // recheck for a source that starts streaming after that gate is unit-tested in pmtui.
    if !tmux_available() {
        eprintln!("skipping busy-source fork test: tmux not available");
        return;
    }
    let rig = rig(
        "forkbusy",
        Engine::Claude,
        true,
        true,
        SourceStub::Streaming,
    );
    rig.start_source("streamed chunk");

    assert!(send_literal(&rig.fx.host_socket, &rig.fx.host_session, "f"));

    assert!(
        wait_for_pane_text_within(
            &rig.fx.host,
            &rig.fx.host_session,
            "still working",
            Duration::from_secs(10)
        ),
        "dashboard={:?}",
        rig.fx.host.capture_tail(&rig.fx.host_session, 80)
    );
    std::thread::sleep(Duration::from_secs(2));
    let registry = Registry::load(&rig.fx.reg_path).unwrap();
    assert!(
        !registry
            .projects
            .iter()
            .any(|entry| entry.id.starts_with("bot-fork")),
        "{:?}",
        registry.projects
    );
    assert!(!rig.fx.agent.is_alive(&rig.child_session()).unwrap_or(false));
    assert!(
        rig.fx
            .agent
            .is_alive(&rig.source_session())
            .unwrap_or(false)
    );
    assert!(
        !rig.launch_log().contains("--fork-session"),
        "{}",
        rig.launch_log()
    );
}

fn install_stub(fx: &EnterFixture, engine: Engine, launches: &Path, source: SourceStub) {
    let bin = fx.dir.path().join("bin").join(engine.bin());
    let script = match engine {
        Engine::Claude => {
            let source_body = match source {
                SourceStub::Idle => "printf '\\342\\227\\217 earlier answer\\n> \\n'\nexec cat\n",
                SourceStub::Streaming => {
                    "n=0\n\
                     while :; do\n\
                       n=$((n+1))\n\
                       printf '\\342\\227\\217 streamed chunk %s\\n> \\n' \"$n\"\n\
                       sleep 0.2\n\
                     done\n"
                }
            };
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> '{}'\n\
                 case \" $* \" in\n\
                 *\" --fork-session \"*)\n\
                   settings=''\n\
                   while [ \"$#\" -gt 0 ]; do\n\
                     if [ \"$1\" = '--settings' ]; then settings=$2; shift 2; else shift; fi\n\
                   done\n\
                   hook=$(printf '%s' \"$settings\" | jq -er '.hooks.SessionStart[0].hooks[0].command')\n\
                   sleep 3\n\
                   CLAUDE_CODE_SESSION_ID='{}' sh -c \"$hook\"\n\
                   printf '> \\n'\n\
                   exec cat\n\
                   ;;\n\
                 esac\n\
                 {source_body}",
                launches.display(),
                CHILD_ID,
            )
        }
        Engine::Codex => {
            let rollout_dir = fx.dir.path().join("codex/sessions/2026/09/28");
            std::fs::create_dir_all(&rollout_dir).unwrap();
            let rollout = |name: &str, id: &str| {
                let path = rollout_dir.join(format!("rollout-{name}-{id}.jsonl"));
                let meta = serde_json::json!({
                    "type": "session_meta",
                    "payload": {
                        "id": id,
                        "session_id": id,
                        "cwd": fx.proj,
                        "thread_source": "user"
                    }
                });
                std::fs::write(&path, format!("{meta}\n")).unwrap();
                path
            };
            let source_rollout = rollout("source", SOURCE_ID);
            let child_rollout = rollout("fork", CHILD_ID);
            // The child reads the source rollout before it opens its own, as `codex fork`
            // does, so the identity wait first sees the source id and must keep polling.
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> '{}'\n\
                 case \" $* \" in\n\
                 *\" fork \"*)\n\
                   exec 7< '{source}'\n\
                   sleep 1\n\
                   exec 7<&-\n\
                   sleep 2\n\
                   exec 8< '{child}'\n\
                   printf '\\342\\200\\272 Ask Codex to do anything\\n'\n\
                   while IFS= read -r line; do :; done\n\
                   ;;\n\
                 esac\n\
                 exec 8< '{source}'\n\
                 printf '\\342\\200\\242 earlier answer\\n\\342\\200\\272 Ask Codex to do anything\\n'\n\
                 while IFS= read -r line; do :; done\n",
                launches.display(),
                source = source_rollout.display(),
                child = child_rollout.display(),
            )
        }
    };
    std::fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}
