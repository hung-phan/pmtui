//! The real driver: every tmux operation as a `tmux(1)` subprocess against a
//! private server socket. The shell-quoting, the exit-code wrapper script and the
//! exact-match target anchors sit here with it because they exist only to build
//! those command lines — and getting an anchor's form wrong is how every nudge
//! silently failed once already.

use anyhow::{Context, Result, bail};
use ratatui::style::Modifier;
use ratatui::text::Line;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::clock::Epoch;

use super::dialog_keys;
use super::driver::{Driver, StepHandle};
use super::launch::{LaunchError, LaunchOutcome, ManagedEnv};

const CODEX_DRAFT_MATCH_CHARS: usize = 32;

/// Whether the bottom-most Codex composer still contains the payload we just sent, read from a
/// styled (`capture-pane -e`) capture.
///
/// Codex echoes submitted prompts with the same `›` lead, so presence alone is not enough. The
/// live composer is the LAST such row: while pending it contains either Codex's collapsed-paste
/// label or the payload; after submission it returns to a rotating placeholder, drawn dim where
/// typed text is plain. A dim draft is therefore the placeholder, whatever its words.
///
/// A bracketed paste (`pasted`) may show only a line's leading part, so any payload line's prefix
/// counts. A literal send is one line, which Codex wraps onto indented continuation rows when it
/// is wider than the composer; the draft is those rows up to the blank row above the footer. It is
/// pending only while that draft is the whole message, compared without whitespace because a wrap
/// drops the space at a word break and breaks a long word with none. A prefix is not enough there:
/// a message such as `Write tests` is a prefix of the placeholder `Write tests for @filename`,
/// which on a capture without styles would read an accepted message as stuck and press Enter on an
/// empty composer until the send failed.
pub(super) fn codex_draft_visible(capture: &str, text: &str, pasted: bool) -> bool {
    let lines = crate::ansi::styled_lines(capture);
    let rows: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let Some(at) = rows
        .iter()
        .rposition(|row| row.trim_start().starts_with('›'))
    else {
        return false;
    };
    let draft = rows[at]
        .trim()
        .strip_prefix('›')
        .map(str::trim)
        .unwrap_or_default();
    if draft.starts_with("[Pasted Content ") {
        return true;
    }
    if placeholder_drawn(&lines[at]) {
        return false;
    }
    if !pasted {
        let continuation = rows[at + 1..]
            .iter()
            .take_while(|row| row.starts_with(' ') && !row.trim().is_empty());
        let shown: String = std::iter::once(draft)
            .chain(continuation.map(String::as_str))
            .flat_map(str::chars)
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let wanted: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        return !wanted.is_empty() && shown == wanted;
    }
    text.lines().map(str::trim).any(|line| {
        let prefix: String = line.chars().take(CODEX_DRAFT_MATCH_CHARS).collect();
        !prefix.is_empty() && draft.starts_with(&prefix)
    })
}

/// Whether the composer row's first visible character after its `›` lead is drawn dim, the style
/// Codex gives its placeholder and never typed text.
fn placeholder_drawn(row: &Line<'_>) -> bool {
    row.spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |ch| (ch, span.style)))
        .skip_while(|(ch, _)| ch.is_whitespace())
        .skip(1)
        .find(|(ch, _)| !ch.is_whitespace())
        .is_some_and(|(_, style)| style.add_modifier.contains(Modifier::DIM))
}

/// POSIX single-quote escaping so arbitrary argv/paths survive the shell.
pub(crate) fn shq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The largest shell command one interactive launch may hand `tmux new-session`, in bytes
/// after shell quoting.
///
/// The tmux client sends a command's whole argument list to the server as ONE message, capped
/// by tmux's 16 KiB `MAX_IMSGSIZE` less the message header. Measured on tmux 3.6a: a
/// `new-session` whose shell command was 16,300 bytes launched, and 16,330 bytes failed with
/// "failed to send command" (larger ones print "command too long"). That same message also
/// carries the session name, the working directory, the size flags and the managed session env
/// (`-e` pairs naming the session id, its state directory and pmtui), so 12 KiB leaves about
/// 4 KiB for them: room for a working directory a kilobyte or so deep, since the state
/// directory repeats it.
pub const LAUNCH_COMMAND_MAX_BYTES: usize = 12 * 1024;

/// The shell command [`TmuxDriver`] runs for an interactive `argv`: every argument
/// single-quoted ([`shq`]) and space-joined. [`LAUNCH_COMMAND_MAX_BYTES`] bounds its length.
pub fn launch_command(argv: &[String]) -> String {
    argv.iter().map(|a| shq(a)).collect::<Vec<_>>().join(" ")
}

/// The size a **detached** agent pane is created at (`new-session -x/-y`), so pmtui's
/// preview shows a full-width claude/codex instead of tmux's 80x24 default. A detached
/// session with no attached client uses tmux's `default-size`, and `capture-pane` returns
/// exactly that — which is why the preview under-filled its panel. Tunable per run via
/// `PM_AGENT_COLS` / `PM_AGENT_ROWS`; the default fills a wide/maximised terminal.
///
/// Deliberately NOT `window-size manual`: `-x/-y` sets only the INITIAL detached size, so a
/// human who attaches (pmtui's `Enter`) still gets tmux's resize-to-my-terminal — verified
/// `window-size` stays `latest`. The one trade-off is that the preview renders WITHOUT wrap
/// (one capture line = one screen row), so a pane WIDER than the preview clips its right
/// edge: lower `PM_AGENT_COLS` toward your preview width if claude looks cropped, raise it if
/// it under-fills.
/// The binary a launch argv actually runs: `argv[0]`, or, behind an `env` prefix (Claude launches
/// as `env -u CLAUDECODE claude …`), the first word after its `-u NAME` and `NAME=VALUE`
/// arguments. `env` is always on PATH, so checking it would never report a missing agent. An
/// `env` with nothing after its arguments runs `env` itself.
pub(super) fn launched_binary(argv: &[String]) -> &str {
    let first = argv.first().map_or("", String::as_str);
    if Path::new(first).file_name() != Some(std::ffi::OsStr::new("env")) {
        return first;
    }
    let mut rest = argv[1..].iter();
    while let Some(word) = rest.next() {
        if word == "-u" {
            if rest.next().is_none() {
                break;
            }
        } else if !word.contains('=') {
            return word;
        }
    }
    first
}

/// Whether `bin` names an executable a pane could run, searching `path` when one is given and
/// this process's own `PATH` otherwise. A walk rather than `sh -c 'command -v'`, because the
/// search PATH and the PATH that finds `sh` are not the same PATH once the caller supplies one.
pub(super) fn resolves_on_path(bin: &str, path: Option<&str>) -> bool {
    if bin.contains('/') {
        return executable_file(Path::new(bin));
    }
    let search = match path {
        Some(path) => std::ffi::OsString::from(path),
        None => match std::env::var_os("PATH") {
            Some(path) => path,
            None => return false,
        },
    };
    std::env::split_paths(&search).any(|dir| executable_file(&dir.join(bin)))
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    path.is_file()
}

pub(super) fn pane_dimension(value: Option<&str>, dflt: u16, floor: u16) -> u16 {
    value
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value >= floor)
        .unwrap_or(dflt)
}

fn agent_pane_size() -> (u16, u16) {
    const DEFAULT_COLS: u16 = 200;
    const DEFAULT_ROWS: u16 = 50;
    let cols = std::env::var("PM_AGENT_COLS").ok();
    let rows = std::env::var("PM_AGENT_ROWS").ok();
    (
        pane_dimension(cols.as_deref(), DEFAULT_COLS, 20),
        pane_dimension(rows.as_deref(), DEFAULT_ROWS, 5),
    )
}

/// The wrapper that always records the child's exit code atomically, even when
/// the child calls `exit N` (the child runs as a separate process).
///
/// The worker's combined output is `tee`'d so it lands **both** in `{log}` (read
/// by `capture_codex_session_id_from_log`) and on the tmux pane (tee's stdout),
/// letting a human `tmux attach` and watch a live wake. The exit code is still
/// the COMMAND's, never `tee`'s: `echo $? >{done}.code` runs immediately after
/// `{cmd}` *inside* the group, so it captures `{cmd}`'s status before `tee` (the
/// pipe's last stage) can overwrite `$?`. That echo is redirected to a file, so
/// it never leaks into the log/pane. The final commit keeps the ENOSPC guard: if
/// `{done}.code` couldn't be written, `code` is empty → `printf '%s' ""` → empty
/// signal → the daemon treats it as an orphan/timeout (same as today).
pub(super) fn wrapper_script(command: &[String], done: &Path, log: &Path) -> String {
    let cmd = command.iter().map(|a| shq(a)).collect::<Vec<_>>().join(" ");
    let done_q = shq(&done.to_string_lossy());
    let log_q = shq(&log.to_string_lossy());
    // Dependency note: the pane-tee adds a runtime dependency on `tee` (and `cat`).
    // If `tee` is absent or dies early, the piped command hits SIGPIPE and every
    // worker reports exit 141 (Failed). Both are coreutils/busybox-standard, so the
    // risk is low, but it's flagged here since this is the daemon's most load-bearing
    // snippet.
    format!(
        "#!/bin/sh\n\
         {{ {cmd}; echo $? >{done_q}.code; }} 2>&1 | tee {log_q}\n\
         code=$(cat {done_q}.code 2>/dev/null)\n\
         rm -f {done_q}.code\n\
         printf '%s' \"$code\" >{done_q}.tmp && mv -f {done_q}.tmp {done_q}\n"
    )
}

/// tmux resolves `-t` by exact match, then prefix match. Session names are
/// `pmd-<id>-<seq>`, so `pmd-web-2` would prefix-match a *different* project's
/// live `pmd-web-2-0` — masking a crash and cross-killing a sibling. Prefixing
/// the target with `=` forces exact match. (`new-session -s` needs no anchor.)
pub(super) fn exact(session: &str) -> String {
    format!("={session}")
}

/// The same exact-match anchor for the subcommands that take a **pane** target.
///
/// tmux rejects the bare `=<name>` form for a pane target (`can't find pane:
/// =<name>`, measured on tmux 3.6a), because `=` anchors a *session* name and a
/// pane target wants `session:window.pane`. `=<name>:` — exact session, its
/// active window/pane — is accepted and still refuses prefix matches, so it
/// keeps the anti-cross-talk guarantee `exact()` exists for.
///
/// Which form each subcommand we use needs:
/// - PANE form (`=<name>:`): `send-keys`, `paste-buffer`, `capture-pane`.
/// - session form (`=<name>`): `has-session`, `list-clients`, `display-message`,
///   `kill-session`, `attach-session`.
///
/// Getting this wrong is silent-ish and expensive: every `send_keys` returned
/// `Err`, `JobScheduler::nudge` read that as a transient error and re-parked
/// without consuming a continuation, so an idle agent was never actually typed
/// into and autopilot made no progress at all.
pub(super) fn exact_pane(session: &str) -> String {
    format!("={session}:")
}

/// A tmux I/O failure during an interactive launch, with its whole cause chain. Ambiguous by
/// construction: once liveness cannot be read, nothing proves whether our argv started.
fn probe_error(error: anyhow::Error) -> LaunchError {
    LaunchError::Probe(format!("{error:#}"))
}

/// Real driver: shells out to the `tmux` binary. `socket` pins a private tmux
/// server (`tmux -L <socket>`) so the daemon never collides with the user's
/// sessions and "no server running" is a normal, non-error state.
#[derive(Debug, Clone)]
pub struct TmuxDriver {
    pub tmux: String,
    pub socket: Option<String>,
}

impl Default for TmuxDriver {
    fn default() -> Self {
        Self {
            tmux: "tmux".to_string(),
            socket: None,
        }
    }
}

impl TmuxDriver {
    /// Driver pinned to a private tmux server socket (`tmux -L <socket>`).
    pub fn with_socket(socket: impl Into<String>) -> Self {
        Self {
            tmux: "tmux".to_string(),
            socket: Some(socket.into()),
        }
    }

    /// A `tmux [-L <socket>]` command with the server socket applied.
    ///
    /// Deliberately does NOT pass `-f`: driver servers source the user's own `~/.tmux.conf`
    /// so an attach looks and behaves exactly like their normal tmux. (A headless
    /// `new-session -d` whose config runs a plugin manager can wedge; that is handled after
    /// the fact by the orphan sweep + leftover cleanup rather than by overriding the config.)
    fn base(&self) -> Command {
        let mut c = Command::new(&self.tmux);
        if let Some(s) = &self.socket {
            c.arg("-L").arg(s);
        }
        c
    }

    fn pane_current_command(&self, session: &str) -> Result<Option<String>> {
        let out = self
            .base()
            .arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(exact_pane(session))
            .arg("#{pane_current_command}")
            .output()
            .context("run tmux display-message #{pane_current_command}")?;
        if !out.status.success() {
            return Ok(None);
        }
        let command = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!command.is_empty()).then_some(command))
    }

    pub(super) fn send_dialog_key_names(&self, session: &str, keys: &[&str]) -> Result<()> {
        if keys.is_empty() || keys.len() > 64 {
            bail!(
                "invalid dialog key count {} for session {session}",
                keys.len()
            );
        }
        if self.pane_in_mode(session)? {
            bail!("cannot select a dialog while session {session} is in a tmux mode");
        }
        let target = exact_pane(session);
        let status = self
            .base()
            .args(["send-keys", "-t", &target])
            .args(keys)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux send-keys (dialog keys)")?;
        if !status.success() {
            bail!("tmux dialog keys failed for session {session}");
        }
        Ok(())
    }

    /// Every session name, or empty when the server is unreachable.
    pub fn list_sessions(&self) -> Vec<String> {
        let out = self
            .base()
            .arg("list-sessions")
            .arg("-F")
            .arg("#{session_name}")
            .output();
        match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_owned)
                .collect(),
            // No server, no sessions, or tmux unrunnable — all "nothing to reap" here.
            _ => Vec::new(),
        }
    }

    /// The socket name this driver is pinned to (for recording in `session.json`).
    pub fn socket(&self) -> Option<&str> {
        self.socket.as_deref()
    }

    /// Bind bare `Ctrl+q` to detach on this private server. Server-global,
    /// idempotent and best-effort; the normal prefix-d fallback remains.
    pub fn ensure_detach_key(&self) {
        let _ = self
            .base()
            .arg("bind-key")
            .arg("-n")
            .arg("C-q")
            .arg("detach-client")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Whether `bin` is resolvable on this process's PATH.
    fn binary_on_path(&self, bin: &str) -> bool {
        resolves_on_path(bin, None)
    }

    /// The `PATH` panes on this socket search, or `None` when nothing can be read: no server is
    /// running yet, or its global environment names no `PATH`.
    ///
    /// A pane inherits the tmux SERVER's global environment, which belongs to whichever client
    /// first started that server. That is not always ours: `self.tmux` may be a wrapper that
    /// exports its own `PATH` before exec'ing tmux, which is exactly how the acceptance tests put
    /// a stub engine in front of a pane. So a running server's `PATH` is the only authority.
    fn server_path(&self) -> Option<String> {
        let out = self
            .base()
            .arg("show-environment")
            .arg("-g")
            .arg("PATH")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("PATH=").map(str::to_string))
    }

    /// Whether a pane on this socket could run `bin`. `true` whenever that is unprovable, because
    /// refusing a launch the pane would have run is worse than reporting its exit afterwards.
    fn pane_resolves(&self, bin: &str) -> bool {
        match self.server_path() {
            Some(path) => resolves_on_path(bin, Some(&path)),
            None => true,
        }
    }

    /// A foreground `tmux attach-session` command for `session`. The caller runs
    /// it with the terminal's real stdio (after suspending the TUI) so the user
    /// gets the normal claude/codex terminal; on detach control returns. Detach
    /// with a bare `Ctrl+q` (bound in `launch_interactive`), or the built-in
    /// `prefix d` fallback — both leave the session (and its agent) running.
    pub fn attach_command(&self, session: &str) -> Command {
        let mut c = self.base();
        c.arg("attach-session").arg("-t").arg(exact(session));
        c
    }
}

impl Driver for TmuxDriver {
    fn spawn_step(
        &self,
        session: &str,
        cwd: &Path,
        command: &[String],
        done_signal: &Path,
        log: &Path,
    ) -> Result<StepHandle> {
        if command.is_empty() {
            bail!("spawn_step: empty command");
        }
        let signal_dir = done_signal
            .parent()
            .context("done_signal has no parent directory")?;
        std::fs::create_dir_all(signal_dir)?;
        if let Some(p) = log.parent() {
            std::fs::create_dir_all(p)?;
        }
        // Clear any stale signal so we never read a previous run's exit code.
        match std::fs::remove_file(done_signal) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("clear stale done-signal"),
        }
        let wrapper = signal_dir.join(format!("{session}.run.sh"));
        std::fs::write(&wrapper, wrapper_script(command, done_signal, log))
            .with_context(|| format!("write wrapper {}", wrapper.display()))?;
        let cmd_str = format!("sh {}", shq(&wrapper.to_string_lossy()));
        let status = self
            .base()
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(session)
            .arg("-c")
            .arg(cwd)
            .arg(&cmd_str)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux new-session")?;
        if !status.success() {
            bail!("tmux new-session failed for session {session}");
        }
        Ok(StepHandle {
            session: session.to_string(),
            done_signal: done_signal.to_path_buf(),
            log: log.to_path_buf(),
        })
    }

    /// Launch a **persistent, interactive** session running `argv` in `cwd`
    /// (no exit-code wrapper — the human drives it and attaches later), with every
    /// [`ManagedEnv::vars`] pair applied as `new-session -e KEY=VALUE`. Idempotent:
    /// if the session is already alive it is left as-is ([`LaunchOutcome::AlreadyAlive`]).
    ///
    /// Fails with an actionable error when the engine binary isn't on PATH, and
    /// re-checks liveness after creating the session so an engine that exits
    /// immediately (e.g. a crash on startup) surfaces instead of a silent no-op. Each
    /// failure is classified by whether tmux had already been handed our argv
    /// ([`LaunchError::proven_not_started`]).
    fn launch_interactive(
        &self,
        session: &str,
        cwd: &Path,
        argv: &[String],
        env: &ManagedEnv,
    ) -> std::result::Result<LaunchOutcome, LaunchError> {
        if argv.is_empty() {
            return Err(LaunchError::EmptyArgv);
        }
        if self.is_alive(session).map_err(probe_error)? {
            self.ensure_detach_key();
            return Ok(LaunchOutcome::AlreadyAlive);
        }
        let bin = launched_binary(argv);
        // Two lookups, because ours is the cheap one and is right whenever pmtui/pmd started the
        // server itself. A pane on a server somebody else started searches THAT server's PATH, so
        // only its answer may refuse the launch.
        if !self.binary_on_path(bin) && !self.pane_resolves(bin) {
            return Err(LaunchError::NotOnPath(bin.to_string()));
        }
        let cmd_str = launch_command(argv);
        // Refused here with the reason, rather than as tmux's bare exit 1 (its own "command too
        // long" goes to the stderr this driver discards). The budget bounds the command string
        // only, as before; the `-e` pairs share the headroom left for the working directory.
        if cmd_str.len() > LAUNCH_COMMAND_MAX_BYTES {
            return Err(LaunchError::CommandTooLong {
                session: session.to_string(),
                bytes: cmd_str.len(),
            });
        }
        // Size the DETACHED pane up front so the dashboard preview shows a full-width agent
        // (tmux's default is 80x24 — see `agent_pane_size`). `window-size` stays `latest`, so
        // attaching later still resizes to the human's real terminal.
        let (cols, rows) = agent_pane_size();
        let mut command = self.base();
        command
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(session)
            .arg("-c")
            .arg(cwd)
            .arg("-x")
            .arg(cols.to_string())
            .arg("-y")
            .arg(rows.to_string());
        // The session's own environment (tmux 3.0+), so the agent knows which managed session
        // it is in without the driver guessing it.
        for (key, value) in env.vars() {
            command.arg("-e").arg(format!("{key}={value}"));
        }
        let status = command
            .arg(&cmd_str)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux new-session (interactive)")
            .map_err(probe_error)?;
        if !status.success() {
            // A concurrent creator (another pmtui on the same socket) may have won
            // the race: if the session now exists, that's fine — attach to it.
            if self.is_alive(session).map_err(probe_error)? {
                self.ensure_detach_key();
                return Ok(LaunchOutcome::AlreadyAlive);
            }
            return Err(LaunchError::NewSessionFailed(format!(
                "tmux new-session failed for interactive session {session}"
            )));
        }
        // The command runs inside the pane; a missing/crashing engine tears the
        // (only) pane down immediately, destroying the session. Catch that here.
        if !self.is_alive(session).map_err(probe_error)? {
            // The server exists now, so its `PATH` is finally readable. A pane that could never
            // have resolved the engine never ran our argv, which is a PROVEN pre-start failure
            // rather than the ambiguous "it started and died".
            if !self.pane_resolves(bin) {
                return Err(LaunchError::NotOnPath(bin.to_string()));
            }
            return Err(LaunchError::ExitedAfterStart(format!(
                "interactive session {session} exited immediately (did {bin:?} fail to start?)"
            )));
        }
        self.ensure_detach_key();
        Ok(LaunchOutcome::Started)
    }

    fn ensure_detach_key(&self) {
        TmuxDriver::ensure_detach_key(self);
    }

    fn attach_interactive(&self, session: &str) -> Result<()> {
        self.attach_command(session)
            .status()
            .with_context(|| format!("attach tmux session {session}"))?;
        Ok(())
    }

    fn codex_session_id(&self, session: &str, cwd: &Path) -> Result<Option<String>> {
        #[cfg(target_os = "linux")]
        {
            let out = self
                .base()
                .arg("display-message")
                .arg("-p")
                .arg("-t")
                .arg(exact_pane(session))
                .arg("#{pane_pid}")
                .output()
                .context("run tmux display-message #{pane_pid}")?;
            if !out.status.success() {
                bail!("cannot inspect Codex process: tmux session {session} is unavailable");
            }
            let raw = String::from_utf8_lossy(&out.stdout);
            let pane_pid = raw.trim().parse::<u32>().with_context(|| {
                format!("invalid pane pid {:?} for session {session}", raw.trim())
            })?;
            super::codex::session_id_from_proc(Path::new("/proc"), pane_pid, cwd)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (session, cwd);
            bail!("exact Codex session discovery requires Linux /proc")
        }
    }

    fn resize_window(&self, session: &str, cols: u16, rows: u16) -> Result<()> {
        // Best-effort: a dead/missing session (or a server that just went away) is not an
        // error worth propagating — the pane is fit again the next time it exists. Only a
        // failure to SPAWN tmux at all bubbles up. `resize-window` sets `window-size manual`
        // as a side effect; `set_window_size_auto` undoes that before an attach.
        let _ = self
            .base()
            .arg("resize-window")
            .arg("-t")
            .arg(exact(session))
            .arg("-x")
            .arg(cols.to_string())
            .arg("-y")
            .arg(rows.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux resize-window")?;
        Ok(())
    }

    fn set_window_size_auto(&self, session: &str) -> Result<()> {
        // `window-size` is a SESSION option, and set-option rejects the `=name` exact anchor
        // for its target ("no such window: =name") — unlike resize-window/has-session which
        // take it. So target the session by its plain (full, unique) name here.
        let _ = self
            .base()
            .arg("set-option")
            .arg("-t")
            .arg(session)
            .arg("window-size")
            .arg("latest")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux set-option window-size")?;
        Ok(())
    }

    fn clear_history(&self, session: &str) -> Result<()> {
        // A PANE command, so the `=name:` pane anchor (`exact_pane`) — same target form as
        // capture-pane. Best-effort: a dead/missing session is not worth an error.
        let _ = self
            .base()
            .arg("clear-history")
            .arg("-t")
            .arg(exact_pane(session))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux clear-history")?;
        Ok(())
    }

    fn is_alive(&self, session: &str) -> Result<bool> {
        let status = self
            .base()
            .arg("has-session")
            .arg("-t")
            .arg(exact(session))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run tmux has-session")?;
        Ok(status.success())
    }

    fn capture_tail(&self, session: &str, lines: usize) -> Result<String> {
        let out = self
            .base()
            .arg("capture-pane")
            // capture-pane takes a *pane* target, so it needs the `=name:` form —
            // see `exact_pane` for why the bare `=name` anchor is rejected here.
            .arg("-t")
            .arg(exact_pane(session))
            .arg("-p")
            .arg("-S")
            .arg(format!("-{lines}"))
            .output()
            .context("run tmux capture-pane")?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            // Session likely gone — diagnostics only, so empty is fine.
            Ok(String::new())
        }
    }

    /// Deliberately a FULL COPY of [`TmuxDriver::capture_tail`]'s body plus `-e`, rather
    /// than both delegating to a shared helper: `capture_tail` is load-bearing for pane
    /// classification (see the trait docs) and stays byte-for-byte untouched here, so
    /// nobody reading a diff has to reason about whether the classifiers changed.
    ///
    /// Without `-e`, tmux strips EVERY escape sequence from the capture — which is why
    /// the preview rendered flat white text.
    fn capture_tail_styled(&self, session: &str, lines: usize) -> Result<String> {
        let out = self
            .base()
            .arg("capture-pane")
            // capture-pane takes a *pane* target, so it needs the `=name:` form —
            // see `exact_pane` for why the bare `=name` anchor is rejected here.
            .arg("-t")
            .arg(exact_pane(session))
            .arg("-p")
            // `-e`: keep the cell grid's colours/attributes as SGR escapes.
            .arg("-e")
            .arg("-S")
            .arg(format!("-{lines}"))
            .output()
            .context("run tmux capture-pane -e")?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            // Session likely gone — the render path degrades to its next source.
            Ok(String::new())
        }
    }

    /// SIGTERM to the pane's process GROUP, not just its pid: the pane runs the step wrapper (`sh`),
    /// and the agent is its child, so signalling the group is what reaches the agent itself. The
    /// terminal is left alone — the caller kills it on a later frame if this was not enough.
    fn request_stop(&self, session: &str) -> Result<()> {
        let out = self
            .base()
            .arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(exact_pane(session))
            .arg("#{pane_pid}")
            .output()
            .context("run tmux display-message #{pane_pid}")?;
        if !out.status.success() {
            // No pane means nothing to stop, which is the outcome the caller wanted.
            return Ok(());
        }
        let raw = String::from_utf8_lossy(&out.stdout);
        let Some(pid) = raw
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|pid| *pid > 1)
            .and_then(rustix::process::Pid::from_raw)
        else {
            return Ok(());
        };
        match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
            Ok(()) => Ok(()),
            // Gone between the query and the signal: the job stopped on its own.
            Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(error) => bail!("SIGTERM to {session}'s process group ({pid:?}): {error}"),
        }
    }

    fn terminate(&self, session: &str) -> Result<()> {
        let out = self
            .base()
            .arg("kill-session")
            .arg("-t")
            .arg(exact(session))
            .output()
            .context("run tmux kill-session")?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("no server running")
            || stderr.contains("can't find session")
            || stderr.contains("no sessions")
        {
            return Ok(());
        }
        bail!("tmux kill-session failed for {session}: {}", stderr.trim())
    }

    /// Short single-line text is sent literally (`send-keys -l -- <text>`); multiline or large text
    /// (> ~800 bytes) is staged through a tmux paste-buffer (`load-buffer -` +
    /// `paste-buffer -p -r -d`) so bracketed-paste-aware agents receive one paste event and embedded
    /// newlines remain LF instead of becoming Enter-like CR bytes. The literal text and submitting
    /// `Enter` are separate calls with a short guard. Codex may suppress Enter while a paste burst
    /// settles or keep a large payload as `[Pasted Content …]`. For a paste into a Codex pane, the
    /// driver captures the composer after each Enter and retries only while the same draft remains
    /// visible. The bounded verification stops as soon as the composer clears or work starts, so an
    /// extra Enter can never spill into a newly rendered dialog. Each tmux call's exit status is
    /// checked; a dead session ⇒ `Err`.
    ///
    /// All three targeted invocations (`send-keys -l`, `paste-buffer`, `send-keys Enter`) address a
    /// PANE, so they take [`exact_pane`]'s `=name:` form — the bare `=name` anchor is rejected with
    /// `can't find pane`, which made every nudge fail.
    ///
    /// A pane found in COPY-MODE is taken out of it first ([`Driver::pane_in_mode`] + `-X cancel`),
    /// because copy-mode swallows the submitting Enter while `paste-buffer` still reports success —
    /// the silent-payload-loss bug that made a parked human answer disappear.
    fn send_keys(&self, session: &str, text: &str) -> Result<()> {
        let target = exact_pane(session);
        // A pane left in COPY-MODE eats the nudge (see `Driver::pane_in_mode` for the
        // measurements), and the paste path does it while still returning `Ok` — so leave
        // the mode BEFORE either branch runs.
        //
        // Cancelling unattended is safe: the drive path defers entirely while a human is
        // attached (`has_clients`), so a pane in a mode with NOBODY attached is leftover
        // state from a scrollback someone detached out of, never a live scroll we would be
        // yanking away from them.
        //
        // Best-effort in both directions, and deliberately so. A probe error is read as
        // "not in a mode" (fail-safe, matching the trait default) rather than failing the
        // nudge, and the cancel's OWN status is ignored: measured on tmux 3.6a, `-X cancel`
        // against a pane that is not in a mode exits 1 with `not in a mode` and is
        // otherwise harmless, which is not a send failure and must not be reported as one.
        //
        // Cost is one extra tmux call per NUDGE (not per sweep tick — `send_keys` only runs
        // when the harness has already decided to type), plus the cancel itself in the rare
        // leftover-mode case.
        if self.pane_in_mode(session).unwrap_or(false) {
            let _ = self
                .base()
                .arg("send-keys")
                .arg("-t")
                .arg(&target)
                .arg("-X")
                .arg("cancel")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        // Threshold below which literal `send-keys -l` is safe; above it (or with any newline) we route
        // through a paste-buffer so a long/multiline nudge isn't auto-submitted line-by-line.
        const LITERAL_MAX: usize = 800;
        const SUBMIT_GUARD: Duration = Duration::from_millis(100);
        const CODEX_PASTE_GUARD: Duration = Duration::from_millis(200);
        const CODEX_SUBMIT_RECHECK_GUARD: Duration = Duration::from_millis(300);
        const CODEX_SUBMIT_ATTEMPTS: usize = 3;
        let uses_paste_buffer = text.contains('\n') || text.len() > LITERAL_MAX;
        let codex = self.pane_current_command(session)?.as_deref() == Some("codex");
        let codex_paste = uses_paste_buffer && codex;
        if !uses_paste_buffer {
            // `-l` sends the text literally (no key-name interpretation); `--` guards a leading dash.
            let status = self
                .base()
                .arg("send-keys")
                .arg("-t")
                .arg(&target)
                .arg("-l")
                .arg("--")
                .arg(text)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("run tmux send-keys (literal)")?;
            if !status.success() {
                bail!("tmux send-keys (literal) failed for session {session}");
            }
        } else {
            // Route a long/multiline nudge through a NAMED paste buffer, not tmux's server-global
            // unnamed buffer stack. `load-buffer -` / `paste-buffer` with no `-b` both address that
            // one shared stack, so two sends racing on the same `--socket` server — pmd nudging
            // project A while pmtui `s`-sends project B — can interleave (A.load, B.load, A.paste,
            // B.paste) and SILENTLY CROSS-DELIVER: A's pane gets B's instructions. Nothing
            // serializes them (the per-project chat_lock and per-session driver.lock don't span
            // projects). A per-send buffer name — pid + a process-local counter, and pmd/pmtui are
            // different pids on the shared socket — keeps each send addressing only its own buffer.
            static SEND_SEQ: AtomicU64 = AtomicU64::new(0);
            let buf = format!(
                "pm-{}-{}",
                std::process::id(),
                SEND_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            let mut child = self
                .base()
                .arg("load-buffer")
                .arg("-b")
                .arg(&buf)
                .arg("-")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("spawn tmux load-buffer")?;
            child
                .stdin
                .take()
                .context("tmux load-buffer stdin unavailable")?
                .write_all(text.as_bytes())
                .context("write text to tmux load-buffer")?;
            let status = child.wait().context("run tmux load-buffer")?;
            if !status.success() {
                bail!("tmux load-buffer failed for session {session}");
            }
            // `-b <buf>` pastes THIS send's buffer (not "the most recent"); `-d` deletes it after.
            let status = self
                .base()
                .arg("paste-buffer")
                // Ask tmux to wrap the payload when the pane enabled bracketed paste mode. Without
                // this, Codex receives a long multiline nudge as a stream of ordinary key input.
                .arg("-p")
                // Preserve LF bytes. tmux otherwise replaces every LF with CR, which can submit a
                // partial prompt while the rest of the paste is still arriving.
                .arg("-r")
                .arg("-b")
                .arg(&buf)
                .arg("-t")
                .arg(&target)
                .arg("-d")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("run tmux paste-buffer")?;
            if !status.success() {
                bail!("tmux paste-buffer failed for session {session}");
            }
        }
        // Codex suppresses Enter briefly while a paste burst settles. Waiting beyond that window
        // reduces retries, but the pane observation below remains the source of truth.
        std::thread::sleep(if codex_paste {
            CODEX_PASTE_GUARD
        } else {
            SUBMIT_GUARD
        });
        let send_enter = || {
            self.base()
                .arg("send-keys")
                .arg("-t")
                .arg(&target)
                .arg("Enter")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("run tmux send-keys (Enter)")
        };
        let attempts = if codex { CODEX_SUBMIT_ATTEMPTS } else { 1 };
        for attempt in 0..attempts {
            let status = send_enter()?;
            if !status.success() {
                bail!("tmux send-keys (Enter) failed for session {session}");
            }
            if !codex {
                return Ok(());
            }
            std::thread::sleep(CODEX_SUBMIT_RECHECK_GUARD);
            // Styled, so the dim placeholder can be told from a still-typed draft.
            let capture = self.capture_tail_styled(session, 80)?;
            if !codex_draft_visible(&capture, text, uses_paste_buffer) {
                return Ok(());
            }
            if attempt + 1 == attempts {
                bail!(
                    "Codex draft remained visible after {CODEX_SUBMIT_ATTEMPTS} submission attempts for session {session}"
                );
            }
        }
        Ok(())
    }

    fn select_dialog_option(&self, session: &str, current: usize, target: usize) -> Result<()> {
        let keys =
            dialog_keys::single_selection_keys(current, target).map_err(anyhow::Error::msg)?;
        self.send_dialog_key_names(session, &keys)
    }

    fn verify_dialog_interactive(
        &self,
        session: &str,
        expected: &super::PaneDialog,
    ) -> Result<super::PaneDialog> {
        let current = expected
            .selected_index
            .context("dialog has no selected option")?;
        let (probe, key, undo) =
            dialog_keys::probe_plan(current, expected.options.len()).map_err(anyhow::Error::msg)?;
        self.send_dialog_key_names(session, &[key])?;
        std::thread::sleep(Duration::from_millis(100));
        let capture = self.capture_tail(session, 40)?;
        let moved = super::classify_dialog(&capture);
        if !moved
            .as_ref()
            .is_some_and(|dialog| dialog_keys::probe_matches(expected, dialog, probe))
        {
            // Best-effort restoration. On a real menu this returns the highlight;
            // on quoted text it counteracts history navigation without submitting.
            let _ = self.send_dialog_key_names(session, &[undo]);
            bail!("pane did not react like the classified dialog in session {session}");
        }
        moved.context("validated above")
    }

    fn select_dialog_options(
        &self,
        session: &str,
        dialog: &super::PaneDialog,
        targets: &[usize],
    ) -> Result<()> {
        let keys = dialog_keys::selection_keys(dialog, targets).map_err(anyhow::Error::msg)?;
        self.send_dialog_key_names(session, &keys)
    }

    fn has_clients(&self, session: &str) -> Result<bool> {
        let out = self
            .base()
            .arg("list-clients")
            .arg("-t")
            .arg(exact(session))
            .output()
            .context("run tmux list-clients")?;
        // Dead session / no server ⇒ non-zero ⇒ no clients. A live but detached session
        // ⇒ success with EMPTY stdout ⇒ no clients. A client attached ⇒ non-empty stdout.
        Ok(out.status.success() && !out.stdout.is_empty())
    }

    fn session_created(&self, session: &str) -> Result<Option<Epoch>> {
        let out = self
            .base()
            .arg("display-message")
            .arg("-p")
            // display-message resolves its target like a pane, so it needs the
            // `=name:` form too (see `exact_pane`). With the bare `=name` anchor
            // tmux exits 0 and prints NOTHING — a silent `Ok(None)`, which told
            // `chat_lock::is_active` the session's age was unknown and so quietly
            // disabled the marker-independent orphan reap (a stale detached chat
            // session then deferred the poll forever).
            .arg("-t")
            .arg(exact_pane(session))
            .arg("#{session_created}")
            .output()
            .context("run tmux display-message")?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<i64>()
            .ok())
    }

    /// `#{pane_dead}` is `1` for a pane whose process exited under `remain-on-exit`, `0`
    /// for a live one.
    ///
    /// ASSERTS ON OUTPUT, NEVER ON EXIT STATUS — the trap this project has already been
    /// bitten by twice (see [`TmuxDriver::session_created`]): `display-message -p` exits
    /// **0 printing NOTHING** for a target it cannot resolve. Keying off `status.success()`
    /// would therefore read "no such pane" as a successful probe of an empty string. So the
    /// ONLY thing that means dead here is a literal `1` on stdout; an empty print, junk, or
    /// a non-zero exit are all "liveness UNKNOWN" ⇒ `false` ⇒ the caller behaves exactly as
    /// it did before this probe existed (fail-safe: never escalate on a guess).
    ///
    /// Takes the PANE target form (`=name:`), like `capture-pane`/`send-keys` — see
    /// [`exact_pane`]; the bare `=name` session anchor is the shape that silently printed
    /// nothing.
    fn pane_dead(&self, session: &str) -> Result<bool> {
        let out = self
            .base()
            .arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(exact_pane(session))
            .arg("#{pane_dead}")
            .output()
            .context("run tmux display-message #{pane_dead}")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim() == "1")
    }

    /// `#{pane_in_mode}` is `1` for a pane sitting in a tmux mode (copy-mode), `0` for
    /// one at its normal prompt.
    ///
    /// ASSERTS ON OUTPUT, NEVER ON EXIT STATUS, for exactly the reason spelled out on
    /// [`TmuxDriver::pane_dead`] and [`TmuxDriver::session_created`]: `display-message
    /// -p` exits **0 printing NOTHING** for a target it cannot resolve. Measured on
    /// tmux 3.6a, all three cases at once — the PANE form `=name:` prints `0`/`1`, the
    /// bare `=name` SESSION form prints nothing (exit 0), and a bogus `=nosuch:` also
    /// prints nothing (exit 0). So the only thing that means "in a mode" is a literal
    /// `1`; empty, junk or a non-zero exit are all "UNKNOWN" ⇒ `false` ⇒ the caller
    /// behaves exactly as it did before this probe existed.
    fn pane_in_mode(&self, session: &str) -> Result<bool> {
        let out = self
            .base()
            .arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(exact_pane(session))
            .arg("#{pane_in_mode}")
            .output()
            .context("run tmux display-message #{pane_in_mode}")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim() == "1")
    }
}
