//! Phase-worker dispatch (spec §6): the harness builds a phase prompt, launches
//! ONE authenticated `claude`/`codex` CLI session (via the existing tmux
//! substrate), and parses the worker's machine result. **Worker proposes,
//! harness disposes** — the worker only ever writes artifacts + its
//! `result.json`; the harness alone validates and commits control state.
//!
//! This module owns the two pure pieces: constructing the exact CLI argv for an
//! engine, and the agent-authored proposal schema (`StopDraft`/`CompletionDigest`). Actual
//! spawning reuses `tmux::Driver::spawn_step` (the detached, non-blocking,
//! done-signal substrate), so the daemon sweep never blocks on a worker.
//!
//! One launch shape per file: `launch` builds the argv for the two that must not drift
//! (the ephemeral headless `-p` worker and the persistent interactive REPL the daemon
//! nudges), which is why they share a module; `result` is the strict proposal schema the
//! agent hands back (`StopDraft`/`CompletionDigest`); and `supervisor` is the deliberately
//! different third shape — one model-pinned, spend-capped consult that answers one question
//! with one JSON object.

mod launch;
mod result;
mod supervisor;

pub use launch::{
    PermissionMode, Resume, build_command, build_fork_command, build_job_command,
    build_loop_command, build_standard_command, turn_hook_enabled,
};
pub use result::{
    CompletionDigest, EffectAuthority, EffectReversibility, EffectScope, StopDraft, StopEffect,
};
pub use supervisor::{
    SUPERVISOR_BUDGET_USD_ENV, SUPERVISOR_CODEX_MODEL_ENV, SUPERVISOR_MODEL, SUPERVISOR_MODEL_ENV,
    build_supervisor_command, build_supervisor_command_codex,
};
#[cfg(test)]
mod tests;
