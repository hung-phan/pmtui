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
        self.pending_update = true;
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
