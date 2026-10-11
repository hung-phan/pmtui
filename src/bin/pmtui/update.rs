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
        for line in stdout.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("current=") {
                installed = non_empty(value);
            } else if let Some(value) = line.strip_prefix("latest=") {
                latest = non_empty(value);
            }
        }
        Self {
            installed,
            latest,
            outcome: Outcome::from_code(code),
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
            Self::Unchecked => format!("{RUNNING} · Enter to check"),
            Self::Available { latest } => format!("{RUNNING} → {latest}"),
            Self::Current => format!("{RUNNING} · up to date"),
            Self::Incomparable { latest } => format!("{RUNNING} · latest {latest}"),
            Self::NothingPublished => format!("{RUNNING} · no releases"),
            Self::Unreachable => format!("{RUNNING} · unreachable"),
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

/// The values this row's dropdown commits. Each is an INSTRUCTION rather than a stored preference,
/// which is why `commit_setting` runs them instead of writing them to `pmtui.json`.
pub(crate) const ACTION_CHECK: &str = "check";
pub(crate) const ACTION_UPDATE: &str = "update";

/// One dropdown row: the value it commits, its label, and its dim note.
pub(crate) struct Action {
    pub(crate) value: &'static str,
    pub(crate) label: String,
    pub(crate) note: String,
}

/// The dropdown's rows for a given status: always a check, plus an update once there is something
/// to update to.
///
/// `Incomparable` offers the update too, and that is deliberate — this module's comparison is
/// PRESENTATIONAL. `install.sh --update --if-newer` makes the real decision and declines on its own
/// when nothing is newer, so offering the action cannot cause a wrong install; withholding it on a
/// tag shape we merely failed to parse would strand the human with no way forward.
pub(crate) fn actions(status: &Status) -> Vec<Action> {
    // The notes are TAGS, not sentences. A dropdown's note is a column sized to the widest one, and
    // the label — the thing a human is choosing between — gets what is left: two sentences here once
    // left the label zero columns wide, so the list rendered its options as blanks.
    let mut rows = vec![Action {
        value: ACTION_CHECK,
        label: "Check now".to_string(),
        note: "asks GitHub".to_string(),
    }];
    let latest = match status {
        Status::Available { latest } | Status::Incomparable { latest } => Some(latest),
        _ => None,
    };
    if let Some(latest) = latest {
        rows.push(Action {
            value: ACTION_UPDATE,
            label: format!("Update to {latest}"),
            // Eleven columns, like its sibling: the note column is capped at a third of the
            // dropdown, so anything longer is clipped mid-word at every width the view supports.
            note: "installs it".to_string(),
        });
    }
    rows
}
