//! The `integration` test target: the things the harness can only be proven to do
//! against REAL substrate — a real `tmux` server on a private `-L` socket, a real
//! `pmd`, a real `pmtui` driven with real keystrokes. The plain tests need nothing
//! but a `tmux` binary; the `#[ignore]`d ones are the acceptance suite, run with
//! `cargo test --test integration -- --ignored --test-threads=1`.
//!
//! `probe`, `keystrokes`, `stubs`, `seed` and `pmtui_fixture` are the shared fixture
//! layer; every other module is one thing being proven.

mod audit_tabs;
mod autopilot_dial;
mod conversation_fork;
mod create_form;
mod decider_bench;
mod decider_visibility;
mod direct_dispatch;
mod doctor;
mod driver;
mod enter_routing;
mod indicators;
mod job_scheduler;
mod keystrokes;
#[cfg(target_os = "linux")]
mod nonblocking_checkpoint;
mod nudge_judge;
mod pane_reading;
mod pmtui_fixture;
mod preview_scroll;
mod probe;
mod seed;
mod send_keys;
mod send_message;
mod session_switcher;
mod settings_theme;
mod single_instance;
mod spawn;
mod stubs;
mod ui_gallery;
