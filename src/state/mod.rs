//! On-disk state: the `.project-state/` files that carry a project's durable state.
//!
//! **One writer per file** (protocol hardening): pmd owns `state.json` and
//! `driver.json`; pmtui owns human inputs such as `control.json`. There is no
//! cross-process read-modify-write race on one file.
//!
//! Consistency model: every write is atomic and durable — write a temp file in
//! the same directory, `fsync` it, `rename` into place, then `fsync` the parent
//! directory. A reader never sees a torn/partial JSON.
//!
//! Four files, in the order a write passes through them: `paths` is the pure
//! [`ProjectPaths`] resolver that decides the `.project-state/` layout in exactly one
//! place, `records` is the serde schema of every file on disk plus the typed reads and
//! the one-writer writes, `atomic` is the single temp+fsync+rename implementation
//! (with the tolerant readers paired with it) that every one of those files goes through,
//! and `purge` is the one thing here that DELETES — a single session's subtree, once its
//! row is gone for good. `turn_signal` stands apart from all of them: it only READS, and the
//! file it reads is the one in here that nothing in this crate writes — the engine's turn-end
//! hook does.

mod atomic;
mod checkpoint;
mod paths;
mod purge;
mod records;
mod turn_signal;

pub use atomic::{
    read_json, read_json_opt, read_json_or, write_atomic, write_json_atomic, write_text_atomic,
};
pub use checkpoint::{
    CheckpointActivity, CheckpointActivityStatus, WorkerCheckpoint, read_checkpoint,
};
pub use paths::{ProjectPaths, STATE_DIR};
pub use purge::purge_session_state;
pub use records::{
    ANSWERS_MAX, Answer, Config, Control, DriverState, ExitReason, RiskClass, SessionInfo, Stop,
    Tier, append_answer, read_control, stop_product_text, synthesized_stop_text, write_control,
    write_driver,
};
pub use turn_signal::{TURN_SIGNAL_LAG_MAX, TurnSignalHealth, turn_signal_health};

#[cfg(test)]
mod tests;
