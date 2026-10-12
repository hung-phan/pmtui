//! The Settings `Updates` row's impure half: run the installer and report.
//!
//! The decisions live in [`crate::update`], which is pure and tested without a network. This is the
//! part that touches the world — the same split `send.rs` and `app/sending.rs` already use.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::app::App;

/// Where `install.sh` keeps its own copy of itself, from the environment.
///
/// The installer puts it there precisely so it can be re-run later (`write_manifest`: "keep a copy
/// of ourselves so the wrappers (and `--update`) work after the original `curl | bash` pipe is
/// gone"), and the launcher wrappers already call it from exactly this path. A repository checkout
/// would work only for people who have one.
///
/// Read ONCE, into [`App::installer_path`], rather than here on every use: edition 2024 forbids
/// `set_var` in tests, so a function that reads the environment cannot be driven by one. Injecting
/// the resolved path is the seam this project already uses for `claude_home`/`codex_home`.
pub(crate) fn installer_from_env() -> PathBuf {
    installer_in(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// The RULE, separated from the reading of it.
///
/// `install.sh` resolves the same two variables the same way (`DATA_HOME="${XDG_DATA_HOME:-$HOME/
/// .local/share}"`), so this is one place where the dashboard and the installer must agree — and
/// where disagreeing would send the row looking for an installer that is not there. Taking the
/// values as arguments is what lets BOTH branches be driven: edition 2024 forbids `set_var` in
/// tests, so a function that reads the environment itself keeps a branch no test can reach.
///
/// An empty `XDG_DATA_HOME` counts as unset, which is what the shell means by it.
pub(crate) fn installer_in(
    data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    let data = match data_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home.unwrap_or_default()).join(".local/share"),
    };
    data.join("agent-manager/install.sh")
}

/// Where a repair should fetch the published installer from, for this dashboard.
///
/// Read from the installer ON DISK so a machine installed from a fork repairs itself from that fork.
/// `$PMTUI_REPAIR_URL` overrides it, which is how the acceptance test points the fetch at a local
/// `file://` copy instead of the network.
pub(crate) fn repair_source(app: &App) -> String {
    // THE FIELD, never the environment, and never a default that reaches the network. Reading
    // `$PMTUI_REPAIR_URL` here instead let a unit test — which cannot set it — fall through to the
    // live URL and run the REAL installer against the machine running the suite. The fixture now
    // injects a `file://` sentinel, so an unset test cannot reach GitHub at all.
    if let Some(url) = &app.repair_url {
        return url.clone();
    }
    let installer = std::fs::read_to_string(&app.installer_path).unwrap_or_default();
    crate::update::repair_url(&installer)
}

/// Download the published `install.sh` to a temp file and return its path.
///
/// Shelled out to `curl`/`wget` rather than linking an HTTP stack: `install.sh` resolves releases
/// the same way, and the dashboard having no HTTP client of its own is a property worth keeping.
/// Runs inside the terminal handoff, so the human watches the download they asked for.
///
/// The download is SNIFFED before it is handed to `bash`. A proxy error page and a truncated
/// response are both a body with a 200, and running one of those as root-adjacent shell is the worst
/// outcome available here.
pub(crate) fn fetch_installer(url: &str) -> anyhow::Result<PathBuf> {
    // A FRESH, UNIQUE DESTINATION EVERY TIME. A path keyed only on the pid is reused, and `curl -f`
    // leaves the previous file in place when a download fails — so a failed fetch would sniff and
    // then RUN whatever a previous fetch left behind. Found by a test whose download was deliberate
    // junk and which "succeeded" by executing the good script from the fetch before it.
    let dir = std::env::temp_dir().join(format!(
        "pmtui-repair-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir)?;
    let script = dir.join("install.sh");
    println!("fetching {url}");
    let argv = download_argv(which("curl"), url, &script);
    let fetched = Command::new(&argv[0]).args(&argv[1..]).status()?;
    if !fetched.success() {
        // Never leave a partial body behind for anything else to find.
        let _ = std::fs::remove_file(&script);
        anyhow::bail!("could not download {url} ({fetched})");
    }
    let text = std::fs::read_to_string(&script)?;
    if !crate::update::looks_like_installer(&text) {
        let _ = std::fs::remove_file(&script);
        anyhow::bail!("what {url} returned is not the installer — refusing to run it");
    }
    Ok(script)
}

/// The download command, preferring `curl` and falling back to `wget` exactly as `install.sh` does.
///
/// Both forms are time-capped for the same reason `latest_tag` is: this runs inside the terminal
/// handoff, and an unbounded fetch would park the dashboard with no way back.
///
/// Pure, and taking the answer to "is curl here?" as an ARGUMENT, because the machine running the
/// tests has curl — so the `wget` branch is otherwise a branch nothing can reach, which is the same
/// as not knowing whether it works.
pub(crate) fn download_argv(have_curl: bool, url: &str, dest: &Path) -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = if have_curl {
        ["curl", "-fsSL", "-m", "30", "-o"]
            .iter()
            .map(Into::into)
            .collect()
    } else {
        ["wget", "-q", "--timeout=30", "--tries=1", "-O"]
            .iter()
            .map(Into::into)
            .collect()
    };
    argv.push(dest.into());
    argv.push(url.into());
    argv
}

/// Whether `program` is on `PATH`, so the fetch can prefer `curl` and fall back to `wget` the way
/// `install.sh` does.
fn which(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program} >/dev/null 2>&1"))
        .status()
        .is_ok_and(|status| status.success())
}

/// The installer invocation for [`crate::session::drain_terminal_requests`].
///
/// `--if-newer` is passed even though the row only offers an update once something looked newer:
/// the row's comparison is presentational, and the installer is the authority on whether an update
/// is warranted. It costs nothing and stops a stale row forcing a pointless reinstall.
pub(crate) fn update_args() -> [&'static str; 3] {
    ["--update", "--if-newer", "-y"]
}

/// Run a check that a frame has already announced.
///
/// Called from the event loop right AFTER the draw, which is the only point where the status line
/// the keypress wrote is on screen before the blocking call starts. A check gets no terminal handoff
/// — it prints two lines nobody needs to watch — so without this the dashboard would simply stop
/// repainting for as long as `install.sh` took, which is the one thing a status line exists to
/// prevent.
pub(crate) fn drain_pending_check(app: &mut App) {
    if std::mem::take(&mut app.pending_check) {
        app.check_for_update();
    }
}

impl App {
    /// `Check now`: say that a check is starting, and leave the running of it to the next frame.
    pub(crate) fn request_check(&mut self) {
        self.status = "checking GitHub for the newest release…".into();
        self.pending_check = true;
    }

    /// Ask the installer what the newest release is, and say so.
    ///
    /// Runs INLINE rather than through the terminal handoff. A check prints two lines nobody needs
    /// to watch, and its network wait is bounded inside `install.sh` (both `latest_tag_url` and
    /// `latest_tag_api` cap their request at ten seconds), so the event loop cannot be parked
    /// indefinitely. The handoff is for the update, which is slow and worth seeing.
    pub(crate) fn check_for_update(&mut self) {
        let script = self.installer_path.clone();
        if !script.is_file() {
            // Not a failure to apologise for: plenty of installs are a cargo build or a package
            // manager. Say what is missing and what that means, rather than "check failed".
            self.update_status = crate::update::Status::Unreachable;
            self.update_installed = None;
            self.status = missing_installer(&script);
            return;
        }
        // A failure to SPAWN is folded into "no exit code", which `Outcome::from_code` already reads
        // as `Unreachable`. It gets no branch of its own because it would be a branch for "this
        // machine has no bash" — on which neither the installer nor this dashboard's own terminal
        // handoff would work either, so a bespoke message would be describing a machine that cannot
        // reach this line.
        let output = Command::new("bash").arg(&script).arg("--check").output();
        let code = output.as_ref().ok().and_then(|out| out.status.code());
        let stdout = output.map(|out| out.stdout).unwrap_or_default();
        let check = crate::update::Check::parse(&String::from_utf8_lossy(&stdout), code);
        self.update_installed = check.installed.clone();
        self.update_status = crate::update::status(&check);
        // The row already carries the detail; the status line says what just happened.
        self.status = match &self.update_status {
            crate::update::Status::Available { latest } => format!("{latest} is available"),
            crate::update::Status::Current => "already on the newest release".into(),
            crate::update::Status::Incomparable { latest } => format!("newest release is {latest}"),
            crate::update::Status::NothingPublished => "no releases published yet".into(),
            // NOT "could not reach GitHub" — that is the diagnosis this state exists to stop. The
            // network is fine; the installer on disk is older than the question.
            crate::update::Status::InstallerTooOld => {
                "the installed install.sh is too old to check — Enter fetches a current one".into()
            }
            crate::update::Status::Unreachable | crate::update::Status::Unchecked => {
                "could not reach GitHub".into()
            }
        };
    }

    /// `Update to …`: queue the installer for the terminal handoff.
    ///
    /// Queued rather than run here because [`crate::session::drain_terminal_requests`] owns handing
    /// the real terminal over — the installer downloads, verifies, smoke-tests and may ask
    /// something, and watching it is the point. Same path `$EDITOR` and attach take.
    pub(crate) fn request_update(&mut self) {
        if !self.installer_path.is_file() {
            self.status = missing_installer(&self.installer_path);
            return;
        }
        self.pending_install = Some(crate::update::InstallerRun::Update);
    }

    /// `Repair the installer`: fetch the published script and run THAT.
    ///
    /// The one action that does not simply run what is on disk, because what is on disk is the
    /// problem: a pre-`--check` installer answers `--update --if-newer` with "Already up to date"
    /// and skips its own self-copy, so running it again changes nothing, forever. Measured on a real
    /// machine before this existed.
    pub(crate) fn request_repair(&mut self) {
        if !self.installer_path.is_file() {
            self.status = missing_installer(&self.installer_path);
            return;
        }
        self.pending_install = Some(crate::update::InstallerRun::Repair);
    }
}

/// One wording for "there is no installer here", shared by both entry points so the check and the
/// update cannot describe the same machine differently.
fn missing_installer(script: &Path) -> String {
    format!(
        "no installer at {} — this build was not installed by install.sh, so it cannot check or \
         update itself",
        script.display()
    )
}
