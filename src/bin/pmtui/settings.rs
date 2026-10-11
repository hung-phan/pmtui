//! The dashboard's own preferences: `pmtui.json`, beside the registry it belongs to.
//!
//! Separate from `.project-state/*/config.json`, which is PER SESSION and describes a project's
//! autonomy. A theme is a property of the human looking at the screen, not of any one session, so
//! storing it per session would write the same answer a dozen times and let rows disagree about it.
//!
//! Beside the REGISTRY rather than at a fixed absolute path, for the reason the status log is: a
//! scratch `--registry` under `/tmp` gets its own preferences, so a test or a throwaway dashboard
//! cannot rewrite the human's.

use std::path::{Path, PathBuf};

use agent_manager::state;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The preferences file's name inside the dashboard's own directory.
const SETTINGS_FILE: &str = "pmtui.json";

/// Where the preferences live for the dashboard that owns `registry`.
pub(crate) fn settings_path(registry: &Path) -> PathBuf {
    match registry.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(SETTINGS_FILE),
        _ => PathBuf::from(SETTINGS_FILE),
    }
}

/// What the dashboard remembers between runs.
///
/// `#[serde(default)]` on the struct so a file written by an older or newer pmtui still parses: a
/// preferences file that failed to load would take the whole dashboard's look with it, and the right
/// answer to "this file is missing a field I know" is to keep the fields it does have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    /// The opaline theme id, e.g. `catppuccin-mocha`.
    pub(crate) theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: agent_manager::theme::DEFAULT_THEME.to_string(),
        }
    }
}

/// Read the preferences at `path`, defaulting when the file is absent.
///
/// A file that is PRESENT but unparseable is an error rather than a silent default: it is a file the
/// human (or a previous pmtui) wrote, and quietly ignoring it would look like the setting had been
/// forgotten.
pub(crate) fn load(path: &Path) -> Result<Settings> {
    state::read_json_or(path, Settings::default())
}

/// Replace the preferences at `path` atomically.
pub(crate) fn save(path: &Path, settings: &Settings) -> Result<()> {
    state::write_json_atomic(path, settings)
}

/// One thing the Settings view can change.
///
/// The view is a TABLE of settings, each with a dropdown, rather than one setting's list filling the
/// screen — user: *"we may have many settings in the future, so can you make a drop down instead"*.
/// Adding the next setting is a variant here, two arms below, one branch in `App::setting_row` and one
/// in `App::commit_setting`. Neither the renderer nor the key handler grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingKind {
    /// The dashboard's colours.
    Theme,
    /// The version this dashboard runs, and whether a newer release exists.
    ///
    /// The ODD ONE OUT, deliberately: its dropdown offers ACTIONS (check, update) rather than
    /// values to store, so `commit_setting` runs them and nothing lands in `pmtui.json`. It earns
    /// the exception by needing no new key and no new renderer branch — the row's value column is
    /// the answer to "am I behind?", and the existing Enter-opens-then-Enter-picks flow is already
    /// the right gesture for "do this now". A whole screen for one line of text would be worse.
    Updates,
}

/// What `Enter` on a settings row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gesture {
    /// Open the row's list of values and commit the one the cursor lands on. Two presses, with the
    /// list on screen for the second, because picking a value is a choice.
    Pick,
    /// Do the one thing the row's state says is next. ONE press, because there is nothing to choose
    /// between: a list of one option is ceremony, and the row's third column already says what will
    /// happen.
    Act,
}

impl SettingKind {
    /// Whether `Enter` on this row opens a list or acts.
    pub(crate) fn gesture(self) -> Gesture {
        match self {
            Self::Theme => Gesture::Pick,
            Self::Updates => Gesture::Act,
        }
    }

    /// Every setting, in the order the view lists them.
    pub(crate) const ALL: [Self; 2] = [Self::Theme, Self::Updates];

    /// The setting's name — the row's first column.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::Updates => "Updates",
        }
    }

    /// What it governs, in one dim phrase after the value, so a table of settings reads without help.
    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Theme => "every colour the dashboard draws",
            Self::Updates => "the version this dashboard runs",
        }
    }
}

/// One choice inside a setting's dropdown: what it is called, an optional dim note, and the value it
/// commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingOption {
    /// The name, which is all a dropdown row needs.
    pub(crate) label: String,
    /// A dim suffix for a fact the name does not carry — a theme's `dark`/`light`.
    pub(crate) note: String,
    /// What gets written when this row is chosen.
    pub(crate) value: String,
}

/// One row of the Settings view, resolved for the frame that draws it.
///
/// No `kind`: every caller already has one (it asked for this row by kind), and a copy on the row is
/// a second source of truth for the same fact.
pub(crate) struct SettingRow {
    /// The value in force, as a human reads it.
    pub(crate) value: String,
    /// Which option is in force, or `None` when the stored value is not one this build offers.
    pub(crate) current: Option<usize>,
    pub(crate) options: Vec<SettingOption>,
}
