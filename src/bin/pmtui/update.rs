//! What the Settings `Updates` row knows, and nothing that performs it.
//!
//! The dashboard does NOT speak to GitHub. `install.sh` already resolves the newest release — with a
//! redirect-first strategy and an API fallback, because the API's unauthenticated hourly limit is
//! shared per IP — and it already knows how to download, verify a checksum, smoke-test and replace
//! binaries. Re-implementing any of that here would be a second answer to "what is the newest
//! version?" that could disagree with the one that actually installs.
//!
//! So this module is a PARSER and a PRESENTER over `install.sh --check`: it turns two lines plus an
//! exit code into the row's text and the actions its dropdown offers. Everything here is pure, which
//! is what lets the awkward states — unreachable, nothing published, a source install, an installer
//! marker that disagrees with the running binary — be tested without a network.

/// The version of pmtui that is RUNNING, compiled in.
///
/// Deliberately distinct from the installer's `$STATE_DIR/version` marker: they disagree whenever
/// `scripts/link-release.sh` has pointed `~/.local/bin` at a working-tree build, and that
/// disagreement is the single most useful thing this row can say — it is why auto-update appears to
/// do nothing on a developer's machine.
pub(crate) const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// How the check itself went, from `install.sh --check`'s exit code.
///
/// THREE outcomes, not two. `latest_tag` separates them on purpose and the row must keep them
/// apart: telling someone they are up to date when GitHub was unreachable is the one answer here
/// that is actively misleading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A tag came back.
    Found,
    /// GitHub answered and the repository has published nothing (exit 10).
    NothingPublished,
    /// GitHub could not be asked at all — offline, a proxy, a throttled API (any other exit).
    Unreachable,
    /// The installer ran and did not answer the question: it is older than `--check`.
    ///
    /// THE FOURTH OUTCOME, and the one this module first got wrong. An installer that predates
    /// `--check` prints `unknown argument: --check` and exits 1 — the same exit as "could not ask" —
    /// so on every machine installed before this feature the row reported a network problem to
    /// someone whose network was fine, and sent them to debug the wrong thing.
    Unsupported,
}

impl Outcome {
    /// Read the installer's verdict from its exit status. A signal-killed child has no code at all,
    /// which is "could not ask" rather than any answer.
    pub(crate) fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Found,
            Some(10) => Self::NothingPublished,
            _ => Self::Unreachable,
        }
    }

    /// The verdict including whether the installer answered the CONTRACT at all.
    ///
    /// Judged on the two key lines rather than on the old script's error wording: a supporting
    /// installer always prints `current=` and `latest=`, empty or not, so neither key means the
    /// request was not understood. The wording belongs to one release; the contract belongs to
    /// every release. A child with no exit code at all was killed, which is still "could not ask".
    fn read(code: Option<i32>, answered: bool) -> Self {
        match code {
            Some(_) if !answered => Self::Unsupported,
            other => Self::from_code(other),
        }
    }
}

/// Everything one `install.sh --check` told us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Check {
    /// The installer's `$STATE_DIR/version` marker: a tag, `source@<sha>`, or `None` when this
    /// machine was never installed by the installer.
    pub(crate) installed: Option<String>,
    /// The newest published tag, when there is one.
    pub(crate) latest: Option<String>,
    pub(crate) outcome: Outcome,
}

impl Check {
    /// Parse the two-line contract. An unknown line is IGNORED rather than fatal, so a future
    /// installer that prints a third fact does not break an older dashboard; an absent or empty
    /// value reads as `None`, which is exactly what the installer means by a bare `current=`.
    pub(crate) fn parse(stdout: &str, code: Option<i32>) -> Self {
        let mut installed = None;
        let mut latest = None;
        let mut answered = false;
        for line in stdout.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("current=") {
                installed = non_empty(value);
                answered = true;
            } else if let Some(value) = line.strip_prefix("latest=") {
                latest = non_empty(value);
                answered = true;
            }
        }
        Self {
            installed,
            latest,
            outcome: Outcome::read(code, answered),
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Numeric version ordering for the `vMAJOR.MINOR.PATCH` tags this project publishes.
///
/// `None` when either side is not that shape — a `source@<sha>` marker, a date tag, an empty
/// string. `None` means "cannot say", NEVER "equal": the difference decides whether the row claims
/// to know something, and claiming to know is the failure that matters.
///
/// Lexicographic string comparison is wrong here and would stay wrong silently: `v0.1.10` sorts
/// BELOW `v0.1.9` as text, so the row would hide exactly the update a long-lived project needs
/// most.
fn newer(latest: &str, current: &str) -> Option<bool> {
    fn parts(tag: &str) -> Option<Vec<u64>> {
        let tag = tag.trim();
        let tag = tag.strip_prefix('v').unwrap_or(tag);
        let parts: Option<Vec<u64>> = tag
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect();
        parts.filter(|parts| !parts.is_empty())
    }
    Some(parts(latest)? > parts(current)?)
}

/// What the row should say, and what its dropdown may offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    /// Nothing asked yet. The row never checks on its own: opening a screen should not reach the
    /// network, where a stall is indistinguishable from a frozen dashboard.
    Unchecked,
    /// A newer release than the one running exists.
    Available {
        latest: String,
    },
    /// The newest release is not newer than what is running.
    Current,
    /// A tag came back but could not be compared with what is running (a `source@` marker, an
    /// unusual tag). Reported as a fact rather than as a verdict.
    Incomparable {
        latest: String,
    },
    NothingPublished,
    Unreachable,
    /// The `install.sh` on disk predates `--check`, so it cannot be asked.
    ///
    /// Its own `--update` still works and replaces its copy of itself, so this state has a way out
    /// — and naming it is the whole point: reported as `Unreachable`, as it was at first, it sent a
    /// person with a perfectly good network to go and debug their network.
    InstallerTooOld,
}

impl Status {
    /// The row's value column — the whole point of the feature: one line answering "am I behind?"
    /// and, when they disagree, naming the installer marker, the fact that explains why a
    /// developer's machine never auto-updates.
    pub(crate) fn value(&self, installed: Option<&str>) -> String {
        // TERSE ON PURPOSE. This is a table CELL: the Settings view gives every row the same value
        // column, and a sentence here is elided to a `…` that hides the fact it was written to
        // carry. The status line says the long version when a check has just run.
        let head = match self {
            Self::Unchecked => format!("{RUNNING} · not checked yet"),
            Self::Available { latest } => format!("{RUNNING} → {latest}"),
            Self::Current => format!("{RUNNING} · up to date"),
            Self::Incomparable { latest } => format!("{RUNNING} · latest {latest}"),
            Self::NothingPublished => format!("{RUNNING} · no releases"),
            Self::Unreachable => format!("{RUNNING} · could not reach GitHub"),
            Self::InstallerTooOld => format!("{RUNNING} · installer too old to ask"),
        };
        match installed {
            // Only when it DISAGREES. On an ordinary install these are the same string, and saying
            // it twice would be noise.
            Some(marker) if marker.trim().trim_start_matches('v') != RUNNING => {
                format!("{head} (installed {})", marker.trim())
            }
            _ => head,
        }
    }
}

/// Turn one check into the row's status.
pub(crate) fn status(check: &Check) -> Status {
    match check.outcome {
        Outcome::NothingPublished => Status::NothingPublished,
        Outcome::Unreachable => Status::Unreachable,
        Outcome::Unsupported => Status::InstallerTooOld,
        Outcome::Found => match check.latest.as_deref() {
            // Exit 0 promises a tag; an empty one is a contract violation, and "could not reach"
            // is the honest reading of an answer we cannot use.
            None => Status::Unreachable,
            Some(latest) => match newer(latest, RUNNING) {
                Some(true) => Status::Available {
                    latest: latest.to_string(),
                },
                Some(false) => Status::Current,
                None => Status::Incomparable {
                    latest: latest.to_string(),
                },
            },
        },
    }
}

/// Where the published installer lives, when the one on disk does not say.
///
/// Only a fallback: `repair_url` prefers the `REPO=` line of the installer ALREADY installed, so a
/// machine that installed from a fork repairs itself from that fork rather than silently being
/// pointed at upstream.
pub(crate) const REPO: &str = "hung-phan/pmtui";

/// The `install.sh` to repair a too-old installer from, read from the installer on disk.
///
/// A machine stuck with a pre-`--check` script CANNOT repair itself by running it: `--update
/// --if-newer` answers "Already up to date" and the self-copy is skipped because the running script
/// is the stored one. Measured, not assumed. So the only way out is a fresh copy of the script, and
/// the honest place to get it is the repository that installed this machine.
pub(crate) fn repair_url(installer: &str) -> String {
    let repo = installer
        .lines()
        .filter_map(|line| line.trim().strip_prefix("REPO="))
        .map(|value| value.trim().trim_matches(['"', '\'']))
        // `owner/name` and nothing else. Anything with whitespace, or without the slash, is a line
        // this parser does not understand — and guessing at a URL from it would fetch a script from
        // somewhere nobody chose.
        .find(|repo| repo.contains('/') && !repo.contains(char::is_whitespace))
        .unwrap_or(REPO);
    format!("https://raw.githubusercontent.com/{repo}/main/install.sh")
}

/// Whether a downloaded file IS this installer.
///
/// A proxy error page and a truncated download are both a body with a 200, and replacing a working
/// installer with one of those is the worst outcome available here. Structural markers rather than a
/// version string, because those are what every release has in common.
pub(crate) fn looks_like_installer(text: &str) -> bool {
    text.lines()
        .any(|line| line.starts_with("install_binary()"))
        && text.lines().any(|line| line.starts_with("latest_tag()"))
}

/// Which installer run the drain owes the human.
///
/// Two kinds because a too-old installer needs a DIFFERENT run: the one on disk cannot repair
/// itself, so the repair fetches the published script first and runs that instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallerRun {
    /// `install.sh --update --if-newer -y`, using the installer already on disk.
    Update,
    /// Download the published `install.sh`, then run its update.
    Repair,
}

/// What `Enter` on the row does next.
///
/// ONE KEYPRESS, NOT A MENU. The row has at most one sensible action at any moment — ask, or install
/// what asking found — so a dropdown made the human choose between an option and nothing, which is
/// ceremony around a single answer (user: *"the UI, looks complex. it can be just a simple to tap on
/// to check for update"*). So this row has no dropdown: its third column says what `Enter` will do,
/// and `Enter` does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Next {
    /// Ask the installer what the newest release is.
    Check,
    /// Run the installer to install `latest`.
    ///
    /// Offered for `Incomparable` too, deliberately: this module's comparison is PRESENTATIONAL, and
    /// `install.sh --update --if-newer` makes the real decision and declines on its own when nothing
    /// is newer. Withholding the action on a tag shape we merely failed to parse would leave the
    /// human with no way forward.
    Install { latest: String },
    /// Run the installer so it replaces its own copy, because the one on disk cannot be asked.
    FixInstaller,
}

/// The one action a status leaves to do.
pub(crate) fn next(status: &Status) -> Next {
    match status {
        Status::Available { latest } | Status::Incomparable { latest } => Next::Install {
            latest: latest.clone(),
        },
        Status::InstallerTooOld => Next::FixInstaller,
        // Checked and current, never checked, nothing published, unreachable: asking again is the
        // only thing left, and it is also the only thing that could change the answer.
        Status::Unchecked | Status::Current | Status::NothingPublished | Status::Unreachable => {
            Next::Check
        }
    }
}
