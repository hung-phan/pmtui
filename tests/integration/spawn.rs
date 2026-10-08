//! Real-tmux acceptance for `pmtui spawn`: an agent inside a real `pm-` terminal asks the
//! running dashboard for a Standard child session. The engine is an inert shell stub; tmux, the
//! managed launch env, request publication, the dashboard broker, its crash recovery and its
//! rendering are the production paths.
//!
//! One `claude` stub plays every session and tells a parent from a child by its argv: Enter
//! resumes the seeded parent interactively with no Message, while the broker launches a child as a
//! JOB — a one-shot `claude -p … --json-schema <file> -- <prompt>` whose prompt's last line is the
//! Message. In job mode the stub plays that shape: it prints a `result` event and exits, which is
//! what the dashboard retires. A job is handed no `PMTUI_SESSION`, so the stub takes the child's id
//! from the one argument that names its state dir, `--json-schema`.
//!
//! Each stub records its env, folder and prompt under `logs/<session>.*`, and each `pmtui spawn` it
//! runs leaves `<session>.<tag>.json` (stdout), `.err` and `.code` (exit status) beside them. The
//! stub reaches the dashboard only through the env its terminal was launched with: it is never told
//! the registry, the socket or its own ids.
//!
//! A job that exited is retired within a frame, which would erase the row mid-assertion, so a job
//! stub [`Stub::job_waits`] for a release file the test touches when it is done looking. That is the
//! whole lifecycle under test: the child runs, the dashboard shows it, it reports, it is gone.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use agent_manager::lease;
use agent_manager::registry::{LaunchState, ProjectEntry, Registry, SpawnOutcome};
use agent_manager::spawn::{self, JobOutcome, ReceiptState, SpawnReceipt};
use agent_manager::state::ProjectPaths;
use agent_manager::tmux::{Driver, session_name};

use crate::keystrokes::{send_key, send_literal};
use crate::pmtui_fixture::{EnterFixture, PmdSibling, enter_fixture_with_env};
use crate::probe::{probe_pane_pid, tmux_available, wait_for_pane_text_within, wait_until};
use crate::seed::seed_standard_loop_session;

const PARENT: &str = "bot";
/// The parent's display name. `from <label>` then differs from the raw id the status line uses
/// (`dashboard spawned <child> from bot`), so a lineage assertion cannot be met by the log.
const PARENT_LABEL: &str = "Lead";
const TITLE: &str = "Fix flaky fork test";
const MESSAGE: &str = "Make the fork acceptance test deterministic";
const REQUEST_ID: &str = "5f0c3a1e-8b2d-4c6f-9a7e-1d2b3c4d5e6f";

/// `git -C <dir> <args…>`, which must succeed.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
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
fn init_repo(root: &std::path::Path) {
    git(root, &["init", "--quiet", "-b", "main"]);
    git(root, &["config", "user.email", "pm@example.invalid"]);
    git(root, &["config", "user.name", "pm tests"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("README.md"), "start\n").expect("write");
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "start"]);
}

/// A stub line that runs the spawn these tests make, recorded under `tag`, plus `extra` flags.
fn spawn_call(tag: &str, extra: &str) -> String {
    format!("spawn {tag} --title '{TITLE}' --message '{MESSAGE}'{extra}")
}

/// What the stub runs besides recording itself. Each field is shell; in it,
/// `spawn <tag> <args…>` runs `pmtui spawn <args…> --json` and records the result. It prefers
/// `$PMTUI_BIN` and falls back to the fixture's own binary, so a JOB child — which is given no
/// managed env on purpose — can still reach the real command and be refused by it.
#[derive(Default)]
struct Stub {
    /// Run once by the parent, which Enter launches interactively with no Message.
    parent_on_start: String,
    /// Run once by a spawned JOB child, before it reports its result.
    job_on_start: String,
    /// Hold the job child open until the test touches `logs/release`. Without it the child exits
    /// at once and the dashboard retires its row, leaving nothing to observe.
    job_waits: bool,
    /// IGNORE SIGTERM and keep waiting, so the cancel's KILL is what ends this child. A job that dies
    /// from the polite signal never exercises the fallback.
    job_ignores_term: bool,
    /// The outcome the job child reports (`done` when empty).
    job_outcome: String,
    /// Run for each line typed into the stub's pane, with the line in `$line`.
    on_line: String,
}

impl Stub {
    fn outcome(&self) -> &str {
        if self.job_outcome.trim().is_empty() {
            "done"
        } else {
            &self.job_outcome
        }
    }
}

/// What a job stub reports as its summary, and the receipt then carries.
const JOB_SUMMARY: &str = "the stub job did the work";

fn stub_script(logs: &Path, pmtui: &Path, stub: &Stub) -> String {
    let body = |snippet: &str| {
        if snippet.trim().is_empty() {
            ":".to_string()
        } else {
            snippet.to_string()
        }
    };
    // claude's own final event, the one `spawn::read_job_result` reads the outcome from.
    let report = format!(
        r#"{{"type":"result","subtype":"success","result":"prose","structured_output":{{"outcome":"{}","summary":"{JOB_SUMMARY}"}}}}"#,
        stub.outcome()
    );
    // `sleep … & wait $!` rather than a bare `sleep`: a POSIX shell runs a trap only when the
    // foreground command returns, and `wait` is the one that a trapped signal interrupts at once. With
    // a bare sleep the pane is gone before the handler runs, so SIGTERM looks undelivered.
    let hold = if stub.job_waits {
        "  while [ ! -f \"$logs/release\" ]; do sleep 0.1 & wait $!; done"
    } else {
        "  :"
    };
    // A handler that does nothing is not the same as `trap '' TERM`: both survive the signal, and this
    // one keeps the shell's own default-ignore semantics out of it, so the loop below simply continues.
    let trap = if stub.job_ignores_term {
        "  trap : TERM"
    } else {
        "  :"
    };
    format!(
        r#"#!/bin/sh
logs='{logs}'
pmtui='{pmtui}'
sid="${{PMTUI_SESSION:-unmanaged}}"
msg=''
take=0
schema=''
want_schema=0
job=0
for arg in "$@"; do
  if [ "$take" = 1 ]; then msg=$arg; fi
  if [ "$want_schema" = 1 ]; then schema=$arg; want_schema=0; fi
  take=0
  case $arg in
    --) take=1 ;;
    --json-schema|--output-schema) want_schema=1; job=1 ;;
  esac
done
# A job is given no PMTUI_SESSION, so it names itself from its own terminal: `$TMUX` is inherited, so
# `#S` is its `pm-<id>-<hash>` session. (Not from the schema argument: claude takes the schema DOCUMENT
# inline, so only codex's `--output-schema` is a path at all.)
if [ "$job" = 1 ]; then
  name=$(tmux display-message -p '#S' 2>/dev/null)
  case $name in
    pm-*) seg=${{name#pm-}}; sid=${{seg%-*}} ;;
    *) if [ -n "$schema" ]; then
         seg=$(basename "$(dirname "$(dirname "$schema")")")
         sid=${{seg%-*}}
       fi ;;
  esac
fi
# The job prompt ends with a newline, so no `\n` is added here: with one, `tail` would read the
# empty line after it as the last.
last=$(printf '%s' "$msg" | tail -n 1)
{{
  printf 'PMTUI_SESSION=%s\n' "${{PMTUI_SESSION-<unset>}}"
  printf 'PMTUI_STATE_DIR=%s\n' "${{PMTUI_STATE_DIR-<unset>}}"
  printf 'PMTUI_BIN=%s\n' "${{PMTUI_BIN-<unset>}}"
  printf 'PWD=%s\n' "$(pwd -P)"
}} > "$logs/$sid.env.tmp"
mv "$logs/$sid.env.tmp" "$logs/$sid.env"
printf '%s\n' "$last" >> "$logs/$sid.launches"
printf '%s' "$msg" > "$logs/$sid.prompt"
printf 'stub %s message: %s\n' "$sid" "$last"
spawn() {{
  tag=$1
  shift
  "${{PMTUI_BIN:-$pmtui}}" spawn "$@" --json > "$logs/$sid.$tag.json" 2> "$logs/$sid.$tag.err"
  printf '%s\n' "$?" > "$logs/$sid.$tag.code.tmp"
  mv "$logs/$sid.$tag.code.tmp" "$logs/$sid.$tag.code"
}}
# A JOB: do the work, report the result the harness carries, exit. No prompt, no input loop.
if [ "$job" = 1 ]; then
{trap}
{job}
{hold}
  printf '%s\n' '{report}'
  exit 0
fi
if [ -z "$msg" ]; then
{parent}
fi
printf '> \n'
while IFS= read -r line; do
{on_line}
done
"#,
        logs = logs.display(),
        pmtui = pmtui.display(),
        parent = body(&stub.parent_on_start),
        job = body(&stub.job_on_start),
        on_line = body(&stub.on_line),
    )
}

/// One `pmtui spawn` a stub ran: its exit status, its `--json` document and its stderr.
#[derive(Debug)]
struct SpawnRun {
    code: i32,
    json: serde_json::Value,
    stderr: String,
}

impl SpawnRun {
    fn state(&self) -> &str {
        self.json["state"].as_str().unwrap_or("")
    }

    fn error_code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }

    fn child_id(&self) -> Option<&str> {
        self.json["receipt"]["session"]["id"].as_str()
    }
}

/// One real dashboard with a paused Standard parent row whose engine is the stub. `fx` is
/// declared first so its servers are torn down before any scratch engine home is removed.
struct SpawnRig {
    fx: EnterFixture,
    logs: PathBuf,
    _codex_home: Option<tempfile::TempDir>,
}

impl SpawnRig {
    fn new(tag: &str, stub: &Stub) -> Self {
        Self::build(tag, stub, None)
    }

    /// `codex_home`, when given, is exported as `CODEX_HOME` to the dashboard and every terminal
    /// it starts, so a `codex` the stub runs never reads or writes the user's own.
    fn build(tag: &str, stub: &Stub, codex_home: Option<tempfile::TempDir>) -> Self {
        let fx = {
            let env: Vec<(&str, &Path)> = codex_home
                .iter()
                .map(|home| ("CODEX_HOME", home.path()))
                .collect();
            enter_fixture_with_env(tag, PmdSibling::Missing, 300, 50, &env)
        };
        assert!(fx.up, "pmtui fixture did not start for {tag}");
        let logs = fx.dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let bin = fx.dir.path().join("bin").join("claude");
        let pmtui = fx.dir.path().join("exe").join("pmtui");
        std::fs::write(&bin, stub_script(&logs, &pmtui, stub)).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Paused, so Enter resumes it and the dashboard starts its terminal detached, exactly as
        // a human's Enter does.
        seed_standard_loop_session(&fx.reg_path, &fx.proj, PARENT);
        let mut registry = Registry::load(&fx.reg_path).unwrap();
        let parent = &mut registry.projects[0];
        parent.enabled = false;
        parent.display_name = Some(PARENT_LABEL.into());
        registry.save(&fx.reg_path).unwrap();
        assert!(
            wait_for_pane_text_within(
                &fx.host,
                &fx.host_session,
                PARENT_LABEL,
                Duration::from_secs(10)
            ),
            "the dashboard never listed the parent: {:?}",
            fx.host.capture_tail(&fx.host_session, 80)
        );
        Self {
            fx,
            logs,
            _codex_home: codex_home,
        }
    }

    fn parent_paths(&self) -> ProjectPaths {
        ProjectPaths::for_session(&self.fx.proj, PARENT)
    }

    fn parent_requests(&self) -> PathBuf {
        spawn::requests_dir(&self.parent_paths().state_dir())
    }

    fn parent_session(&self) -> String {
        session_name(PARENT, &self.fx.proj)
    }

    /// Press Enter on the paused parent and wait until its stub has started.
    fn start_parent(&self) {
        assert!(send_key(
            &self.fx.host_socket,
            &self.fx.host_session,
            "Enter"
        ));
        let env = self.logs.join(format!("{PARENT}.env"));
        assert!(
            wait_until(Duration::from_secs(15), || env.exists()),
            "the parent stub never started\n{}",
            self.diagnose()
        );
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.logs.join(name)).unwrap_or_default()
    }

    /// `key` as the stub `sid` saw it in its environment when it started.
    fn env_var(&self, sid: &str, key: &str) -> Option<String> {
        self.log(&format!("{sid}.env")).lines().find_map(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::to_string)
        })
    }

    /// The Message each launch of stub `sid` received, one entry per launch. For a job that is the
    /// LAST line of its prompt, which is where the job prompt puts the Message.
    fn launches(&self, sid: &str) -> Vec<String> {
        self.log(&format!("{sid}.launches"))
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The whole prompt stub `sid` was launched with, preamble included.
    fn prompt(&self, sid: &str) -> String {
        self.log(&format!("{sid}.prompt"))
    }

    /// The id of the one child row the parent's agent created, once the broker has staged it.
    ///
    /// Read from the REGISTRY, not from the command's output: `--wait` now waits for the child's
    /// RESULT, so a test that deliberately holds its job open gets `in_progress` back and the id has to
    /// come from somewhere that does not depend on the command's timing.
    fn wait_child_id(&self, bound: Duration) -> String {
        let start = Instant::now();
        while start.elapsed() < bound {
            if let Some(child) = self.children().first() {
                return child.id.clone();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no child row was ever staged\n{}", self.diagnose());
    }

    /// Every line the dashboard has logged, timestamps included.
    fn status_log(&self) -> String {
        std::fs::read_to_string(self.fx.reg_path.parent().unwrap().join("pmtui.log"))
            .unwrap_or_default()
    }

    /// Let every waiting job stub report its result and exit.
    fn release_jobs(&self) {
        std::fs::write(self.logs.join("release"), "go").unwrap();
    }

    /// Wait until the dashboard has retired `id`: no row, no terminal.
    fn wait_retired(&self, id: &str, bound: Duration) -> bool {
        let session = session_name(id, &self.fx.proj);
        wait_until(bound, || {
            self.row(id).is_none() && !self.terminals().contains(&session)
        })
    }

    /// Wait for the `spawn <tag>` that stub `sid` ran to finish, and read what it printed.
    fn wait_spawn(&self, sid: &str, tag: &str, bound: Duration) -> SpawnRun {
        let code = self.logs.join(format!("{sid}.{tag}.code"));
        assert!(
            wait_until(bound, || code.exists()),
            "{sid}'s `spawn {tag}` never finished\n{}",
            self.diagnose()
        );
        let stdout = self.log(&format!("{sid}.{tag}.json"));
        let json = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!(
                "{sid}'s `spawn {tag}` printed no JSON ({error}): {stdout:?}\n{}",
                self.diagnose()
            )
        });
        SpawnRun {
            code: self
                .log(&format!("{sid}.{tag}.code"))
                .trim()
                .parse()
                .unwrap(),
            json,
            stderr: self.log(&format!("{sid}.{tag}.err")),
        }
    }

    /// Every `pm-` terminal on the agent server, sorted.
    fn terminals(&self) -> Vec<String> {
        let mut sessions: Vec<String> = self
            .fx
            .agent
            .list_sessions()
            .into_iter()
            .filter(|name| name.starts_with("pm-"))
            .collect();
        sessions.sort();
        sessions
    }

    fn alive(&self, session: &str) -> bool {
        self.fx.agent.is_alive(session).unwrap_or(false)
    }

    fn rows(&self) -> Vec<ProjectEntry> {
        Registry::load(&self.fx.reg_path)
            .map(|registry| registry.projects)
            .unwrap_or_default()
    }

    /// The registry rows the parent's agent spawned.
    fn children(&self) -> Vec<ProjectEntry> {
        self.rows()
            .into_iter()
            .filter(|row| row.spawned_by.as_deref() == Some(PARENT))
            .collect()
    }

    fn row(&self, id: &str) -> Option<ProjectEntry> {
        self.rows().into_iter().find(|row| row.id == id)
    }

    fn receipt(&self, id: &str) -> Option<SpawnReceipt> {
        spawn::read_receipt(&self.parent_requests(), id)
            .ok()
            .flatten()
    }

    /// Poll the parent's receipt for `id` every few milliseconds, so a short-lived state is seen
    /// the moment the dashboard writes it.
    fn wait_receipt(
        &self,
        id: &str,
        bound: Duration,
        wanted: impl Fn(&SpawnReceipt) -> bool,
    ) -> Option<SpawnReceipt> {
        let start = Instant::now();
        while start.elapsed() < bound {
            if let Some(receipt) = self.receipt(id).filter(&wanted) {
                return Some(receipt);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    fn dashboard(&self) -> String {
        self.fx
            .host
            .capture_tail(&self.fx.host_session, 200)
            .unwrap_or_default()
    }

    /// Everything a failed assertion needs: rows, terminals, spawn files, stub logs and the screen.
    fn diagnose(&self) -> String {
        let files = |dir: &Path| {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
                .unwrap_or_default();
            paths.sort();
            paths
                .iter()
                .map(|path| {
                    format!(
                        "--- {}\n{}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        std::fs::read_to_string(path).unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "rows={:?}\nterminals={:?}\nparent requests:\n{}\nstub logs:\n{}\ndashboard:\n{}",
            self.rows(),
            self.terminals(),
            files(&self.parent_requests()),
            files(&self.logs),
            self.dashboard()
        )
    }
}

/// A `pmtui spawn` that returned while its job is STILL WORKING: exit 3, and a state that says so.
///
/// Either word is correct depending on how far the broker had got when the deadline passed — `queued`
/// when no receipt existed yet, `in_progress` when one did — and both are exit 3, which is the thing a
/// parent branches on. Asserting the pair rather than one word is what keeps this from being a race.
fn assert_still_working(run: &SpawnRun, rig: &SpawnRig) {
    assert_eq!(
        run.code,
        3,
        "a job still working reports exit 3: {run:?}\n{}",
        rig.diagnose()
    );
    assert!(
        matches!(run.state(), "in_progress" | "queued"),
        "unexpected state for a job still working: {run:?}\n{}",
        rig.diagnose()
    );
}

/// Type one line into a stub's pane, as a human at its terminal would.
fn type_line(fx: &EnterFixture, session: &str, line: &str) -> bool {
    send_literal(&fx.agent_socket, session, line) && send_key(&fx.agent_socket, session, "Enter")
}

/// The PREVIEW half of a captured wide-layout pane: everything right of the SESSIONS column.
fn preview_column(pane: &str) -> String {
    pane.lines()
        .filter_map(|line| line.split_once("││").map(|(_, right)| right))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a captured Task board shows a card whose title line is `title` with `lineage` on its
/// context line, a few rows below (title, metadata, state, next, then context).
fn card_shows(pane: &str, title: &str, lineage: &str) -> bool {
    let lines: Vec<&str> = pane.lines().collect();
    lines.iter().enumerate().any(|(at, line)| {
        line.contains(title)
            && lines
                .iter()
                .skip(at + 1)
                .take(8)
                .any(|below| below.contains(lineage))
    })
}

/// Select the Session row labelled `label`. The PREVIEW's title names the selected row, so walk
/// the list from the top until it names `label`.
fn select_session_row(fx: &EnterFixture, label: &str) -> bool {
    let (socket, session) = (fx.host_socket.as_str(), fx.host_session.as_str());
    let title = format!("┌ {label} ");
    let selected = || {
        fx.host
            .capture_tail(session, 200)
            .is_ok_and(|pane| preview_column(&pane).contains(&title) || pane.contains(&title))
    };
    for _ in 0..4 {
        send_key(socket, session, "Up");
    }
    for _ in 0..4 {
        if wait_until(Duration::from_secs(2), selected) {
            return true;
        }
        send_key(socket, session, "Down");
    }
    false
}

#[test]
#[ignore]
fn spawn_from_a_live_session_runs_one_job_the_dashboard_shows_then_retires() {
    if !tmux_available() {
        eprintln!("skipping spawn test: tmux not available");
        return;
    }
    let rig = SpawnRig::new(
        "spwlive",
        &Stub {
            parent_on_start: spawn_call("spawn", ""),
            job_waits: true,
            ..Stub::default()
        },
    );
    rig.start_parent();

    // The dashboard launched the parent's terminal with the managed env, and the stub used only
    // that env to reach it.
    let pmtui = std::fs::canonicalize(rig.fx.dir.path().join("exe").join("pmtui")).unwrap();
    assert_eq!(
        rig.env_var(PARENT, "PMTUI_SESSION").as_deref(),
        Some(PARENT)
    );
    assert_eq!(
        rig.env_var(PARENT, "PMTUI_STATE_DIR"),
        Some(rig.parent_paths().state_dir().display().to_string())
    );
    assert_eq!(
        rig.env_var(PARENT, "PMTUI_BIN"),
        Some(pmtui.display().to_string())
    );

    // `--wait` waits for the child's RESULT, and this job is held open on purpose, so the command
    // reports that it is still working rather than calling the launch an answer.
    let run = rig.wait_spawn(PARENT, "spawn", Duration::from_secs(60));
    assert_still_working(&run, &rig);
    let child = rig.wait_child_id(Duration::from_secs(20));
    let child_session = session_name(&child, &rig.fx.proj);
    let receipt = &run.json["receipt"]["session"];
    assert_eq!(receipt["tmux_session"], child_session.as_str());
    assert_eq!(receipt["spawned_by"], PARENT);
    assert_eq!(receipt["title"], TITLE);
    assert!(
        !run.json.to_string().contains(MESSAGE),
        "the command must never echo the Message: {}",
        run.json
    );

    // Exactly one new terminal, whose agent received the Message once, in the parent's folder.
    let mut expected = vec![rig.parent_session(), child_session.clone()];
    expected.sort();
    assert_eq!(rig.terminals(), expected, "{}", rig.diagnose());
    assert!(
        wait_for_pane_text_within(
            &rig.fx.agent,
            &child_session,
            &format!("stub {child} message: {MESSAGE}"),
            Duration::from_secs(10)
        ),
        "the child's pane never showed its Message\n{}",
        rig.diagnose()
    );
    assert_eq!(rig.launches(&child), vec![MESSAGE.to_string()]);
    // The Message is the TAIL of the job prompt, under a preamble that tells the child nobody is
    // watching. The prompt is what the child was launched with, not something typed in later.
    let prompt = rig.prompt(&child);
    assert!(
        prompt.trim_end().ends_with(MESSAGE) && prompt.contains("Nobody is at this terminal"),
        "the child's prompt is not the job prompt: {prompt:?}\n{}",
        rig.diagnose()
    );
    assert_eq!(
        rig.env_var(&child, "PWD"),
        Some(rig.fx.proj.display().to_string())
    );
    // A JOB IS GIVEN NO MANAGED ENV, which is what makes nested spawning impossible rather than
    // merely refused (`a_child_cannot_spawn` runs the command anyway and is turned away by it).
    assert_eq!(
        rig.env_var(&child, "PMTUI_SESSION").as_deref(),
        Some("<unset>"),
        "{}",
        rig.diagnose()
    );
    assert_eq!(rig.env_var(&child, "PMTUI_BIN").as_deref(), Some("<unset>"));
    let rows = rig.children();
    assert_eq!(rows.len(), 1, "{}", rig.diagnose());
    assert_eq!(rows[0].id, child);
    assert!(rows[0].enabled);
    assert_eq!(rows[0].task_title.as_deref(), Some(TITLE));
    assert_eq!(
        rows[0].launch.as_ref().and_then(|launch| launch.outcome),
        Some(SpawnOutcome::Ready)
    );
    assert!(
        wait_for_pane_text_within(
            &rig.fx.host,
            &rig.fx.host_session,
            &format!("dashboard spawned {child} from {PARENT}"),
            Duration::from_secs(10)
        ),
        "no status line reported the child\n{}",
        rig.diagnose()
    );

    // Session view, without a restart: the child's row, the lineage fact in its preview head, and
    // its live terminal showing the Message. The Session row itself has no title or lineage slot
    // by design; the title lives on the Task card.
    let lineage = format!("from {PARENT_LABEL}");
    assert!(
        select_session_row(&rig.fx, &child),
        "could not select {child} in the Session view\n{}",
        rig.diagnose()
    );
    assert!(
        wait_until(Duration::from_secs(10), || {
            let preview = preview_column(&rig.dashboard());
            preview.contains(&lineage) && preview.contains(MESSAGE)
        }),
        "the Session preview never showed {lineage:?} and the Message\n{}",
        rig.diagnose()
    );

    // Task view: the child's card carries its title and the same lineage.
    assert!(send_key(&rig.fx.host_socket, &rig.fx.host_session, "2"));
    assert!(
        wait_until(Duration::from_secs(10), || card_shows(
            &rig.dashboard(),
            TITLE,
            &lineage
        )),
        "the Task board never showed the child's card\n{}",
        rig.diagnose()
    );

    // THE JOB ENDS ITSELF. Released, the child reports `done` and exits; the dashboard records that
    // in the parent's receipt and retires the row and its terminal without anyone pressing a key.
    rig.release_jobs();
    let finished = rig
        .wait_receipt(
            run.json["request_id"].as_str().expect("a request id"),
            Duration::from_secs(30),
            |receipt| receipt.state.is_job_terminal(),
        )
        .unwrap_or_else(|| panic!("the job's result was never recorded\n{}", rig.diagnose()));
    assert_eq!(finished.state, ReceiptState::Done, "{finished:?}");
    assert!(finished.error.is_none(), "{finished:?}");
    let result = finished.result.expect("a done receipt carries the result");
    assert_eq!(result.outcome, JobOutcome::Done);
    assert_eq!(result.summary, JOB_SUMMARY);
    assert!(
        rig.wait_retired(&child, Duration::from_secs(20)),
        "the finished job was never retired\n{}",
        rig.diagnose()
    );
    assert_eq!(
        rig.terminals(),
        vec![rig.parent_session()],
        "retiring a job must leave the parent's terminal running\n{}",
        rig.diagnose()
    );
    // AND IT LEAVES NOTHING ON DISK. Its state went with its row; the parent's receipt above is the
    // part that outlives it, and the PARENT's own state is untouched.
    assert!(
        !ProjectPaths::for_session(&rig.fx.proj, &child)
            .state_dir()
            .exists(),
        "the retired job left its state behind\n{}",
        rig.diagnose()
    );
    assert!(
        rig.parent_paths().state_dir().is_dir(),
        "the parent's own state must survive its child's retirement"
    );
    assert!(
        wait_for_pane_text_within(
            &rig.fx.host,
            &rig.fx.host_session,
            &format!("retired {child}"),
            Duration::from_secs(10)
        ),
        "no status line reported the retirement\n{}",
        rig.diagnose()
    );
}

#[test]
#[ignore]
fn spawn_without_a_dashboard_is_queued_then_created_when_one_starts() {
    if !tmux_available() {
        eprintln!("skipping spawn test: tmux not available");
        return;
    }
    let on_line = format!(
        "case $line in\n  queue) {} ;;\n  again) {} ;;\nesac",
        spawn_call("queue", &format!(" --request-id {REQUEST_ID} --wait 1")),
        spawn_call("again", &format!(" --request-id {REQUEST_ID} --wait 30")),
    );
    let mut rig = SpawnRig::new(
        "spwqueue",
        &Stub {
            on_line,
            job_waits: true,
            ..Stub::default()
        },
    );
    rig.start_parent();
    let parent_session = rig.parent_session();
    assert!(
        rig.fx.quit_dashboard(),
        "the dashboard did not quit\n{}",
        rig.diagnose()
    );
    assert!(
        rig.alive(&parent_session),
        "quitting the dashboard must leave the parent's terminal running"
    );

    // No dashboard: the request is durable and the command reports it queued.
    assert!(type_line(&rig.fx, &parent_session, "queue"));
    let queued = rig.wait_spawn(PARENT, "queue", Duration::from_secs(20));
    assert_eq!(
        (queued.code, queued.state()),
        (3, "queued"),
        "{queued:?}\n{}",
        rig.diagnose()
    );
    assert_eq!(queued.json["request_id"], REQUEST_ID);
    let dir = rig.parent_requests();
    assert!(spawn::request_path(&dir, REQUEST_ID).is_file());
    assert!(
        !spawn::receipt_path(&dir, REQUEST_ID).exists(),
        "no dashboard ran, so nothing may have answered the request"
    );
    assert!(rig.children().is_empty(), "{}", rig.diagnose());
    assert_eq!(rig.terminals(), vec![parent_session.clone()]);

    // A dashboard that starts later creates the child from the waiting request.
    assert!(
        rig.fx.relaunch_dashboard(),
        "the second dashboard did not start"
    );
    let claimed = format!("pid:{}", rig.fx.dashboard_pid().expect("dashboard pid"));
    let done = rig
        .wait_receipt(REQUEST_ID, Duration::from_secs(30), |receipt| {
            receipt.state.is_final()
        })
        .unwrap_or_else(|| panic!("the request was never answered\n{}", rig.diagnose()));
    assert_eq!(done.state, ReceiptState::Ready, "{}", rig.diagnose());
    assert_eq!(done.claimed_by.as_deref(), Some(claimed.as_str()));
    let child = done.session.expect("a ready receipt names its child").id;
    let child_session = session_name(&child, &rig.fx.proj);
    assert!(
        wait_until(Duration::from_secs(10), || rig.launches(&child).len() == 1),
        "{}",
        rig.diagnose()
    );

    // Rerunning with the same request id reports the same child and creates nothing.
    assert!(type_line(&rig.fx, &parent_session, "again"));
    let again = rig.wait_spawn(PARENT, "again", Duration::from_secs(60));
    assert_still_working(&again, &rig);
    assert_eq!(
        again.child_id(),
        Some(child.as_str()),
        "the rerun names the child it already has"
    );
    let mut expected = vec![parent_session, child_session];
    expected.sort();
    assert_eq!(rig.terminals(), expected, "{}", rig.diagnose());
    assert_eq!(rig.children().len(), 1, "{}", rig.diagnose());
    assert_eq!(rig.launches(&child), vec![MESSAGE.to_string()]);

    // Released, the job finishes and the dashboard that answered the queued request retires it.
    rig.release_jobs();
    assert!(
        rig.wait_retired(&child, Duration::from_secs(30)),
        "the finished job was never retired\n{}",
        rig.diagnose()
    );
    assert_eq!(
        rig.receipt(REQUEST_ID).map(|receipt| receipt.state),
        Some(ReceiptState::Done),
        "{}",
        rig.diagnose()
    );
}

/// The installed codex's version when it has the `sandbox` subcommand this test drives. `Err`,
/// the only skip, when codex or that subcommand is missing. An installed sandbox this test cannot
/// drive, or one that then fails to run the command, fails the test instead.
///
/// `codex sandbox [OPTIONS] [COMMAND]...` is "Run commands within a Codex-provided sandbox"
/// (0.158 help): it executes the given argv under the Linux sandbox and starts no model session.
/// Its built-in `:workspace` permission profile is Codex's workspace-write shape: the working
/// directory and `/tmp` are writable, and the rest of the filesystem, `$HOME` included, is
/// read-only.
fn codex_sandbox(codex_home: &Path) -> Result<String, String> {
    let codex = |args: &[&str]| {
        Command::new("codex")
            .args(args)
            .env("CODEX_HOME", codex_home)
            .output()
    };
    let version = match codex(&["--version"]) {
        Err(error) => return Err(format!("codex is not installed ({error})")),
        Ok(out) => {
            assert!(
                out.status.success(),
                "`codex --version` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    };
    let help = codex(&["sandbox", "--help"]).expect("codex ran a moment ago");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    // Without the subcommand, clap reads `sandbox` as a prompt and `--help` prints the top-level
    // usage instead, so the usage line is what proves the subcommand exists.
    if !help.status.success() || !text.contains("Usage: codex sandbox") {
        return Err(format!("codex {version} has no `sandbox` subcommand"));
    }
    assert!(
        text.contains("--permission-profile"),
        "codex {version} has `codex sandbox` but no `--permission-profile`; update this test's \
         invocation:\n{text}"
    );
    Ok(version)
}

#[test]
#[ignore]
fn spawn_under_codex_workspace_write_sandbox_publishes_its_request() {
    if !tmux_available() {
        eprintln!("skipping spawn sandbox test: tmux not available");
        return;
    }
    // A scratch CODEX_HOME, so the test neither reads the user's codex config (which could change
    // the sandbox) nor writes into it.
    let codex_home = tempfile::tempdir().unwrap();
    let version = match codex_sandbox(codex_home.path()) {
        Ok(version) => version,
        Err(reason) => {
            eprintln!("skipping spawn sandbox test: {reason}");
            return;
        }
    };
    // Before the spawn, the sandboxed shell proves `$HOME` is read-only. On a broken sandbox the
    // probe file is created, so it is removed both inside and after the run.
    let canary = format!(".am-spawn-sandbox-canary-{}", std::process::id());
    let parent_on_start = format!(
        r#"codex sandbox -P :workspace -C "$PWD" -- sh -c 'if ( : > "$HOME/{canary}" ) 2>/dev/null; then rm -f "$HOME/{canary}"; echo home=writable >&2; else echo home=readonly >&2; fi; exec "$PMTUI_BIN" spawn --title "{TITLE}" --message "{MESSAGE}" --json' > "$logs/$sid.sandbox.json" 2> "$logs/$sid.sandbox.err"
printf '%s\n' "$?" > "$logs/$sid.sandbox.code.tmp"
mv "$logs/$sid.sandbox.code.tmp" "$logs/$sid.sandbox.code""#
    );
    let rig = SpawnRig::build(
        "spwsbx",
        &Stub {
            parent_on_start,
            job_waits: true,
            ..Stub::default()
        },
        Some(codex_home),
    );
    rig.start_parent();
    let run = rig.wait_spawn(PARENT, "sandbox", Duration::from_secs(60));
    let canary_path = std::env::home_dir().unwrap_or_default().join(&canary);
    let canary_left = canary_path.exists();
    let _ = std::fs::remove_file(&canary_path);
    assert!(
        run.stderr.contains("home=readonly") && !canary_left,
        "codex {version}'s workspace-write sandbox left $HOME writable, so it proves nothing:\n{}\n{}",
        run.stderr,
        rig.diagnose()
    );

    // The request was published from inside the sandbox, into the parent's own state dir.
    let request_id = run.json["request_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no request id: {run:?}\n{}", rig.diagnose()))
        .to_string();
    let request_path = spawn::request_path(&rig.parent_requests(), &request_id);
    let request: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&request_path).unwrap_or_else(|error| {
            panic!(
                "no request at {} ({error}): {run:?}\n{}",
                request_path.display(),
                rig.diagnose()
            )
        }),
    )
    .unwrap();
    assert_eq!(request["parent_session"], PARENT);
    assert_eq!(request["args"]["title"], TITLE);

    // And the dashboard, outside the sandbox, acted on it: the child is running, which is all the
    // command can say while its job is held open.
    assert_still_working(&run, &rig);
    let child = rig.wait_child_id(Duration::from_secs(20));
    assert_eq!(rig.children().len(), 1, "{}", rig.diagnose());
    assert!(rig.alive(&session_name(&child, &rig.fx.proj)));
    assert_eq!(rig.launches(&child), vec![MESSAGE.to_string()]);
    rig.release_jobs();
    assert!(
        rig.wait_retired(&child, Duration::from_secs(30)),
        "the finished job was never retired\n{}",
        rig.diagnose()
    );
}

/// FIVE AT ONCE, over real terminals. Every acceptance until now dispatched ONE child, and a user who
/// asked for five got four results and one `ended_without_result` ("the job's terminal is gone and it
/// left no result"). Five concurrent requests is its own case: five ids allocated in one frame, five
/// `new-session` calls on one tmux server, five state dirs, and five retirements racing each other's
/// purges. Each child must get its own id, its own terminal, and its own answer.
#[test]
#[ignore]
fn five_children_dispatched_at_once_each_get_their_own_answer() {
    if !tmux_available() {
        eprintln!("skipping spawn fan-out test: tmux not available");
        return;
    }
    // All five dispatched in the background from ONE shell, then waited on — the shape an agent uses
    // when it fans out, and the shape that broke.
    let fanout = (0..5)
        .map(|n| {
            format!("spawn kid{n} --title '{TITLE} {n}' --message '{MESSAGE} {n}' --wait 60 &",)
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\nwait";
    let rig = SpawnRig::new(
        "spwfanout",
        &Stub {
            parent_on_start: fanout,
            ..Stub::default()
        },
    );
    rig.start_parent();

    // Every one of the five answers, and each names a DIFFERENT child. A shared id or a shared state
    // directory shows up here as a duplicate or a missing result rather than as a mystery later.
    let mut ids: Vec<String> = Vec::new();
    for n in 0..5 {
        let run = rig.wait_spawn(PARENT, &format!("kid{n}"), Duration::from_secs(120));
        assert_eq!(
            run.state(),
            "done",
            "child {n} did not report\n{run:?}\n{}",
            rig.diagnose()
        );
        assert_eq!(
            run.json["receipt"]["result"]["summary"].as_str(),
            Some(JOB_SUMMARY),
            "child {n} reported no summary\n{run:?}"
        );
        ids.push(
            run.child_id()
                .unwrap_or_else(|| panic!("child {n} has no id\n{run:?}"))
                .to_string(),
        );
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 5, "two children shared an id: {ids:?}");

    // And nothing is left behind: five `done` jobs retire themselves, terminals and rows.
    for id in &ids {
        assert!(
            rig.wait_retired(id, Duration::from_secs(30)),
            "{id} was never retired\n{}",
            rig.diagnose()
        );
    }
    assert!(rig.children().is_empty(), "{}", rig.diagnose());
}

/// FIVE CHILDREN, FIVE CHECKOUTS, ONE REPOSITORY — over real terminals and real git. Each child commits
/// the SAME path in its own worktree, and all five commits survive on their own branches with the human's
/// checkout untouched. A shared working tree would have five children overwriting one file; nothing else
/// in this suite can prove that it does not happen.
#[test]
#[ignore]
fn five_children_commit_the_same_file_in_their_own_worktrees() {
    if !tmux_available() {
        eprintln!("skipping spawn worktree test: tmux not available");
        return;
    }
    let fanout = (0..5)
        .map(|n| {
            format!("spawn kid{n} --title '{TITLE} {n}' --message '{MESSAGE} {n}' --wait 60 &")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\nwait";
    let rig = SpawnRig::new(
        "spwwtree",
        &Stub {
            parent_on_start: fanout,
            // Each child writes the same path and commits it, which is what the branch has to carry.
            job_on_start: concat!(
                "printf '// %s\\n' \"$sid\" > shared.rs\n",
                "git add -A >/dev/null 2>&1\n",
                "git -c user.email=pm@example.invalid -c user.name=pm -c commit.gpgsign=false \\\n",
                "  commit --quiet -m \"work from $sid\" >/dev/null 2>&1\n"
            )
            .to_string(),
            ..Stub::default()
        },
    );
    init_repo(&rig.fx.proj);
    let head_before = git(&rig.fx.proj, &["rev-parse", "HEAD"]);
    rig.start_parent();

    let mut branches = Vec::new();
    for n in 0..5 {
        let run = rig.wait_spawn(PARENT, &format!("kid{n}"), Duration::from_secs(120));
        assert_eq!(
            run.state(),
            "done",
            "child {n}\n{run:?}\n{}",
            rig.diagnose()
        );
        let work = &run.json["receipt"]["work"];
        let branch = work["branch"]
            .as_str()
            .unwrap_or_else(|| panic!("child {n} reported no branch\n{run:?}"))
            .to_string();
        let commit = work["commit"]
            .as_str()
            .unwrap_or_else(|| panic!("child {n} reported no commit\n{run:?}"))
            .to_string();
        assert_eq!(
            work["touched"].as_array().map(|files| files.len()),
            Some(1),
            "child {n} touched exactly its own file\n{run:?}"
        );
        assert_eq!(work["uncommitted"].as_bool(), None, "it committed it all");
        // The commit is reachable from the HUMAN'S checkout, because the worktrees share one `.git`.
        assert_eq!(
            git(&rig.fx.proj, &["rev-parse", &branch]),
            commit,
            "child {n}'s commit is not on its branch\n{}",
            rig.diagnose()
        );
        assert_eq!(
            git(&rig.fx.proj, &["show", &format!("{commit}:shared.rs")]).trim(),
            format!("// {}", run.child_id().unwrap_or("")),
            "each child committed its OWN content, not a sibling's"
        );
        branches.push(branch);
    }

    branches.sort();
    branches.dedup();
    assert_eq!(
        branches.len(),
        5,
        "five branches, one per child: {branches:?}"
    );
    assert_eq!(
        git(&rig.fx.proj, &["rev-parse", "HEAD"]),
        head_before,
        "and the human's checkout never moved"
    );
    assert_eq!(
        git(
            &rig.fx.proj,
            &["status", "--porcelain", "--untracked-files=no"]
        ),
        "",
        "nor was anything staged in it"
    );
}

/// THE PARENT READS ITS ANSWER FROM A LISTING, over real terminals: it dispatches, asks `--status`
/// while the job is held open and is told the child is RUNNING, then asks again once the job has
/// reported and is told `done` with the summary the child itself wrote. No id carried between asks, and
/// no transcript parsed — the whole point of the listing, proven against a real dashboard rather than a
/// receipt a test wrote itself.
#[test]
#[ignore]
fn a_parent_lists_its_children_and_reads_the_answer_without_an_id() {
    if !tmux_available() {
        eprintln!("skipping spawn status test: tmux not available");
        return;
    }
    let rig = SpawnRig::new(
        "spwstatus",
        &Stub {
            // `--wait 1`: the dispatch returns while the job is still held open, which is the case the
            // listing exists for.
            parent_on_start: spawn_call("spawn", " --wait 1"),
            // Two tags, because each `spawn <tag>` overwrites its own log and the second ask must be
            // distinguishable from the first.
            on_line: "case $line in\n  look) spawn look --status ;;\n  again) spawn again --status ;;\nesac"
                .to_string(),
            job_waits: true,
            ..Stub::default()
        },
    );
    rig.start_parent();
    let run = rig.wait_spawn(PARENT, "spawn", Duration::from_secs(60));
    assert_still_working(&run, &rig);
    let child = rig.wait_child_id(Duration::from_secs(20));
    // WAIT FOR THE SIGNAL THE LISTING READS, rather than asking at whatever frame the clock lands on:
    // the row is staged a moment before the receipt says the job is up, and a listing asked in that gap
    // says `staged` — true, and not what this test is about.
    let request_id = run.json["request_id"]
        .as_str()
        .expect("a request id")
        .to_string();
    rig.wait_receipt(&request_id, Duration::from_secs(30), |receipt| {
        receipt.state == ReceiptState::Ready
    })
    .unwrap_or_else(|| panic!("the job never came up\n{}", rig.diagnose()));

    // WHILE IT RUNS: one entry, named, and NOT finished. A listing is not an outcome, so exit 0.
    assert!(type_line(&rig.fx, &rig.parent_session(), "look"));
    let running = rig.wait_spawn(PARENT, "look", Duration::from_secs(30));
    assert_eq!(
        (running.code, running.stderr.as_str()),
        (0, ""),
        "{running:?}"
    );
    let entries = running.json["children"]
        .as_array()
        .unwrap_or_else(|| panic!("--status printed no children array\n{running:?}"));
    assert_eq!(entries.len(), 1, "{running:?}");
    assert_eq!(
        entries[0]["id"].as_str(),
        Some(child.as_str()),
        "{running:?}"
    );
    assert_eq!(entries[0]["title"].as_str(), Some(TITLE), "{running:?}");
    assert_eq!(
        entries[0]["state"].as_str(),
        Some("ready"),
        "a held-open job is still running\n{running:?}"
    );
    assert!(
        entries[0]["summary"].is_null(),
        "nothing to report yet\n{running:?}"
    );

    // THEN THE ANSWER, from the same command: the child reports, the dashboard retires it, and the next
    // ask carries the summary the child wrote — after its own state directory is gone.
    rig.release_jobs();
    assert!(
        rig.wait_retired(&child, Duration::from_secs(30)),
        "the finished job was never retired\n{}",
        rig.diagnose()
    );
    assert!(type_line(&rig.fx, &rig.parent_session(), "again"));
    let answered = rig.wait_spawn(PARENT, "again", Duration::from_secs(30));
    assert_eq!(answered.code, 0, "{answered:?}");
    let entry = &answered.json["children"][0];
    assert_eq!(entry["state"].as_str(), Some("done"), "{answered:?}");
    assert_eq!(
        entry["summary"].as_str(),
        Some(JOB_SUMMARY),
        "the parent reads the child's own summary, not a transcript\n{answered:?}"
    );
    assert_eq!(
        entry["request_id"].as_str(),
        Some(request_id.as_str()),
        "{answered:?}"
    );
}

/// THE PARENT'S CANCEL, over a real terminal: the marker it writes makes the dashboard SIGTERM the
/// job's process group, kill it when that is not enough, answer the parent `cancelled`, and retire the
/// row — while the parent's own terminal keeps running. The child here records the signal and refuses to
/// die from it, so both halves are observable instead of inferred.
#[test]
#[ignore]
fn a_parent_can_cancel_its_job_and_the_dashboard_stops_it() {
    if !tmux_available() {
        eprintln!("skipping spawn cancel test: tmux not available");
        return;
    }
    let on_line = format!(
        "case $line in\n  cancel) spawn cancel --request-id {REQUEST_ID} --cancel ;;\nesac"
    );
    let rig = SpawnRig::new(
        "spwcancel",
        &Stub {
            parent_on_start: spawn_call("spawn", &format!(" --request-id {REQUEST_ID}")),
            on_line,
            job_waits: true,
            // IGNORES the polite signal, so the kill that follows is what actually ends this child —
            // the fallback half of the cancel, exercised rather than assumed.
            job_ignores_term: true,
            ..Stub::default()
        },
    );
    rig.start_parent();
    let run = rig.wait_spawn(PARENT, "spawn", Duration::from_secs(60));
    assert_still_working(&run, &rig);
    let child = rig.wait_child_id(Duration::from_secs(20));
    let child_session = session_name(&child, &rig.fx.proj);
    assert!(rig.alive(&child_session), "{}", rig.diagnose());

    // The parent asks for it to stop. The command records the ask and leaves the stopping to the
    // dashboard, so it reports `cancel_requested` rather than a finished state.
    assert!(type_line(&rig.fx, &rig.parent_session(), "cancel"));
    let cancel = rig.wait_spawn(PARENT, "cancel", Duration::from_secs(30));
    assert_eq!(
        (cancel.code, cancel.state()),
        (3, "cancel_requested"),
        "{cancel:?}\n{}",
        rig.diagnose()
    );

    // POLITE FIRST, read from the DASHBOARD'S OWN LOG. Watching the child for the signal is a race it
    // cannot win reliably: SIGTERM goes to the whole process group, so the step wrapper can die and
    // tmux tear the pane down before the child's trap handler finishes writing. (It did, on CI.) That
    // the signal reaches a real process group is proven by `request_stop_signals_the_panes_own_group…`
    // against a real process; what belongs HERE is that the dashboard asked before it killed.
    assert!(
        wait_until(Duration::from_secs(20), || rig
            .status_log()
            .contains(&format!("cancelling {child}"))),
        "the dashboard never asked {child} to stop before killing it\n{}",
        rig.diagnose()
    );

    // THEN THE KILL, the receipt and the retirement — without touching the parent.
    let done = rig
        .wait_receipt(REQUEST_ID, Duration::from_secs(30), |receipt| {
            receipt.state.is_job_terminal()
        })
        .unwrap_or_else(|| panic!("the cancel was never answered\n{}", rig.diagnose()));
    assert_eq!(done.state, ReceiptState::Cancelled, "{done:?}");
    assert!(
        done.error.expect("a reason").message.contains("cancelled"),
        "{}",
        rig.diagnose()
    );
    assert!(
        rig.wait_retired(&child, Duration::from_secs(20)),
        "the cancelled job was never retired\n{}",
        rig.diagnose()
    );
    assert_eq!(
        rig.terminals(),
        vec![rig.parent_session()],
        "cancelling a job must leave its parent running\n{}",
        rig.diagnose()
    );
}

/// A JOB CANNOT SPAWN, and its refusal comes from the production command. A job child is handed no
/// managed env (proved in `spawn_from_a_live_session_…`), so when it runs the real `pmtui spawn`
/// anyway the command cannot identify a calling session and turns it away before publishing
/// anything. Nothing is created and no request is left anywhere for a dashboard to find.
#[test]
#[ignore]
fn a_child_cannot_spawn() {
    if !tmux_available() {
        eprintln!("skipping spawn test: tmux not available");
        return;
    }
    let rig = SpawnRig::new(
        "spwnest",
        &Stub {
            parent_on_start: spawn_call("spawn", ""),
            job_on_start: "  spawn nested --title 'Grandchild' --message 'Split the fork test'"
                .into(),
            job_waits: true,
            ..Stub::default()
        },
    );
    rig.start_parent();
    let run = rig.wait_spawn(PARENT, "spawn", Duration::from_secs(60));
    assert_still_working(&run, &rig);
    let child = rig.wait_child_id(Duration::from_secs(20));

    let nested = rig.wait_spawn(&child, "nested", Duration::from_secs(40));
    assert_eq!(
        (nested.code, nested.state(), nested.error_code()),
        (1, "failed", "not_in_a_session"),
        "{nested:?}\n{}",
        rig.diagnose()
    );
    // The command refused on its own, before writing anything: no request under the child, and
    // nothing for the dashboard to answer.
    let child_requests =
        spawn::requests_dir(&ProjectPaths::for_session(&rig.fx.proj, &child).state_dir());
    let nested_id = nested.json["request_id"].as_str().unwrap();
    assert!(
        !spawn::request_path(&child_requests, nested_id).exists(),
        "a refused spawn must publish nothing\n{}",
        rig.diagnose()
    );
    assert!(
        spawn::read_receipt(&child_requests, nested_id)
            .ok()
            .flatten()
            .is_none(),
        "nothing answered a request that was never published\n{}",
        rig.diagnose()
    );

    // Nothing was created for the grandchild.
    let rows = rig.rows();
    assert_eq!(rows.len(), 2, "{}", rig.diagnose());
    assert!(
        !rows
            .iter()
            .any(|row| row.spawned_by.as_deref() == Some(child.as_str())),
        "{}",
        rig.diagnose()
    );
    let mut expected = vec![rig.parent_session(), session_name(&child, &rig.fx.proj)];
    expected.sort();
    assert_eq!(rig.terminals(), expected, "{}", rig.diagnose());

    // The refusal did not stop the job: it still reports and is retired.
    rig.release_jobs();
    assert!(
        rig.wait_retired(&child, Duration::from_secs(30)),
        "the finished job was never retired\n{}",
        rig.diagnose()
    );
}

#[test]
#[ignore]
fn killing_the_dashboard_mid_request_recovers_without_a_second_terminal() {
    if !tmux_available() {
        eprintln!("skipping spawn recovery test: tmux not available");
        return;
    }
    // The command waits long enough to outlive both crashes below.
    let mut rig = SpawnRig::new(
        "spwkill",
        &Stub {
            parent_on_start: spawn_call("spawn", &format!(" --request-id {REQUEST_ID} --wait 90")),
            // The child must outlive three dashboards, so it holds until the test releases it.
            job_waits: true,
            ..Stub::default()
        },
    );
    rig.start_parent();

    // Crash 1, while the request is staged. The dashboard advances one phase per frame (500 ms
    // apart here), so the `staged` receipt is caught well before the launch, and holding the
    // child's driver.lock then keeps the broker from launching, as it does for interactive New.
    let staged = rig
        .wait_receipt(REQUEST_ID, Duration::from_secs(20), |receipt| {
            receipt.state == ReceiptState::Staged
        })
        .unwrap_or_else(|| panic!("the request never reached staged\n{}", rig.diagnose()));
    let session = staged.session.expect("a staged receipt names its row");
    let child = session.id.clone();
    let child_session = session.tmux_session.clone();
    let lock = ProjectPaths::for_session(&session.root, &child)
        .daemon_dir()
        .join("driver.lock");
    let hold = lease::try_acquire(&lock)
        .expect("open the child's driver.lock")
        .unwrap_or_else(|| panic!("the broker was already launching {child}: staged was missed"));
    let still_pending = |rig: &SpawnRig| {
        rig.row(&child).is_some_and(|row| {
            !row.enabled
                && row
                    .launch
                    .is_some_and(|launch| launch.state == LaunchState::Pending)
        }) && !rig.alive(&child_session)
    };
    assert!(
        still_pending(&rig),
        "the lock was taken after the launch\n{}",
        rig.diagnose()
    );
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        still_pending(&rig),
        "a held driver.lock must keep the broker from launching\n{}",
        rig.diagnose()
    );
    assert!(
        rig.fx.kill_dashboard(),
        "the first dashboard survived SIGKILL"
    );

    // A second dashboard recovers the staged request, and still waits for the lock.
    assert!(
        rig.fx.relaunch_dashboard(),
        "the second dashboard did not start"
    );
    let second = format!("pid:{}", rig.fx.dashboard_pid().expect("dashboard pid"));
    let recovered = rig
        .wait_receipt(REQUEST_ID, Duration::from_secs(20), |receipt| {
            receipt.claimed_by.as_deref() == Some(second.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "the second dashboard never took the request\n{}",
                rig.diagnose()
            )
        });
    assert_eq!(recovered.state, ReceiptState::Staged, "{}", rig.diagnose());
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        still_pending(&rig),
        "the recovered request launched through a held driver.lock\n{}",
        rig.diagnose()
    );

    // Crash 2, right after the launch and before readiness is proven: readiness needs two live
    // observations on later frames, so a started `launching` receipt stays up for about a second.
    drop(hold);
    let launched = rig.wait_receipt(REQUEST_ID, Duration::from_secs(20), |receipt| {
        receipt.state.is_final()
            || (receipt.state == ReceiptState::Launching
                && receipt.launch_state == Some(LaunchState::Started))
    });
    assert!(
        rig.fx.kill_dashboard(),
        "the second dashboard survived SIGKILL"
    );
    let launched = launched
        .unwrap_or_else(|| panic!("the recovered request never launched\n{}", rig.diagnose()));
    let at_kill = rig.receipt(REQUEST_ID).expect("a receipt");
    assert!(
        !launched.state.is_final() && !at_kill.state.is_final(),
        "readiness finished before the kill, so recovery of a started row went untested: \
         {at_kill:?}\n{}",
        rig.diagnose()
    );
    let row = rig.row(&child).expect("the child row");
    assert!(row.enabled, "{row:?}");
    assert_eq!(
        row.launch
            .as_ref()
            .map(|launch| (launch.state, launch.outcome)),
        Some((LaunchState::Started, None)),
        "{row:?}"
    );
    let child_pid = probe_pane_pid(&rig.fx.agent_socket, &child_session)
        .unwrap_or_else(|| panic!("the launched child has no pane\n{}", rig.diagnose()));

    // A third dashboard finishes readiness on the terminal that is already there.
    assert!(
        rig.fx.relaunch_dashboard(),
        "the third dashboard did not start"
    );
    let third = format!("pid:{}", rig.fx.dashboard_pid().expect("dashboard pid"));
    let done = rig
        .wait_receipt(REQUEST_ID, Duration::from_secs(30), |receipt| {
            receipt.state.is_final()
        })
        .unwrap_or_else(|| panic!("the request was never finished\n{}", rig.diagnose()));
    assert_eq!(done.state, ReceiptState::Ready, "{}", rig.diagnose());
    assert_eq!(done.claimed_by.as_deref(), Some(third.as_str()));
    assert_eq!(
        probe_pane_pid(&rig.fx.agent_socket, &child_session).as_deref(),
        Some(child_pid.as_str()),
        "recovery must keep the child's original terminal"
    );
    let mut expected = vec![rig.parent_session(), child_session];
    expected.sort();
    assert_eq!(rig.terminals(), expected, "{}", rig.diagnose());
    assert_eq!(
        rig.launches(&child),
        vec![MESSAGE.to_string()],
        "the Message must be delivered exactly once"
    );
    let children = rig.children();
    assert_eq!(children.len(), 1, "{}", rig.diagnose());
    assert_eq!(
        children[0]
            .launch
            .as_ref()
            .and_then(|launch| launch.outcome),
        Some(SpawnOutcome::Ready)
    );

    // The command that published the request before both crashes waited them out, and still names the
    // one child those crashes left running. `--wait 90` is the deadline it sits out, since its job is
    // held open until the release below.
    let run = rig.wait_spawn(PARENT, "spawn", Duration::from_secs(120));
    assert_still_working(&run, &rig);
    assert_eq!(run.child_id(), Some(child.as_str()));

    // And the third dashboard, which only finished readiness, also retires it when it ends.
    rig.release_jobs();
    assert!(
        rig.wait_retired(&child, Duration::from_secs(30)),
        "the finished job was never retired\n{}",
        rig.diagnose()
    );
    assert_eq!(
        rig.receipt(REQUEST_ID).map(|receipt| receipt.state),
        Some(ReceiptState::Done),
        "{}",
        rig.diagnose()
    );
}
