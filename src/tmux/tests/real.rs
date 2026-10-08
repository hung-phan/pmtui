//! Tests for the real driver's pure parts — shell quoting, the target anchors and
//! the wrapper script, which is also RUN under a real `sh` here because that is the
//! only way to prove the command's exit code (never `tee`'s) round-trips through
//! the pipe.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::tempdir;

use crate::tmux::codex::{
    process_parent_and_start_time, process_path_gone,
    session_id_from_proc as codex_session_id_from_proc,
};
use crate::tmux::fake::FakeDriver;
use crate::tmux::real::{
    codex_draft_visible, exact, exact_pane, launched_binary, pane_dimension, shq, wrapper_script,
};
use crate::tmux::{
    Driver, Observation, PaneDialog, StepHandle, TmuxDriver, classify_dialog, observe,
};
use crate::tmux::{
    LAUNCH_COMMAND_MAX_BYTES, LaunchError, LaunchOutcome, ManagedEnv, launch_command,
};

#[test]
fn shq_escapes_single_quotes() {
    assert_eq!(shq("simple"), "'simple'");
    assert_eq!(shq("it's"), "'it'\\''s'");
}

#[test]
fn launch_command_is_the_quoted_argv_tmux_receives() {
    assert_eq!(
        launch_command(&["claude".into(), "--".into(), "it's".into()]),
        "'claude' '--' 'it'\\''s'"
    );
}

#[test]
fn an_oversized_launch_is_refused_with_its_size_before_tmux_runs() {
    // tmux's own "command too long" goes to the stderr the driver discards, so without this
    // the human only ever saw "tmux new-session failed". The scripted tmux records every call,
    // so it proves no `new-session` is sent for the refused launch.
    let fx = ScriptedTmux::new();
    let argv = ["sh".to_string(), "x".repeat(LAUNCH_COMMAND_MAX_BYTES)];
    let env = ManagedEnv::default();
    let error = fx
        .driver
        .launch_interactive("gone", fx._dir.path(), &argv, &env)
        .expect_err("an over-budget launch must be refused");
    assert_eq!(
        error,
        LaunchError::CommandTooLong {
            session: "gone".into(),
            bytes: launch_command(&argv).len(),
        }
    );
    assert!(error.proven_not_started(), "tmux never ran: {error}");
    let text = error.to_string();
    assert!(
        text.contains("after shell quoting")
            && text.contains(&LAUNCH_COMMAND_MAX_BYTES.to_string()),
        "{text}"
    );
    // An already-live session launches nothing, so the same argv is not refused there.
    assert_eq!(
        fx.driver
            .launch_interactive("already", fx._dir.path(), &argv, &env)
            .unwrap(),
        LaunchOutcome::AlreadyAlive
    );
    let calls = std::fs::read_to_string(&fx.log).unwrap();
    assert!(calls.contains("has-session -t =gone\n"), "{calls}");
    assert!(!calls.contains("new-session"), "{calls}");
}

#[test]
fn codex_draft_detection_uses_the_bottom_composer_not_the_transcript_echo() {
    let prompt = concat!(
        "You are a long-running agent working toward the goal.\n",
        "- safe production-shaped filler line 20\n",
        "- safe production-shaped filler line 21\n",
    );
    assert!(codex_draft_visible(
        "› [Pasted Content 2400 chars]\n\n  model footer\n",
        prompt,
        true
    ));
    assert!(codex_draft_visible(
        "› You are a long-running agent working toward the goal.\n\n  model footer\n",
        prompt,
        true
    ));
    assert!(codex_draft_visible(
        "› - safe production-shaped filler line 21\n\n  model footer\n",
        prompt,
        true
    ));
    assert!(!codex_draft_visible(
        "› You are a long-running agent working toward the goal.\n\n• Working (1s • esc to interrupt)\n\n› Ask Codex to do anything\n\n  model footer\n",
        prompt,
        true
    ));
    assert!(!codex_draft_visible(
        "Would you like to run this command?\n› 1. Yes, proceed\n  2. No\n",
        prompt,
        true
    ));
}

#[test]
fn a_literal_codex_draft_is_pending_only_while_the_composer_holds_the_whole_message() {
    // After a submit Codex refills its composer with a rotating placeholder, and a short
    // message can be a prefix of one ("Write tests" of "Write tests for @filename"). These
    // captures carry no styles, so only the whole message still sitting there is a pending draft.
    let placeholder =
        "› Write tests\n\n• Working (0s)\n\n› Write tests for @filename\n\n  footer\n";
    assert!(!codex_draft_visible(placeholder, "Write tests", false));
    assert!(codex_draft_visible(
        "› Write tests  \n\n  footer\n",
        " Write tests",
        false
    ));
    assert!(!codex_draft_visible(
        "› Summarize recent commits\n\n  footer\n",
        "Summarize",
        false
    ));
    // Codex's collapsed-paste label is never a placeholder, so it stays pending either way.
    assert!(codex_draft_visible(
        "› [Pasted Content 900 chars]\n",
        "Write tests",
        false
    ));
    assert!(!codex_draft_visible(
        "no composer here\n",
        "Write tests",
        false
    ));
    // A bracketed paste keeps the prefix rule: its composer may show only the first line.
    assert!(codex_draft_visible(placeholder, "Write tests", true));
}

/// Codex wraps a literal message wider than its composer onto indented continuation rows, and
/// only the first carries the `›` lead. Captured from Codex 0.158 in a 90-column pane.
#[test]
fn a_literal_codex_draft_wrapped_over_several_rows_is_still_pending() {
    let message = "AM_WRAP_PROBE please review the recent commits in this repository and summarize the three riskiest changes, then propose a short plan for each of them END";
    let wrapped = concat!(
        "› AM_WRAP_PROBE please review the recent commits in this repository and summarize the\n",
        "  three riskiest changes, then propose a short plan for each of them END\n",
        "\n",
        "  GPT-5.6-Sol xhigh · /workplace/phahng · Context 0% used\n",
    );
    assert!(codex_draft_visible(wrapped, message, false));
    // A word wider than the composer is broken mid-word, with no space at the break.
    let long_word = "check https://example.com/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/b  two  spaces END";
    let broken = concat!(
        "› check https://example.com/\n",
        "  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        "  aaaaaaaaaaaaaaaaa/b  two  spaces END\n",
        "\n",
        "  footer\n",
    );
    assert!(codex_draft_visible(broken, long_word, false));
    // The rows below the composer's blank line are the footer, never more of the draft.
    assert!(!codex_draft_visible(
        "› AM_WRAP_PROBE please review\n\n  the rest\n",
        "AM_WRAP_PROBE please review the rest",
        false
    ));
    // A placeholder the message merely starts with is not the message.
    assert!(!codex_draft_visible(
        "› Explain this codebase\n\n  footer\n",
        "Explain this codebase's auth flow",
        false
    ));
    // An empty send has no draft to wait for.
    assert!(!codex_draft_visible("›\n\n  footer\n", "  ", false));
}

/// With styles kept (`capture-pane -e`), Codex draws its rotating placeholder dim and typed text
/// plain, so a message that equals a placeholder still reads as submitted once Codex took it.
#[test]
fn a_dim_codex_placeholder_is_never_a_pending_draft() {
    let placeholder =
        "\u{1b}[1m›\u{1b}[0m \u{1b}[2mSummarize recent commits\u{1b}[0m\n\n  footer\n";
    let typed = "\u{1b}[1m›\u{1b}[0m Summarize recent commits\n\n  footer\n";
    for pasted in [false, true] {
        assert!(!codex_draft_visible(
            placeholder,
            "Summarize recent commits",
            pasted
        ));
        assert!(codex_draft_visible(
            typed,
            "Summarize recent commits",
            pasted
        ));
    }
    // Dimness is judged on the draft, not on a dim row above it or a style left open before it.
    let after_dim_echo = "\u{1b}[2m› Summarize recent commits\u{1b}[0m\n\n\u{1b}[1m›\u{1b}[0m Summarize recent commits\n";
    assert!(codex_draft_visible(
        after_dim_echo,
        "Summarize recent commits",
        false
    ));
    // Codex's collapsed-paste label is drawn in colour, and it is always pending.
    let label = "\u{1b}[1m›\u{1b}[0m \u{1b}[38;5;6m[Pasted Content 2990 chars]\u{1b}[39m\n";
    assert!(codex_draft_visible(label, "anything", true));
}

#[test]
fn wrapper_script_quotes_and_records_exit() {
    let s = wrapper_script(
        &[
            "reference-coordinator.sh".to_string(),
            "/tmp/proj root".to_string(),
        ],
        Path::new("/x/y.done"),
        Path::new("/x/y.log"),
    );
    // The command's argv is still shq-quoted verbatim.
    assert!(
        s.contains("'reference-coordinator.sh' '/tmp/proj root'"),
        "{s}"
    );
    // Combined output is tee'd to the log so it also reaches the pane.
    assert!(s.contains("2>&1 | tee '/x/y.log'"), "{s}");
    // The exit code captured is the COMMAND's (echoed inside the group,
    // before tee runs), stashed beside the done-signal and read back.
    assert!(
        s.contains("{ 'reference-coordinator.sh' '/tmp/proj root'; echo $? >'/x/y.done'.code; }"),
        "{s}"
    );
    assert!(
        s.contains("code=$(cat '/x/y.done'.code 2>/dev/null)"),
        "{s}"
    );
    // The commit is still guarded so a failed write never publishes an empty signal.
    assert!(
        s.contains("printf '%s' \"$code\" >'/x/y.done'.tmp && mv -f '/x/y.done'.tmp '/x/y.done'"),
        "{s}"
    );
}

/// Write the generated wrapper to a temp file, run it under a real `sh`, and
/// return `(done-signal contents, log contents)`. This is the only way to
/// prove the exit code round-trips through the pipe (FakeDriver never runs it).
fn run_wrapper(dir: &Path, name: &str, command: &[String]) -> (String, String) {
    let done = dir.join(format!("{name}.done"));
    let log = dir.join(format!("{name}.log"));
    let wrapper = dir.join(format!("{name}.run.sh"));
    std::fs::write(&wrapper, wrapper_script(command, &done, &log)).unwrap();
    let status = Command::new("sh")
        .arg(&wrapper)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "wrapper itself should exit 0");
    let signal = std::fs::read_to_string(&done).unwrap_or_default();
    let log_contents = std::fs::read_to_string(&log).unwrap_or_default();
    (signal, log_contents)
}

#[test]
fn wrapper_run_records_exit_zero_and_tees_output_to_log() {
    let dir = tempdir().unwrap();
    let (signal, log) = run_wrapper(
        dir.path(),
        "ok",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "printf out; printf err 1>&2; exit 0".to_string(),
        ],
    );
    // The command's exit code (0), NOT tee's, is committed.
    assert_eq!(signal, "0", "done-signal should be the command's exit code");
    // Combined stdout+stderr reached the log (and thus the pane).
    assert!(log.contains("out"), "log missing stdout: {log:?}");
    assert!(log.contains("err"), "log missing stderr: {log:?}");
    // The internal `echo $?` must NOT leak into the log/pane. The command's
    // output has no digits, so ANY numeric char in the log is a leaked exit
    // code — this catches interleaving (`outerr0`) that a `!contains("out0")`
    // check would miss.
    assert!(
        !log.contains(char::is_numeric),
        "exit-code echo leaked a digit into log: {log:?}"
    );
}

#[test]
fn wrapper_run_records_nonzero_exit() {
    let dir = tempdir().unwrap();
    let (signal, _log) = run_wrapper(
        dir.path(),
        "fail",
        &["sh".to_string(), "-c".to_string(), "exit 7".to_string()],
    );
    // Proves $? is the command's (7), never tee's (which would be 0).
    assert_eq!(
        signal, "7",
        "done-signal should preserve non-zero exit code"
    );
}

#[test]
fn wrapper_run_preserves_exit_with_heavy_output() {
    let dir = tempdir().unwrap();
    // Emit a lot of output (100k lines) before exiting non-zero: even when
    // tee is busy streaming, the recorded code stays the command's.
    let (signal, log) = run_wrapper(
        dir.path(),
        "heavy",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "i=0; while [ $i -lt 100000 ]; do echo line$i; i=$((i+1)); done; exit 5".to_string(),
        ],
    );
    assert_eq!(signal, "5", "heavy output must not corrupt the exit code");
    assert!(log.contains("line0"), "log missing first line");
    assert!(log.contains("line99999"), "log missing last line");
}

#[test]
fn wrapper_run_with_unwritable_code_file_publishes_empty_signal_not_zero() {
    // If `{done}.code` can't be written (here a directory occupies its path so
    // `echo $? >{done}.code` fails), `code` is empty → an EMPTY done-signal is
    // published, which observe() treats as orphan/timeout — never a fabricated
    // exit 0. This shows the ENOSPC fail-safe survives the tee/.code structure.
    let dir = tempdir().unwrap();
    let done = dir.path().join("blocked.done");
    let log = dir.path().join("blocked.log");
    let wrapper = dir.path().join("blocked.run.sh");
    // Occupy `{done}.code` with a directory so the code-file write fails.
    std::fs::create_dir(format!("{}.code", done.display())).unwrap();
    std::fs::write(
        &wrapper,
        wrapper_script(
            &["sh".to_string(), "-c".to_string(), "exit 0".to_string()],
            &done,
            &log,
        ),
    )
    .unwrap();
    let status = Command::new("sh")
        .arg(&wrapper)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "wrapper still exits cleanly");

    let signal = std::fs::read_to_string(&done).unwrap_or_default();
    assert!(
        signal.trim().is_empty(),
        "an unwritable code-file must publish an EMPTY signal, got {signal:?}"
    );
    assert_ne!(signal, "0", "must never fabricate a clean exit 0");

    // The empty signal + a dead session routes to Orphaned, not Completed.
    let h = StepHandle {
        session: "gone".to_string(),
        done_signal: done.clone(),
        log,
    };
    let d = FakeDriver::new();
    d.set_alive("gone", false);
    assert_eq!(observe(&d, &h).unwrap(), Observation::Orphaned);
}

#[test]
fn exact_anchors_the_target() {
    assert_eq!(exact("pmd-web-2"), "=pmd-web-2");
}

#[test]
fn exact_pane_anchors_the_session_and_selects_its_pane() {
    // The PANE form: still `=`-anchored (no prefix matching, so `pmd-web-2`
    // can never reach `pmd-web-2-0`), plus the trailing `:` that makes it a
    // pane target. tmux rejects the bare `=name` for send-keys/paste-buffer/
    // capture-pane, so this suffix is what makes a nudge land at all.
    assert_eq!(exact_pane("pmd-web-2"), "=pmd-web-2:");
    assert!(exact_pane("s").starts_with('='), "must stay exact-anchored");
    assert!(exact_pane("s").ends_with(':'), "must be a pane target");
}

#[test]
fn terminate_reports_when_tmux_cannot_be_spawned() {
    let driver = TmuxDriver {
        tmux: "/definitely/missing/tmux".into(),
        socket: Some("unused".into()),
    };
    assert!(
        driver.terminate("pm-x").is_err(),
        "a failed kill must not be reported as success"
    );
}

/// Run a just-written script once so the fixture starts only after it can be exec'd. A sibling
/// test that forks while `fs::write` holds the script open keeps a write fd on it until that child
/// execs, and exec of a file open for writing fails with ETXTBSY. The driver maps a failed spawn to
/// "no tmux", so the test's first call would read an empty answer instead of the script's.
fn wait_until_executable(bin: &Path) {
    for _ in 0..100 {
        match Command::new(bin).arg("exec-probe").status() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => {
                result.unwrap();
                return;
            }
        }
    }
    panic!("{bin:?} stayed busy for writing past the retry budget");
}

struct ScriptedTmux {
    _dir: tempfile::TempDir,
    driver: TmuxDriver,
    fail: PathBuf,
    log: PathBuf,
    payload: PathBuf,
    moved: PathBuf,
    pane_path: PathBuf,
    late: PathBuf,
    /// The pid the scripted tmux reports for a `=ownpid:` target — a process group the TEST created,
    /// so a signal test never aims at anything it does not own.
    own_pid: PathBuf,
}

impl ScriptedTmux {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let bin = dir.path().join("tmux");
        let fail = dir.path().join("fail");
        let live = dir.path().join("live");
        let log = dir.path().join("calls");
        let payload = dir.path().join("payload");
        let moved = dir.path().join("moved");
        let submitted = dir.path().join("submitted");
        let pane_path = dir.path().join("pane-path");
        let late = dir.path().join("late-env");
        let initial = dir.path().join("initial");
        let changed = dir.path().join("changed");
        let own_pid = dir.path().join("own-pid");
        std::fs::create_dir(&live).unwrap();
        std::fs::write(live.join("already"), "").unwrap();
        std::fs::write(
            &initial,
            "Which option?\n❯ 1. Alpha\n  2. Beta\nEnter to select · Esc to cancel\n",
        )
        .unwrap();
        std::fs::write(
            &changed,
            "Which option?\n  1. Alpha\n❯ 2. Beta\nEnter to select · Esc to cancel\n",
        )
        .unwrap();
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
[ "$1" = "-L" ] && shift 2
cmd=$1; shift
fail=$(cat {fail} 2>/dev/null)
case "$cmd" in
list-sessions) printf 'alpha\nbeta\n' ;;
has-session)
  [ "$fail" = unexec-on-has ] && chmod -x "$0" && exit 1
  target=$2
  target=${{target#=}}
  target=${{target%:}}
  [ -f {live}/"$target" ] ;;
new-session)
  session=
  previous=
  for arg in "$@"; do
    [ "$previous" = -s ] && session=$arg && break
    previous=$arg
  done
  [ "$fail" = new-session ] && exit 1
  [ "$fail" = new-session-race ] && : >{live}/"$session" && exit 1
  [ "$fail" = new-session-dead ] && exit 0
  [ "$fail" = new-session-unexec ] && chmod -x "$0" && exit 1
  [ "$fail" = new-session-dead-unexec ] && chmod -x "$0" && exit 0
  : >{live}/"$session" ;;
display-message)
  [ "$fail" = display ] && exit 1
  case "$*" in
    *pane_current_command*) [ "$fail" = empty-command ] || echo codex ;;
    *pane_pid*)
      case "$*" in
        *=initpid:*) echo 1 ;;
        *=gonepid:*) echo 4194303 ;;
        *=ownpid:*) cat {own_pid} ;;
        *) echo not-a-pid ;;
      esac ;;
    *session_created*) case "$*" in *=created:*) echo 1700000000;; *) echo junk;; esac ;;
    *pane_dead*) case "$*" in *=dead:*) echo 1;; *) echo junk;; esac ;;
    *pane_in_mode*) case "$*" in *=mode:*) echo 1;; *) echo 0;; esac ;;
  esac ;;
capture-pane)
  [ "$fail" = capture ] && exit 1
  case "$*" in
    *dialog*) [ -f {moved} ] && cat {changed} || cat {initial} ;;
    *=long:*)
      count=$(grep -c 'send-keys -t =long: Enter' {log})
      [ "$count" -lt 2 ] && echo '› [Pasted Content 801 chars]' || echo '› Ask Codex to do anything' ;;
    *=second-enter:*) echo '› [Pasted Content 801 chars]' ;;
    *) echo captured ;;
  esac ;;
load-buffer)
  [ "$fail" = load ] && cat >/dev/null && exit 1
  [ "$fail" = load-close ] && exit 0
  cat >{payload} ;;
paste-buffer) [ "$fail" = paste ] && exit 1; exit 0 ;;
send-keys)
  case "$*" in *Down*) [ "$fail" = static-dialog ] || : >{moved};; esac
  case "$fail:$*" in
    literal:*'-l'*) exit 1 ;;
    enter:*Enter*) exit 1 ;;
    second-enter:*Enter*)
      [ -f {submitted} ] && exit 1
      : >{submitted} ;;
    dialog:*) exit 1 ;;
  esac ;;
show-environment)
  case "$fail" in
    no-server-env) exit 1 ;;
    unset-path) echo '-PATH' ;;
    *)
      # `late-env`: no server to ask until one exists, then readable.
      if [ -f {late} ] && [ ! -f {late}.seen ]; then : >{late}.seen; exit 1; fi
      if [ -f {pane_path} ]; then printf 'PATH=%s\n' "$(cat {pane_path})"; else printf 'PATH=%s\n' "$PATH"; fi ;;
  esac ;;
list-clients) [ "$fail" = clients ] && exit 1; case "$*" in *attached*) echo client;; esac ;;
kill-session)
  case "$*" in
    *missing*) echo "can't find session" >&2; exit 1 ;;
    *noserver*) echo "no server running" >&2; exit 1 ;;
    *nosessions*) echo "no sessions" >&2; exit 1 ;;
    *weird*) echo "unexpected failure" >&2; exit 1 ;;
  esac ;;
resize-window|set-option|clear-history|bind-key) [ "$fail" = "$cmd" ] && exit 1 ;;
esac
"#,
            log = shq(&log.to_string_lossy()),
            fail = shq(&fail.to_string_lossy()),
            live = shq(&live.to_string_lossy()),
            payload = shq(&payload.to_string_lossy()),
            moved = shq(&moved.to_string_lossy()),
            submitted = shq(&submitted.to_string_lossy()),
            pane_path = shq(&pane_path.to_string_lossy()),
            late = shq(&late.to_string_lossy()),
            own_pid = shq(&own_pid.to_string_lossy()),
            initial = shq(&initial.to_string_lossy()),
            changed = shq(&changed.to_string_lossy()),
        );
        std::fs::write(&bin, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        wait_until_executable(&bin);
        let _ = std::fs::remove_file(&log);
        Self {
            driver: TmuxDriver {
                tmux: bin.to_string_lossy().into_owned(),
                socket: Some("scripted".into()),
            },
            _dir: dir,
            fail,
            log,
            payload,
            moved,
            pane_path,
            late,
            own_pid,
        }
    }

    fn fail(&self, value: &str) {
        std::fs::write(&self.fail, value).unwrap();
    }

    /// What this driver's tmux server reports as its global `PATH` — the one a pane it creates
    /// searches, whoever started that server.
    fn pane_path(&self, value: &str) {
        std::fs::write(&self.pane_path, value).unwrap();
    }

    /// Make the server's `PATH` unreadable until a session exists, as a real socket behaves.
    fn late_server_env(&self) {
        std::fs::write(&self.late, "").unwrap();
    }
}

#[test]
fn constructors_and_attach_command_build_expected_arguments() {
    let default = TmuxDriver::default();
    assert_eq!(default.tmux, "tmux");
    assert_eq!(default.socket(), None);

    let driver = TmuxDriver::with_socket("private");
    assert_eq!(driver.socket(), Some("private"));
    let command = driver.attach_command("pm-project");
    assert_eq!(command.get_program(), "tmux");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["-L", "private", "attach-session", "-t", "=pm-project"]
    );
}

#[test]
fn pane_dimensions_accept_only_valid_values_at_or_above_the_floor() {
    assert_eq!(pane_dimension(None, 200, 20), 200);
    assert_eq!(pane_dimension(Some("not-a-number"), 200, 20), 200);
    assert_eq!(pane_dimension(Some("19"), 200, 20), 200);
    assert_eq!(pane_dimension(Some(" 120 "), 200, 20), 120);
}

#[test]
fn missing_tmux_binary_reports_spawn_errors_without_panicking() {
    let driver = TmuxDriver {
        tmux: "/definitely/missing/tmux".into(),
        socket: None,
    };
    driver.ensure_detach_key();
    assert!(driver.list_sessions().is_empty());
    assert!(driver.is_alive("x").is_err());
    assert!(driver.capture_tail("x", 1).is_err());
    assert!(driver.capture_tail_styled("x", 1).is_err());
    assert!(driver.resize_window("x", 80, 24).is_err());
    assert!(driver.set_window_size_auto("x").is_err());
    assert!(driver.clear_history("x").is_err());
    assert!(driver.has_clients("x").is_err());
    assert!(driver.session_created("x").is_err());
    assert!(driver.pane_dead("x").is_err());
    assert!(driver.pane_in_mode("x").is_err());
    assert!(driver.codex_session_id("x", Path::new("/")).is_err());
    assert!(driver.send_keys("x", "short").is_err());
}

/// `request_stop` SIGNALS A PROCESS GROUP AND NEVER KILLS THE TERMINAL, and every answer tmux can give
/// about a pane that is not there resolves to `Ok`: nothing to signal is the outcome the caller wanted.
///
/// The one real signal here goes to a group this test created with `setsid`, so it can never reach
/// anything it does not own.
#[test]
fn request_stop_signals_the_panes_own_group_and_tolerates_a_missing_pane() {
    let fx = ScriptedTmux::new();

    // A process group of our own: signalled, and gone afterwards.
    let mut child = Command::new("setsid")
        .args(["sh", "-c", "sleep 30"])
        .spawn()
        .expect("spawn a scratch process group");
    std::fs::write(&fx.own_pid, child.id().to_string()).unwrap();
    fx.driver.request_stop("ownpid").expect("signal our group");
    let reaped = (0..50).any(|_| {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
        false
    });
    assert!(reaped, "the scratch group survived SIGTERM");
    let calls = std::fs::read_to_string(&fx.log).unwrap();
    assert!(
        !calls.contains("kill-session"),
        "a stop must not kill the terminal:\n{calls}"
    );

    // Nothing to signal, every way tmux can say so.
    fx.driver
        .request_stop("nosuch")
        .expect("an unparseable pid");
    fx.driver
        .request_stop("initpid")
        .expect("pid 1 is not a job");
    fx.driver
        .request_stop("gonepid")
        .expect("a group that ended");
    fx.fail("display");
    fx.driver.request_stop("ownpid").expect("no pane at all");
}

/// A driver that has not opted in still STOPS the job: the default `request_stop` terminates, so a
/// cancel is never silently dropped by a driver that cannot signal.
#[test]
fn the_default_request_stop_terminates_instead_of_doing_nothing() {
    let driver = FakeDriver::new();
    driver.set_alive("pm-job", true);
    driver.request_stop("pm-job").expect("the default path");
    assert!(!driver.is_alive("pm-job").unwrap(), "the job was stopped");
}

/// The `new-session` line the scripted tmux logged for `session`, split into its arguments.
fn logged_new_session(fx: &ScriptedTmux, session: &str) -> Vec<String> {
    let calls = std::fs::read_to_string(&fx.log).unwrap();
    let marker = format!("new-session -d -s {session} ");
    let line = calls
        .lines()
        .find(|line| line.contains(&marker))
        .unwrap_or_else(|| panic!("no new-session for {session}:\n{calls}"));
    line.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn interactive_launch_passes_each_managed_var_with_dash_e_before_the_command() {
    let fx = ScriptedTmux::new();
    let env = ManagedEnv {
        session_id: "s1".into(),
        state_dir: PathBuf::from("/x/.project-state/sessions/s1-0a1b2c3d"),
        pmtui_bin: Some(PathBuf::from("/bin/pmtui")),
    };
    assert_eq!(
        fx.driver
            .launch_interactive("started", fx._dir.path(), &["true".into()], &env)
            .unwrap(),
        LaunchOutcome::Started
    );
    let args = logged_new_session(&fx, "started");
    let y = args.iter().position(|arg| arg == "-y").expect("-y <rows>");
    assert_eq!(
        args[y + 2..],
        [
            "-e",
            "PMTUI_SESSION=s1",
            "-e",
            "PMTUI_STATE_DIR=/x/.project-state/sessions/s1-0a1b2c3d",
            "-e",
            "PMTUI_BIN=/bin/pmtui",
            "'true'",
        ],
        "every managed var sits after the size flags and before the command: {args:?}"
    );

    // Without a pmtui path the variable is omitted, not passed empty.
    let env = ManagedEnv {
        pmtui_bin: None,
        ..env
    };
    fx.driver
        .launch_interactive("no-bin", fx._dir.path(), &["true".into()], &env)
        .unwrap();
    let args = logged_new_session(&fx, "no-bin");
    assert!(
        !args.iter().any(|arg| arg.starts_with("PMTUI_BIN")),
        "{args:?}"
    );
    assert_eq!(args.last().map(String::as_str), Some("'true'"));
}

#[test]
fn already_alive_session_returns_already_alive_and_does_not_run_new_session() {
    let fx = ScriptedTmux::new();
    assert_eq!(
        fx.driver
            .launch_interactive(
                "already",
                fx._dir.path(),
                &["unused".into()],
                &ManagedEnv::default()
            )
            .unwrap(),
        LaunchOutcome::AlreadyAlive
    );
    let calls = std::fs::read_to_string(&fx.log).unwrap();
    assert!(!calls.contains("new-session"), "{calls}");
    assert!(
        calls.contains("bind-key -n C-q detach-client"),
        "an adopted session still gets the detach key: {calls}"
    );

    // A racing creator that leaves the session alive after our non-zero exit is the same
    // outcome: our argv did not start it.
    fx.fail("new-session-race");
    assert_eq!(
        fx.driver
            .launch_interactive(
                "race",
                fx._dir.path(),
                &["true".into()],
                &ManagedEnv::default()
            )
            .unwrap(),
        LaunchOutcome::AlreadyAlive
    );
}

#[test]
fn new_session_nonzero_with_absent_session_is_new_session_failed() {
    let fx = ScriptedTmux::new();
    fx.fail("new-session");
    let error = fx
        .driver
        .launch_interactive(
            "create-failed",
            fx._dir.path(),
            &["true".into()],
            &ManagedEnv::default(),
        )
        .unwrap_err();
    assert_eq!(
        error,
        LaunchError::NewSessionFailed(
            "tmux new-session failed for interactive session create-failed".into()
        )
    );
    assert!(
        !error.proven_not_started(),
        "the session may have briefly existed"
    );
}

#[test]
fn a_session_that_dies_right_after_creation_is_exited_after_start() {
    let fx = ScriptedTmux::new();
    fx.fail("new-session-dead");
    let error = fx
        .driver
        .launch_interactive(
            "exited",
            fx._dir.path(),
            &["true".into()],
            &ManagedEnv::default(),
        )
        .unwrap_err();
    assert_eq!(
        error,
        LaunchError::ExitedAfterStart(
            "interactive session exited exited immediately (did \"true\" fail to start?)".into()
        )
    );
    assert!(
        !error.proven_not_started(),
        "the agent may have read its argv"
    );
}

#[test]
fn interactive_launch_refuses_before_tmux_runs_only_when_nothing_can_have_started() {
    let fx = ScriptedTmux::new();
    let driver = &fx.driver;
    let env = ManagedEnv::default();
    assert_eq!(
        driver.launch_interactive("empty", fx._dir.path(), &[], &env),
        Err(LaunchError::EmptyArgv)
    );
    assert_eq!(
        driver.launch_interactive(
            "missing-bin",
            fx._dir.path(),
            &["definitely-not-an-installed-binary".into()],
            &env
        ),
        Err(LaunchError::NotOnPath(
            "definitely-not-an-installed-binary".into()
        ))
    );
    let calls = std::fs::read_to_string(&fx.log).unwrap_or_default();
    assert!(!calls.contains("new-session"), "{calls}");

    driver
        .launch_interactive("started", fx._dir.path(), &["true".into()], &env)
        .unwrap();
    let calls = std::fs::read_to_string(&fx.log).unwrap();
    assert!(calls.contains("new-session -d -s started"));
    assert!(calls.contains(" -x "));
    assert!(calls.contains(" -y "));
    assert!(calls.contains("bind-key -n C-q detach-client"));
}

#[test]
fn an_env_prefix_is_skipped_to_check_the_real_agent_binary() {
    let fx = ScriptedTmux::new();
    let env = ManagedEnv::default();
    let argv = |words: &[&str]| {
        words
            .iter()
            .map(|word| word.to_string())
            .collect::<Vec<_>>()
    };

    // Claude launches as `env -u CLAUDECODE claude …`: `env` is always on PATH, so checking
    // argv[0] could never report a missing agent.
    assert_eq!(
        fx.driver.launch_interactive(
            "env-missing",
            fx._dir.path(),
            &argv(&[
                "env",
                "-u",
                "CLAUDECODE",
                "FOO=bar",
                "definitely-not-an-installed-binary",
                "--flag"
            ]),
            &env
        ),
        Err(LaunchError::NotOnPath(
            "definitely-not-an-installed-binary".into()
        ))
    );
    let calls = std::fs::read_to_string(&fx.log).unwrap_or_default();
    assert!(!calls.contains("new-session"), "{calls}");

    assert_eq!(
        fx.driver
            .launch_interactive(
                "env-present",
                fx._dir.path(),
                &argv(&["env", "-u", "CLAUDECODE", "FOO=bar", "true"]),
                &env
            )
            .unwrap(),
        LaunchOutcome::Started
    );
    // Nothing after the prefix: `env` itself is what runs.
    assert_eq!(
        fx.driver
            .launch_interactive(
                "env-alone",
                fx._dir.path(),
                &argv(&["env", "-u", "CLAUDECODE"]),
                &env
            )
            .unwrap(),
        LaunchOutcome::Started
    );
}

#[test]
fn a_binary_only_the_tmux_servers_own_path_resolves_is_still_launched() {
    // A pane inherits the SERVER's environment, so pmtui's own PATH is not the authority when
    // somebody else started that server: refusing here would refuse a launch that works.
    let fx = ScriptedTmux::new();
    let stub_dir = fx._dir.path().join("stub-bin");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("server-only-agent");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    wait_until_executable(&stub);
    assert!(
        !super::super::real::resolves_on_path("server-only-agent", None),
        "the stub must be off our own PATH for this test to mean anything"
    );
    fx.pane_path(&stub_dir.to_string_lossy());

    let argv = ["env", "-u", "CLAUDECODE", "server-only-agent"].map(str::to_string);

    assert_eq!(
        fx.driver
            .launch_interactive("server-path", fx._dir.path(), &argv, &ManagedEnv::default())
            .unwrap(),
        LaunchOutcome::Started
    );
    let calls = std::fs::read_to_string(&fx.log).unwrap_or_default();
    assert!(calls.contains("new-session"), "{calls}");
}

#[test]
fn an_unreadable_server_path_launches_instead_of_refusing() {
    // `self.tmux` may be a wrapper that gives the pane a PATH we cannot see, so "no server yet"
    // and "no PATH in its environment" both prove nothing. Launch, and let the pane's own exit
    // report the outcome.
    let argv = ["env", "-u", "CLAUDECODE", "definitely-not-installed"].map(str::to_string);

    for arm in ["no-server-env", "unset-path"] {
        let fx = ScriptedTmux::new();
        fx.fail(arm);
        assert_eq!(
            fx.driver
                .launch_interactive(arm, fx._dir.path(), &argv, &ManagedEnv::default())
                .unwrap(),
            LaunchOutcome::Started,
            "{arm}"
        );
        let calls = std::fs::read_to_string(&fx.log).unwrap_or_default();
        assert!(calls.contains("new-session"), "{arm}: {calls}");
    }
}

#[test]
fn a_pane_that_died_without_the_engine_on_its_path_proves_nothing_ran() {
    // The session existed long enough to read its PATH, and that PATH cannot resolve the engine:
    // the pane never exec'd our argv, so this is a pre-start failure, not an ambiguous exit.
    let fx = ScriptedTmux::new();
    // `new-session-dead` leaves no live session; the late server environment keeps the PATH
    // unreadable until then, so the pre-flight check cannot refuse and the launch runs.
    fx.fail("new-session-dead");
    fx.late_server_env();
    let argv = ["env", "-u", "CLAUDECODE", "definitely-not-installed"].map(str::to_string);

    assert_eq!(
        fx.driver.launch_interactive(
            "dead-no-engine",
            fx._dir.path(),
            &argv,
            &ManagedEnv::default()
        ),
        Err(LaunchError::NotOnPath("definitely-not-installed".into()))
    );
    let calls = std::fs::read_to_string(&fx.log).unwrap_or_default();
    assert!(calls.contains("new-session"), "{calls}");
}

#[test]
fn the_launched_binary_is_the_payload_after_an_env_prefix() {
    let argv = |words: &[&str]| {
        words
            .iter()
            .map(|word| word.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(launched_binary(&argv(&["claude", "--x"])), "claude");
    assert_eq!(
        launched_binary(&argv(&[
            "env", "-u", "A", "-u", "B", "K=V", "codex", "exec"
        ])),
        "codex"
    );
    assert_eq!(launched_binary(&argv(&["env", "K=V"])), "env");
    assert_eq!(launched_binary(&argv(&["env", "-u"])), "env");
    assert_eq!(
        launched_binary(&argv(&["/usr/bin/env", "claude"])),
        "claude"
    );
}

#[test]
fn a_tmux_that_cannot_run_is_an_ambiguous_probe_error() {
    let env = ManagedEnv::default();
    let missing = TmuxDriver {
        tmux: "/definitely/missing/tmux".into(),
        socket: None,
    };
    let error = missing
        .launch_interactive("x", Path::new("/"), &["true".into()], &env)
        .unwrap_err();
    assert!(
        matches!(&error, LaunchError::Probe(text) if text.starts_with("run tmux has-session: ")),
        "the probe keeps its whole cause chain: {error:?}"
    );
    assert!(!error.proven_not_started());

    // The same holds wherever tmux stops being runnable: at `new-session` itself, at the
    // re-check after a non-zero exit, and at the re-check after a zero one.
    for (failure, context) in [
        ("unexec-on-has", "run tmux new-session (interactive): "),
        ("new-session-unexec", "run tmux has-session: "),
        ("new-session-dead-unexec", "run tmux has-session: "),
    ] {
        let fx = ScriptedTmux::new();
        fx.fail(failure);
        let error = fx
            .driver
            .launch_interactive("probe", fx._dir.path(), &["true".into()], &env)
            .unwrap_err();
        assert!(
            matches!(&error, LaunchError::Probe(text) if text.starts_with(context)),
            "{failure}: {error:?}"
        );
    }
}

#[test]
fn scripted_tmux_covers_fail_safe_and_error_paths() {
    let fx = ScriptedTmux::new();
    let driver = &fx.driver;
    assert_eq!(driver.list_sessions(), ["alpha", "beta"]);
    assert!(driver.is_alive("already").unwrap());
    assert!(!driver.is_alive("gone").unwrap());
    assert!(driver.has_clients("attached").unwrap());
    assert!(!driver.has_clients("detached").unwrap());
    fx.fail("clients");
    assert!(!driver.has_clients("attached").unwrap());
    fx.fail("");
    assert_eq!(
        driver.session_created("created").unwrap(),
        Some(1_700_000_000)
    );
    assert_eq!(driver.session_created("unknown").unwrap(), None);
    assert!(driver.pane_dead("dead").unwrap());
    assert!(!driver.pane_dead("unknown").unwrap());
    assert!(driver.pane_in_mode("mode").unwrap());
    assert_eq!(driver.capture_tail("plain", 2).unwrap(), "captured\n");
    assert_eq!(
        driver.capture_tail_styled("plain", 2).unwrap(),
        "captured\n"
    );
    assert!(driver.codex_session_id("bad-pid", Path::new("/")).is_err());
    for session in ["missing", "noserver", "nosessions"] {
        driver.terminate(session).unwrap();
    }
    driver.terminate("success").unwrap();
    assert!(driver.terminate("weird").is_err());

    driver.send_keys("mode", "short").unwrap();
    driver.send_keys("long", &"x".repeat(801)).unwrap();
    assert_eq!(std::fs::read_to_string(&fx.payload).unwrap().len(), 801);
    let calls = std::fs::read_to_string(&fx.log).unwrap();
    assert!(calls.contains("send-keys -t =mode: -X cancel"));
    assert!(calls.contains("paste-buffer -p -r -b"));
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.ends_with("send-keys -t =long: Enter"))
            .count(),
        2
    );
    for (failure, text) in [
        ("literal", "short"),
        ("load", "first\nsecond"),
        ("paste", "first\nsecond"),
        ("enter", "short"),
    ] {
        fx.fail(failure);
        assert!(driver.send_keys("fail", text).is_err(), "{failure}");
    }
    fx.fail("empty-command");
    driver
        .send_keys("long-no-command", &"x".repeat(801))
        .unwrap();
    fx.fail("display");
    driver
        .send_keys("long-display-failure", &"x".repeat(801))
        .unwrap();
    assert!(driver.codex_session_id("missing", Path::new("/")).is_err());
    fx.fail("second-enter");
    assert!(driver.send_keys("second-enter", &"x".repeat(801)).is_err());

    fx.fail("");
    let expected =
        classify_dialog("Which option?\n❯ 1. Alpha\n  2. Beta\nEnter to select · Esc to cancel\n")
            .unwrap();
    assert!(driver.send_dialog_key_names("dialog", &[]).is_err());
    assert!(
        driver
            .send_dialog_key_names("dialog", &vec!["Down"; 65])
            .is_err()
    );
    driver.select_dialog_option("dialog", 0, 1).unwrap();
    let _ = std::fs::remove_file(&fx.moved);
    assert_eq!(
        driver
            .verify_dialog_interactive("dialog", &expected)
            .unwrap()
            .selected_index,
        Some(1)
    );
    driver
        .select_dialog_options("dialog", &expected, &[1])
        .unwrap();
    assert!(
        driver
            .verify_dialog_interactive(
                "dialog",
                &PaneDialog {
                    selected_index: None,
                    ..expected.clone()
                }
            )
            .is_err()
    );
    let _ = std::fs::remove_file(&fx.moved);
    fx.fail("static-dialog");
    assert!(
        driver
            .verify_dialog_interactive("dialog", &expected)
            .is_err()
    );
    assert!(driver.select_dialog_option("mode", 0, 1).is_err());
    fx.fail("dialog");
    assert!(driver.select_dialog_option("dialog", 0, 1).is_err());

    for failure in ["resize-window", "set-option", "clear-history"] {
        fx.fail(failure);
        driver.resize_window("x", 1, 1).unwrap();
        driver.set_window_size_auto("x").unwrap();
        driver.clear_history("x").unwrap();
    }
    fx.fail("capture");
    assert_eq!(driver.capture_tail("x", 1).unwrap(), "");
    assert_eq!(driver.capture_tail_styled("x", 1).unwrap(), "");
    fx.fail("display");
    assert_eq!(driver.session_created("x").unwrap(), None);

    let done = fx._dir.path().join("done");
    let log = fx._dir.path().join("nested/log");
    assert!(
        driver
            .spawn_step("empty", fx._dir.path(), &[], &done, &log)
            .is_err()
    );
    assert!(
        driver
            .spawn_step(
                "no-parent",
                fx._dir.path(),
                &["true".into()],
                Path::new("/"),
                &log,
            )
            .is_err()
    );
    let blocked_signal_parent = fx._dir.path().join("blocked-signal-parent");
    std::fs::write(&blocked_signal_parent, "file").unwrap();
    assert!(
        driver
            .spawn_step(
                "signal-parent",
                fx._dir.path(),
                &["true".into()],
                &blocked_signal_parent.join("done"),
                &log,
            )
            .is_err()
    );
    let blocked_log_parent = fx._dir.path().join("blocked-log-parent");
    std::fs::write(&blocked_log_parent, "file").unwrap();
    assert!(
        driver
            .spawn_step(
                "log-parent",
                fx._dir.path(),
                &["true".into()],
                &done,
                &blocked_log_parent.join("log"),
            )
            .is_err()
    );
    let stale_directory = fx._dir.path().join("stale-directory");
    std::fs::create_dir(&stale_directory).unwrap();
    assert!(
        driver
            .spawn_step(
                "stale-directory",
                fx._dir.path(),
                &["true".into()],
                &stale_directory,
                &log,
            )
            .is_err()
    );
    std::fs::write(&done, "stale").unwrap();
    fx.fail("");
    let handle = driver
        .spawn_step("step", fx._dir.path(), &["true".into()], &done, &log)
        .unwrap();
    assert_eq!(handle.done_signal, done);
    assert!(!handle.done_signal.exists());
    assert!(fx._dir.path().join("step.run.sh").exists());
    std::fs::create_dir(fx._dir.path().join("write-fail.run.sh")).unwrap();
    assert!(
        driver
            .spawn_step("write-fail", fx._dir.path(), &["true".into()], &done, &log,)
            .is_err()
    );
    fx.fail("new-session");
    assert!(
        driver
            .spawn_step("fail", fx._dir.path(), &["true".into()], &done, &log)
            .is_err()
    );
}

fn write_rollout(path: &Path, id: &str, session_id: &str, cwd: &Path, thread_source: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let line = serde_json::json!({
        "type": "session_meta",
        "payload": {
            "id": id,
            "session_id": session_id,
            "cwd": cwd,
            "thread_source": thread_source
        }
    });
    std::fs::write(path, format!("{line}\n")).unwrap();
}

fn proc_node(proc_root: &Path, pid: u32, parent_pid: u32, children: &[u32]) {
    let task = proc_root
        .join(pid.to_string())
        .join("task")
        .join(pid.to_string());
    std::fs::create_dir_all(&task).unwrap();
    std::fs::write(
        task.join("children"),
        children
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    )
    .unwrap();
    std::fs::write(proc_root.join(pid.to_string()).join("cmdline"), b"codex\0").unwrap();
    let mut stat_fields = vec!["S".to_string(), parent_pid.to_string()];
    stat_fields.extend(std::iter::repeat_n("0".to_string(), 17));
    stat_fields.push((u64::from(pid) * 100).to_string());
    std::fs::write(
        proc_root.join(pid.to_string()).join("stat"),
        format!("{pid} (codex) {}\n", stat_fields.join(" ")),
    )
    .unwrap();
    std::fs::create_dir_all(proc_root.join(pid.to_string()).join("fd")).unwrap();
}

#[cfg(unix)]
fn link_rollout_fd(proc_root: &Path, pid: u32, fd: u32, rollout: &Path) {
    std::os::unix::fs::symlink(
        rollout,
        proc_root
            .join(pid.to_string())
            .join("fd")
            .join(fd.to_string()),
    )
    .unwrap();
}

#[test]
#[cfg(unix)]
fn codex_proc_probe_chooses_the_user_rollout_and_rejects_a_newer_subagent() {
    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let proc_root = dir.path().join("proc");
    proc_node(&proc_root, 100, 0, &[101, 102]);
    proc_node(&proc_root, 101, 100, &[]);
    proc_node(&proc_root, 102, 100, &[]);
    std::fs::write(
        proc_root.join("102/cmdline"),
        b"tail\0/tmp/.codex/sessions/rollout-user.jsonl\0",
    )
    .unwrap();

    let sessions = dir.path().join("codex/sessions/2026/08/25");
    let user_id = "11111111-2222-4333-8444-555555555555";
    let subagent_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let user = sessions.join(format!("rollout-old-{user_id}.jsonl"));
    let subagent = sessions.join(format!("rollout-new-{subagent_id}.jsonl"));
    let unrelated_id = "99999999-8888-4777-8666-555555555555";
    let unrelated = sessions.join(format!("rollout-unrelated-{unrelated_id}.jsonl"));
    write_rollout(&user, user_id, user_id, &project, "user");
    write_rollout(&subagent, subagent_id, user_id, &project, "subagent");
    write_rollout(&unrelated, unrelated_id, unrelated_id, &project, "user");
    link_rollout_fd(&proc_root, 101, 47, &user);
    link_rollout_fd(&proc_root, 101, 199, &subagent);
    link_rollout_fd(&proc_root, 102, 9, &unrelated);

    assert_eq!(
        codex_session_id_from_proc(&proc_root, 100, &project)
            .unwrap()
            .as_deref(),
        Some(user_id)
    );
}

#[test]
#[cfg(unix)]
fn codex_proc_probe_follows_children_owned_by_a_non_leader_thread() {
    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let proc_root = dir.path().join("proc");
    proc_node(&proc_root, 100, 0, &[]);
    proc_node(&proc_root, 101, 100, &[]);
    let launcher_thread = proc_root.join("100/task/150");
    std::fs::create_dir_all(&launcher_thread).unwrap();
    std::fs::write(launcher_thread.join("children"), "101").unwrap();

    let id = "11111111-2222-4333-8444-555555555555";
    let rollout = dir
        .path()
        .join("codex/sessions/2026/08/25")
        .join(format!("rollout-live-{id}.jsonl"));
    write_rollout(&rollout, id, id, &project, "user");
    link_rollout_fd(&proc_root, 101, 47, &rollout);

    assert_eq!(
        codex_session_id_from_proc(&proc_root, 100, &project)
            .unwrap()
            .as_deref(),
        Some(id),
        "a multithreaded launcher may own the Codex child from a non-leader thread"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn codex_proc_probe_follows_a_real_child_spawned_by_a_non_leader_thread() {
    use std::os::unix::process::CommandExt;

    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let id = "11111111-2222-4333-8444-555555555555";
    let rollout = dir
        .path()
        .join("codex/sessions/2026/08/25")
        .join(format!("rollout-live-{id}.jsonl"));
    write_rollout(&rollout, id, id, &project, "user");
    let rollout_fd = std::fs::File::open(&rollout).unwrap();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let launcher = std::thread::spawn(move || {
        let mut child = Command::new("sleep")
            .arg0("codex")
            .arg("30")
            .stdin(Stdio::from(rollout_fd))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        ready_tx.send(child.id()).unwrap();
        stop_rx.recv().unwrap();
        let _ = child.kill();
        child.wait().unwrap();
    });
    let child_pid = ready_rx.recv().unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut observed = None;
    let mut probe_error = None;
    while std::time::Instant::now() < deadline {
        match codex_session_id_from_proc(Path::new("/proc"), std::process::id(), &project) {
            Ok(id) => observed = id,
            Err(err) => {
                probe_error = Some(err);
                break;
            }
        }
        if observed.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    stop_tx.send(()).unwrap();
    launcher.join().unwrap();
    if let Some(err) = probe_error {
        panic!("live Codex process probe failed: {err:#}");
    }
    assert_eq!(
        observed.as_deref(),
        Some(id),
        "child {child_pid} was launched by a non-leader test thread"
    );
}

#[test]
fn codex_proc_identity_parser_handles_parentheses_in_process_names() {
    let mut fields = vec!["S", "100"];
    fields.extend(std::iter::repeat_n("0", 17));
    fields.push("4242");
    let stat = format!("101 (codex helper (thread)) {}", fields.join(" "));

    assert_eq!(process_parent_and_start_time(&stat).unwrap(), (100, 4242));
}

#[test]
fn codex_proc_probe_treats_linux_esrch_as_a_vanished_process() {
    assert!(process_path_gone(&std::io::Error::from_raw_os_error(3)));
    assert!(process_path_gone(&std::io::Error::from(
        std::io::ErrorKind::NotFound
    )));
    assert!(!process_path_gone(&std::io::Error::from(
        std::io::ErrorKind::PermissionDenied
    )));
}

#[test]
#[cfg(unix)]
fn codex_proc_probe_rejects_a_reused_child_pid_with_a_different_parent() {
    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let proc_root = dir.path().join("proc");
    proc_node(&proc_root, 100, 0, &[101]);
    proc_node(&proc_root, 101, 999, &[]);

    let id = "11111111-2222-4333-8444-555555555555";
    let rollout = dir
        .path()
        .join("codex/sessions/2026/08/25")
        .join(format!("rollout-reused-{id}.jsonl"));
    write_rollout(&rollout, id, id, &project, "user");
    link_rollout_fd(&proc_root, 101, 47, &rollout);

    assert_eq!(
        codex_session_id_from_proc(&proc_root, 100, &project).unwrap(),
        None,
        "a stale children entry must not bind a PID now owned by another parent"
    );
}

#[test]
#[cfg(unix)]
fn codex_proc_probe_refuses_two_distinct_user_rollouts() {
    let dir = tempdir().unwrap();
    let project = std::fs::canonicalize({
        let path = dir.path().join("project");
        std::fs::create_dir_all(&path).unwrap();
        path
    })
    .unwrap();
    let proc_root = dir.path().join("proc");
    proc_node(&proc_root, 200, 0, &[]);
    let sessions = dir.path().join("codex/sessions/2026/08/25");
    for (fd, id) in [
        (10, "11111111-2222-4333-8444-555555555555"),
        (11, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
    ] {
        let rollout = sessions.join(format!("rollout-{fd}-{id}.jsonl"));
        write_rollout(&rollout, id, id, &project, "user");
        link_rollout_fd(&proc_root, 200, fd, &rollout);
    }

    let err = codex_session_id_from_proc(&proc_root, 200, &project).unwrap_err();
    assert!(
        err.to_string().contains("multiple user rollout ids"),
        "{err:#}"
    );
}
