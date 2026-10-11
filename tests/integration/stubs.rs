//! The fake engine binaries these tests put on `PATH` in place of a real `claude`,
//! and the code that installs them. They are one unit because each one's shape is
//! dictated by what the harness reads off its pane — a bare prompt to classify Idle,
//! a composer to accept a send, a dialog to refuse one — and because together they
//! are what keeps an acceptance run deterministic and free.

use std::path::Path;
use std::process::Command;

use crate::pmtui_fixture::EnterFixture;

/// A dual-mode `claude` stub. In production the SUPERVISOR consult (headless `-p`) and
/// the persistent WORKER REPL are the same binary, so they are the same stub here — which
/// also means the test cannot accidentally stub one and hit the real CLI for the other.
///
/// Supervisor mode echoes the nonce it was handed, exactly as a real supervisor must; the
/// test never injects a nonce, so a harness that stopped minting or checking one would
/// fail here rather than pass vacuously. Worker mode is an idle-prompt REPL that appends
/// everything typed at it to `@TYPED@`, so assertions read a FILE instead of guessing at
/// tmux's line wrapping.
const CLAUDE_STUB: &str = r#"#!/bin/sh
mode=worker
for a in "$@"; do
  case "$a" in -p) mode=sup ;; esac
  last="$a"
done
if [ "$mode" = sup ]; then
  nonce=`printf '%s\n' "$last" | sed -n 's/^NONCE: //p' | head -1`
  printf '%s\n' "$last" > '@PROMPT@'
  @REPLY@
  exit 0
fi
printf '> \n'
while IFS= read -r line; do
  printf '%s\n' "$line" >> '@TYPED@'
  printf '> \n'
done
"#;

/// The happy-path supervisor reply, emitted in the FULL `--output-format json` envelope
/// (`result` holding the model's answer as a JSON *string*) so the real production unwrap
/// path is exercised, not a convenient shortcut.
// NOTE the closing delimiter on its OWN LINE. Both shorter spellings silently corrupt the
// stub into a shell syntax error, which manifests as the worker pane "exiting immediately"
// rather than as anything resembling a quoting bug: with `r#"… "$nonce"#` the `"#` ends the
// literal, and with `r##"… "$nonce"##` the `"##` swallows the shell's closing quote too.
// Ending on a newline keeps the shell's quote inside the literal.
pub(crate) const REPLY_PICKS_DPRINT: &str = r##"printf '{"type":"result","subtype":"success","result":"{\\"nonce\\":\\"%s\\",\\"action\\":\\"select_option\\",\\"option_index\\":1,\\"reason\\":\\"the goal already vendors dprint\\"}"}\n' "$nonce"
"##;

/// A reply whose `reason` carries a REAL escape sequence once decoded: `[Z` is
/// shift+tab in a live pane, which CYCLES CLAUDE'S PERMISSION MODE. Emitted as a bare
/// object (the other shape the harness accepts), so between the two halves of this test
/// both parse paths are covered against real substrate.
pub(crate) const REPLY_WITH_ESCAPE: &str = r##"printf '{"nonce":"%s","action":"select_option","option_index":1,"reason":"\\u001b[Z pwned"}\n' "$nonce"
"##;

/// Write the executable dual-mode stub, plus a `tmux` wrapper that puts the stub dir on
/// PATH for every client the driver spawns.
///
/// Why the wrapper: MEASURED on a real server, a tmux pane's PATH is the **creating
/// client's** PATH — a session created by a client with a stubbed PATH sees the stub, one
/// created by a plain client does not, and `set-environment -g PATH` does NOT change that
/// (the server's global PATH held the stub dir and the pane still missed it). `TmuxDriver`
/// runs IN PROCESS here, so this process is the creating client; wrapping the `tmux`
/// binary (a `pub` field on the driver) injects PATH per client with no `unsafe`
/// `std::env::set_var`, which in edition 2024 would be process-global and unsound
/// alongside any other test.
pub(crate) fn write_stubs(
    dir: &Path,
    typed: &Path,
    prompt: &Path,
    reply: &str,
) -> (std::path::PathBuf, String) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    let script = CLAUDE_STUB
        .replace("@TYPED@", &typed.display().to_string())
        .replace("@PROMPT@", &prompt.display().to_string())
        .replace("@REPLY@", reply);
    if std::env::var("PM_TEST_TRACE").is_ok() {
        eprintln!("---- generated claude stub ----\n{script}\n---- end stub ----");
    }
    std::fs::write(&claude, &script).unwrap();
    let tmuxw = dir.join("tmuxw");
    std::fs::write(
        &tmuxw,
        format!(
            "#!/bin/sh\nPATH='{}':\"$PATH\" exec tmux \"$@\"\n",
            bin.display()
        ),
    )
    .unwrap();
    for f in [&claude, &tmuxw] {
        Command::new("chmod").arg("+x").arg(f).status().unwrap();
    }
    (bin, tmuxw.display().to_string())
}

/// A `claude` stub for the resume→create fallback acceptance test. It reproduces the
/// EMPIRICAL engine behaviour the fix is built on (verified against a real `claude`):
///   - `claude --resume <id>` on a conversation that was never persisted prints
///     "No conversation found with session ID: <id>" and **exits** — tmux then tears the
///     session down, which is the whole mechanism of the relaunch loop;
///   - `claude --session-id <id>` **creates** the conversation and comes up as an idle
///     REPL that STAYS ALIVE (a bare prompt + a `read` loop, so it classifies Idle and
///     never exits on its own).
///
/// The `sleep` on the resume arm is LOAD-BEARING, and modelling it is exactly what a
/// `FakeDriver` cannot: a real `claude --resume` first ESTABLISHES the tmux session (Node
/// startup) and only THEN prints the error and exits, so `launch_interactive`'s
/// immediate post-launch `is_alive` check sees it up (the launch succeeds and the ledger
/// records the seed adoption), and the death is caught on a LATER tick. A stub that exits
/// instantly instead trips the "exited immediately" launch-FAILURE guard, so the seed
/// adoption is never persisted and the fallback can never arm — a false negative that
/// hides behind an over-eager stub.
///
/// Every launch appends ONE line — `resume <id>` or `create <id>` — to `@LAUNCHES@`, so
/// the acceptance test reads a FILE to prove the one-shot fallback (resume → dies →
/// create → stays up) rather than racing tmux's teardown. The stub is what makes the
/// proof deterministic and FREE (no real tokens).
const RESUME_FALLBACK_STUB: &str = r#"#!/bin/sh
mode=create
id=""
prev=""
for a in "$@"; do
  case "$prev" in
    --resume) mode=resume; id="$a" ;;
    --session-id) mode=create; id="$a" ;;
  esac
  prev="$a"
done
if [ "$mode" = resume ]; then
  printf 'resume %s\n' "$id" >> '@LAUNCHES@'
  printf 'No conversation found with session ID: %s\n' "$id"
  sleep 2
  exit 1
fi
printf 'create %s\n' "$id" >> '@LAUNCHES@'
printf '> \n'
while IFS= read -r line; do
  printf '> \n'
done
"#;

/// Write the resume-aware stub plus the PATH-injecting `tmux` wrapper (see [`write_stubs`]
/// for why the in-process `TmuxDriver` needs the wrapper rather than `set-environment`).
/// Returns the stub bin dir and the wrapper path to point `TmuxDriver.tmux` at.
pub(crate) fn write_resume_fallback_stubs(
    dir: &Path,
    launches: &Path,
) -> (std::path::PathBuf, String) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    let script = RESUME_FALLBACK_STUB.replace("@LAUNCHES@", &launches.display().to_string());
    std::fs::write(&claude, &script).unwrap();
    let tmuxw = dir.join("tmuxw");
    std::fs::write(
        &tmuxw,
        format!(
            "#!/bin/sh\nPATH='{}':\"$PATH\" exec tmux \"$@\"\n",
            bin.display()
        ),
    )
    .unwrap();
    for f in [&claude, &tmuxw] {
        Command::new("chmod").arg("+x").arg(f).status().unwrap();
    }
    (bin, tmuxw.display().to_string())
}

/// A stub engine that DRAWS A CLAUDE-LIKE COMPOSER and echoes whatever it is sent.
///
/// The fixture default (`exec cat`) is deliberately not enough for `s`: with no input
/// prompt on screen the send is REFUSED, because `send_keys` ends with a separate `Enter`
/// and a pane that is not at a composer is a pane where that Enter selects something.
pub(crate) const ECHOING_COMPOSER_STUB: &str = "#!/bin/sh\n\
     prompt() { printf '\\342\\224\\200\\342\\224\\200\\342\\224\\200\\n\\342\\235\\257  \\n'; }\n\
     prompt\n\
     while IFS= read -r l; do printf 'ECHO %s\\n' \"$l\"; prompt; done\n";

/// A stub engine that puts a PERMISSION DIALOG on screen and reports anything it is sent.
///
/// `LEAKED` is the tripwire: if `s` ever writes here, the text shows up in the pane and
/// the trailing `Enter` would have confirmed the pre-highlighted `1. Yes`.
pub(crate) const DIALOG_STUB: &str = "#!/bin/sh\n\
     printf ' Do you want to create hello.txt?\\n \\342\\235\\257 1. Yes\\n   2. No\\n\\n Esc to cancel\\n'\n\
     while IFS= read -r l; do printf 'LEAKED %s\\n' \"$l\"; done\n";

pub(crate) fn write_stub(fx: &EnterFixture, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let stub = fx.dir.path().join("bin/claude");
    std::fs::write(&stub, body).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The `claude` stub for the nudge-counting tests, written into the fixture's `bin/claude`.
///
/// Three jobs, all load-bearing:
///   1. **Look Idle.** `tmux::classify_pane` only says `Idle` when a line in the pane tail
///      is a BARE PROMPT (`>`/`❯` then whitespace) — the fixture's default `exec cat` stub
///      never draws one, so a `cat` pane classifies Busy forever and NO nudge would ever
///      be sent. This prints `> ` at startup and again after every line it consumes, so a
///      bare prompt is always inside `classify_pane`'s 16-line window.
///   2. **Count nudges deterministically.** It appends one line to `<count_file>` for each
///      arriving nudge, recognised by the prompt's stable opening words. Counting a
///      substring in a `capture-pane` instead would be flaky by construction: the pasted
///      prompt is ~4KB and WRAPS at the pane width, so any short needle can straddle a
///      wrap boundary, and old copies scroll out of the capture window.
///   3. **REPORT, like a real agent.** Since m38 the harness will not nudge a session again until
///      the agent has written its marker with a higher `seq` (`job_engine`'s awaiting-report gate),
///      because a pane that merely LOOKS idle is not proof the agent has finished — user: *"When
///      the session is chatting or working on sth, and not idle, i don't want autopilot to kick in
///      and queue a prompt."* So a stub that never reported would (correctly) be nudged exactly
///      once, and every "the cadence repeats" assertion here would fail. Writing the marker is also
///      what the nudge prompt asks of a real agent at every decision point, so this makes the
///      fixture MORE faithful, not less: it now exercises nudge → report → nudge.
///
///      `seq` is the Unix time in seconds, exactly as the prompt instructs, and the write is
///      tmp+rename so the harness can never read a half-written marker.
pub(crate) fn nudge_counting_claude_stub(count_file: &Path, marker: &Path) -> String {
    format!(
        "#!/bin/sh\nprintf '> \\n'\nwhile IFS= read -r line; do\n  case \"$line\" in\n  \
         'You are a long-running agent'*)\n    echo nudge >> '{}'\n    \
         printf '{{\"seq\":%s,\"state\":\"working\",\"status\":\"stub working\"}}' \
         \"$(date +%s)\" > '{}.tmp' && mv '{}.tmp' '{}'\n    ;;\n  esac\n  \
         printf '> \\n'\ndone\n",
        count_file.display(),
        marker.display(),
        marker.display(),
        marker.display()
    )
}

/// The nudge-counting stub plus a real turn-hook tripwire. It appends to
/// `turn_signal` only when the launch argv actually carries that path in its
/// `--settings` value, then models Claude firing the hook after each handled turn.
pub(crate) fn nudge_counting_claude_with_turn_hook_stub(
    count_file: &Path,
    marker: &Path,
    turn_signal: &Path,
) -> String {
    format!(
        "#!/bin/sh\nhooked=0\nfor arg in \"$@\"; do\n  case \"$arg\" in\n  \
         *'{signal}'*) hooked=1 ;;\n  esac\ndone\nprintf '> \\n'\n\
         while IFS= read -r line; do\n  case \"$line\" in\n  \
         'You are a long-running agent'*)\n    echo nudge >> '{count}'\n    \
         printf '{{\"seq\":%s,\"state\":\"working\",\"status\":\"stub working\"}}' \
         \"$(date +%s)\" > '{marker}.tmp' && mv '{marker}.tmp' '{marker}'\n    \
         if [ \"$hooked\" = 1 ]; then mkdir -p '{signal_dir}' && printf . >> '{signal}'; fi\n    \
         ;;\n  esac\n  printf '> \\n'\ndone\n",
        count = count_file.display(),
        marker = marker.display(),
        signal = turn_signal.display(),
        signal_dir = turn_signal.parent().unwrap().display(),
    )
}

/// How many nudges the stub has recorded so far.
pub(crate) fn nudge_count(count_file: &Path) -> usize {
    std::fs::read_to_string(count_file)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// The `claude` stub for the next_step-echo acceptance test (Task 9) — a sibling of
/// [`nudge_counting_claude_stub`] with the same idle-prompt REPL shape, extended for the
/// one thing this proof needs: it REPORTS a `next_step`, and TEES what it receives.
///
///   1. **Report a `next_step`.** On every nudge — a line opening with the harness's stable
///      `You are a long-running agent` — it OVERWRITES `marker` (the session's
///      `needs-you.json`) with a `WakeReport` carrying `state:"working"`, a monotonic `seq`
///      (`date +%s`, exactly as the prompt instructs), and a FIXED `next_step` (`marker_text`).
///      Written tmp+rename so the harness never reads a half-written file. The marker disposer
///      mirrors that `next_step` into `last_plan`, and the NEXT nudge quotes `last_plan` back
///      verbatim (`loop_nudge_prompt`'s "Your next step, in your words:" line).
///   2. **Tee what it RECEIVED.** Every line the pane is sent is appended to `typed`, so the
///      test asserts on a FILE (the bytes the stub actually received) rather than on tmux's
///      line-wrapping. Crucially, the stub writes `marker_text` only to `needs-you.json`,
///      NEVER to `typed` — so `marker_text` can reach `typed` ONLY if the harness typed it
///      back, which is what makes the assertion NON-VACUOUS: were the mirror feature broken,
///      `last_plan` would stay empty, the second nudge would omit the line, and `typed` would
///      never contain the marker.
///
/// Determinism note: the second nudge cannot fire until the `awaiting_report` gate clears,
/// which REQUIRES this marker to have been disposed first (its `seq` bumps `last_marker_seq`),
/// and disposal is exactly what sets `last_plan`. So the echoing nudge always follows the
/// report — there is no window in which nudge #2 races ahead of the mirror.
pub(crate) fn next_step_reporting_claude_stub(
    typed: &Path,
    marker: &Path,
    marker_text: &str,
) -> String {
    format!(
        "#!/bin/sh\nprintf '> \\n'\nwhile IFS= read -r line; do\n  \
         printf '%s\\n' \"$line\" >> '{typed}'\n  case \"$line\" in\n  \
         'You are a long-running agent'*)\n    \
         printf '{{\"seq\":%s,\"state\":\"working\",\"status\":\"stub working\",\"next_step\":\"{step}\"}}' \
         \"$(date +%s)\" > '{marker}.tmp' && mv '{marker}.tmp' '{marker}'\n    ;;\n  esac\n  \
         printf '> \\n'\ndone\n",
        typed = typed.display(),
        marker = marker.display(),
        step = marker_text,
    )
}

/// Write `body` as an executable `claude` into `<dir>/bin` and return that bin directory, so
/// a caller can prepend it to a spawned `pmd`'s `PATH`.
///
/// Unlike [`write_stubs`], this needs no `tmux` wrapper: a tmux pane inherits its CREATING
/// client's PATH, and every `tmux` a standalone `pmd` runs is a child of that `pmd`, so a
/// `pmd` launched with the stub dir on its own PATH makes the panes it creates resolve
/// `claude` to the stub — no per-client `set-environment` and no `unsafe` process-global
/// `std::env::set_var` required.
pub(crate) fn install_claude_stub(dir: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, body).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}
