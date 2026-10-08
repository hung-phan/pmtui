//! tmux Driver: how the daemon spawns a coordinator step in its own detached
//! tmux session, checks liveness, captures a diagnostic tail, and reaps it.
//! The real impl shells out to `tmux`; `fake::FakeDriver` backs unit tests (not a doc
//! link: `fake` is `#[cfg(test)]`, so it does not exist in a `cargo doc` build).
//!
//! One seam per file: `driver` is the trait and the types that cross it, `real`
//! shells out to the `tmux` binary, `fake` is the in-memory double the unit tests
//! drive, `launch` is the typed vocabulary of an interactive launch (its managed
//! env, outcome and error), `activity` and `dialog` are the pure pane-text heuristics, `send_text`
//! sanitizes a nudge before it is typed, and `session_names` derives the
//! per-family session names. Every item is re-exported here, so every
//! `tmux::<Item>` path outside this module keeps resolving.

mod activity;
mod codex;
mod dialog;
mod dialog_keys;
mod driver;
mod launch;
mod real;
mod send_text;
mod session_names;

#[cfg(test)]
pub mod fake;

#[cfg(test)]
mod tests;

pub use activity::{PaneActivity, classify_pane, has_composer, progress_fingerprint};
// The heartbeat confirmation gate's content-stability helper. `pub` (not `pub(crate)`)
// because pmtui — a SEPARATE crate from this lib — runs the same two-observation gate
// read-only in its refresh to tell a working Standard agent from one idle at its prompt.
pub use activity::idle_fingerprint;
pub use dialog::{
    PaneDialog, PaneDialogClass, PaneDialogMode, classify_dialog, dialog_option_is_concrete,
};
pub use driver::{Driver, Observation, StepHandle, observe};
pub use launch::{
    ENV_BIN, ENV_SESSION, ENV_STATE_DIR, LaunchError, LaunchOutcome, ManagedEnv,
    pmtui_bin_for_current_process,
};
pub use real::{LAUNCH_COMMAND_MAX_BYTES, TmuxDriver, launch_command};
// POSIX shell escaping, shared with `worker::launch`'s turn-hook builder so a project
// root with a shell metacharacter can't malform the hook command.
pub(crate) use real::shq;
pub use send_text::sanitize_send_text;
pub use session_names::{job_session_name, session_name, supervisor_session_name};
