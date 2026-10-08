//! agent-manager: an external, code-based supervisor ("pmd") that drives
//! coordinator sessions to run projects end-to-end, escalating to the human
//! only when a decision genuinely needs them — scaled to project scope.
//!
//! Design: docs/superpowers/specs/2026-08-12-agent-manager-daemon-design.md
//!
//! The daemon holds no authoritative in-memory state; `.project-state/` on disk
//! is the source of truth, so a restart rebuilds the world from disk.

pub mod advise;
pub mod ansi;
pub mod attention;
pub mod chat_lock;
pub mod clock;
pub mod daemon;
pub mod doctor;
pub mod escalation;
pub mod job;
pub mod job_engine;
pub mod lease;
pub mod models;
pub mod ops;
pub mod pmstate;
pub mod policy;
pub mod registry;
pub mod skills;
pub mod spawn;
pub mod state;
pub mod stream_json;
pub mod theme;
pub mod tmux;
pub mod verify;
pub mod view;
pub mod worker;
pub mod worktree;
