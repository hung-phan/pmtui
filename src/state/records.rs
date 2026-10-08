//! The on-disk record schema: every type that is serialized into a `.project-state/`
//! file, its serde defaults, and the typed reads/writes of the well-known files. Schema
//! and accessor belong together because a defaulted field is only half a decision — the
//! other half is what the reader does when the whole file is missing.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::clock::Epoch;

use super::atomic::{read_json_or, write_json_atomic};
use super::paths::ProjectPaths;

/// Per-project autonomy tier — the manual scope baseline (design §8). Two
/// levels: `Standard` (collaborative, the default) and `Autopilot` (hands-off).
///
/// `Standard` is the default in the strong sense: see [`default_autonomy`].
/// The legacy `Guardian` level was removed (it was behaviorally identical to
/// `Standard` — `policy::decide` treated both the same); the `serde(alias)`
/// below keeps any existing on-disk `config.json` carrying `"guardian"` loading
/// as `Standard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Autopilot,
    #[serde(alias = "guardian")]
    Standard,
}

/// Inherent risk of a stop. `Hard` always escalates regardless of tier. The
/// variant order (`Low < Medium < Hard`) is the risk ordering — derived `Ord`
/// lets policy floor a labelled risk to a minimum (e.g. `labelled.max(Medium)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Low,
    Medium,
    Hard,
}

/// Why a step ended, from the daemon's point of view (written to `driver.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    Running,
    Clean,
    Failed,
    Timeout,
    CrashNoSignal,
}

/// `config.json` — intake preferences the daemon reads each tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// DEFAULTED, and the direction of that default is a safety property.
    ///
    /// Without it, a `config.json` that merely MISSED this field — `{}`, a file written by
    /// another tool against a different schema, anything — failed to parse, and an
    /// unparseable config read as "unknown tier", which `daemon::pmd_drives_row` treated as
    /// PERMISSION TO DRIVE. So a session the human had switched to Standard kept being typed
    /// into, and pmtui refused to write the flip in that same state ("config unreadable —
    /// tier unchanged"): a closed trap, reported as *"why my session with autopilot off
    /// receive the prompt"*.
    ///
    /// Defaulting closes it from the other side: a config that PARSES always yields a real
    /// tier, so the dial stays readable and writable even when the file came from elsewhere.
    #[serde(default = "default_autonomy")]
    pub autonomy: Tier,
    #[serde(default = "default_step_timeout_s")]
    pub step_timeout_s: u64,
    #[serde(default = "default_max_failures")]
    pub max_failures: u32,
    #[serde(default = "default_stuck_threshold")]
    pub stuck_threshold: u32,
    #[serde(default = "default_coordinator_lease_s")]
    pub coordinator_lease_s: u64,
    /// The engine the DECIDER (supervisor consult) runs on for THIS session — independent of
    /// the WORKER engine. DEFAULTED to Claude, and the direction of that default is a safety
    /// property, exactly like `autonomy`: the decider auto-approves low-stakes decisions, and
    /// Claude is the validated path the decider benchmark covers. A `config.json` that merely
    /// MISSED this field (written before it existed, or by another tool) must read as Claude,
    /// never silently become codex. `pmtui` is the only writer of this file.
    #[serde(default = "default_decider_engine")]
    pub decider_engine: crate::registry::Engine,
    /// The MODEL the decider runs on for THIS session, launch-ready (claude: stable alias or
    /// provider-specific id; codex: slug). `None` (default) keeps the engine's own default: the
    /// claude arm falls back to
    /// `PM_SUPERVISOR_MODEL`/the `SUPERVISOR_MODEL` const, the codex arm omits `-m`. `pmtui` is
    /// the only writer. Defaulting to `None` keeps every legacy config loading.
    #[serde(default)]
    pub decider_model: Option<String>,
}

/// `control.json` — pmtui-owned requests that pmd applies to its ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    #[serde(default)]
    pub human_cadence_s: Option<u64>,
    #[serde(default)]
    pub wake_generation: u64,
}

pub fn read_control(paths: &ProjectPaths) -> Result<Control> {
    read_json_or(&paths.control(), Control::default())
}

pub fn write_control(paths: &ProjectPaths, control: &Control) -> Result<()> {
    write_json_atomic(&paths.control(), control)
}
/// `Standard` — autopilot OFF — because a config that does not SAY it wants hands-off
/// driving has not asked for it. Silence is not consent for a daemon that types into a live
/// agent session.
fn default_autonomy() -> Tier {
    Tier::Standard
}
fn default_step_timeout_s() -> u64 {
    1800
}
fn default_max_failures() -> u32 {
    3
}
fn default_stuck_threshold() -> u32 {
    3
}
fn default_coordinator_lease_s() -> u64 {
    1860
}
/// `Claude` — the decider's validated default. See the field doc: a config that does not SAY
/// codex has not asked for it, and the decider auto-approves, so silence must fall to the path
/// the benchmark covers.
fn default_decider_engine() -> crate::registry::Engine {
    crate::registry::Engine::Claude
}

impl Config {
    /// The renewed lease must outlast a step's hard timeout by a margin, so a
    /// killed pass can't hold a stale lease past the next spawn (spec §9).
    pub fn validate(&self) -> Result<(), String> {
        let minimum_lease = self
            .step_timeout_s
            .checked_add(60)
            .ok_or_else(|| "step_timeout_s is too large to add the 60s lease margin".to_string())?;
        if self.coordinator_lease_s < minimum_lease {
            return Err(format!(
                "coordinator_lease_s ({}) must be >= step_timeout_s + 60 ({})",
                self.coordinator_lease_s, minimum_lease
            ));
        }
        Ok(())
    }
}

/// `driver.json` — daemon-owned observations + the in-flight crash-recovery
/// handle. Written before spawn (`exit_reason: Running`) and updated on
/// termination. The daemon is the sole writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverState {
    /// Which `step.json.id` this record describes.
    pub step_id: u64,
    /// tmux session name the daemon owns for this step.
    pub pane: String,
    pub spawned_at: Epoch,
    /// Frozen at spawn = spawned_at + step_timeout_s.
    pub deadline: Epoch,
    #[serde(default)]
    pub ended_at: Option<Epoch>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub exit_reason: ExitReason,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub observed_at: Epoch,
}

/// One open stop (design §8). The coordinator sets `risk_class`; the daemon's
/// policy re-derives a floor from `kind` as a safety net.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub id: String,
    pub kind: String,
    pub risk_class: RiskClass,
    #[serde(default)]
    pub question: String,
    /// The choices offered alongside `question`, in the order they were listed.
    /// Empty for an open-ended ask; renderers show them numbered when present.
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub context_ref: Option<String>,
    #[serde(default = "default_stop_status")]
    pub status: String,
}
fn default_stop_status() -> String {
    "awaiting_reply".to_string()
}

/// What a human should be shown for `s`: the agent's own words when it wrote any,
/// otherwise the product string for a kind the HARNESS synthesized.
///
/// Lives here, next to [`Stop`], because EVERY surface that shows a stop to a human has
/// to agree: the pmtui `Stops` block, its answer overlay, and — the one that was missed —
/// `escalation::Escalation::for_stops`, which is the desktop notification the human
/// actually receives. That one rendered a bare `kind`, so a synthesized park notified
/// somebody with the body `[stop-…] confirm_done` and nothing else.
pub fn stop_product_text(s: &Stop) -> &str {
    let question = s.question.trim();
    if !question.is_empty() {
        return question;
    }
    synthesized_stop_text(&s.kind).unwrap_or("")
}

/// The product string for a stop the HARNESS synthesized, which by definition carries no
/// draft text (`phase_engine`'s `confirm_done`/`stuck` parks pass `""`; an agent marker
/// that omitted its `question` lands here too, since `question` is `#[serde(default)]`).
///
/// `None` for every kind the harness never synthesizes: a text-less `publish` means the
/// AGENT dropped its question, and no fixed sentence can say WHAT would be published —
/// inventing one would be worse than the bare kind.
///
/// Each string says the three things the kind alone cannot: what the agent decided, that
/// nothing moves until the human acts, and what the act is.
///
/// **Every one is <= 74 characters, and that is a hard constraint, not a style note.** The
/// pmtui `Stops` block truncates a row's TAIL to the pane width, and the measured budget
/// at an ordinary 120-column terminal is 74 — so a longer sentence loses its ACTION half,
/// which is the only part that tells the human what to do. Keep the verb early and the
/// line short. Any key named here must be a real Normal-mode binding (`Enter` is; there is
/// no `p` and no `K` — both were removed at the user's request).
pub fn synthesized_stop_text(kind: &str) -> Option<&'static str> {
    match kind {
        // The Confirming park (`phase_engine`), and any `confirm_done` marker whose
        // one-line summary went missing: the goal LOOKS satisfied, and the harness will
        // not close the session on the agent's word alone.
        "confirm_done" => {
            Some("Agent says the goal is met and stopped. Close it, or say keep going.")
        }
        // Always-`Hard`, and always "the harness will not decide this one": a blocking
        // in-pane dialog, or a supervisor that declined (`job_engine::park_dialog` /
        // `park_advice_refusal`). Those two DO carry the real question — this string is for
        // a `capability` stop that reaches us without one.
        "capability" => Some("Needs your decision — the harness won't make it. Enter to look."),
        // The no-progress dead end. "to retry" is literal, not encouragement:
        // `phase_engine::on_escalated` re-dispatches the SAME phase once the stop is
        // answered, feeding the answer to the worker as its `extra`. Names no key, because
        // `a` is refused on an AgentLoop+Standard row and `d` on an `Auto` row.
        "stuck" => Some("No progress after repeated tries. Answer with a direction to retry."),
        _ => None,
    }
}

/// One entry in the answer inbox (`answers.json`), appended by the daemon when
/// the human resolves an escalation (or when the safety-net auto-flows), and
/// read by the next step. `answered_by == "auto_flow"` marks a machine decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub stop_id: String,
    pub answer: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default = "default_answered_by")]
    pub answered_by: String,
    pub answered_at: Epoch,
}
fn default_answered_by() -> String {
    "user".to_string()
}

/// `session.json` — a `pmtui`-owned record of the interactive `claude`/`codex`
/// session it launched for a project, so the row can show it and `Enter` can
/// re-find it to attach. Liveness is confirmed against tmux, not this file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The private tmux server socket the session lives on (`tmux -L <socket>`).
    pub socket: String,
    /// The tmux session name to attach to.
    pub tmux_session: String,
    /// Which CLI was launched (`claude` / `codex`), for display.
    pub engine: String,
    pub started_at: Epoch,
}

/// Persist the daemon's driver record.
pub fn write_driver(paths: &ProjectPaths, driver: &DriverState) -> Result<()> {
    write_json_atomic(&paths.driver(), driver)
}

/// Retention window for consumed human answers in `answers.json`. An answer older than
/// this (relative to the newest answer) is DEAD: `JobScheduler::on_blocked` only ever
/// matches answers whose `answered_at >=` the CURRENT park's `since`, and every future
/// park's `since` only increases — so an answer this old can never be delivered again.
/// Pruning them on append is what keeps `answers.json` from growing unboundedly over a
/// long-lived session (it is otherwise append-only and re-parsed every sweep).
pub const ANSWERS_RETENTION_S: Epoch = 14 * 24 * 60 * 60; // 14 days
/// Hard backstop on the answer count regardless of age — bounds a burst of answers that
/// all land inside the retention window.
pub const ANSWERS_MAX: usize = 500;

/// Drop stale answers in place: anything older than [`ANSWERS_RETENTION_S`] before the
/// newest entry, then (as a backstop) the oldest beyond [`ANSWERS_MAX`]. Answers are
/// appended in time order, so `drain(0..)` sheds the oldest; the newest are always kept.
fn prune_answers(answers: &mut Vec<Answer>) {
    // Reference "now" is the newest answer (max, to be robust to any clock skew), not the
    // wall clock — this function is pure and needs no `Clock`.
    let Some(newest) = answers.iter().map(|a| a.answered_at).max() else {
        return;
    };
    let cutoff = newest - ANSWERS_RETENTION_S;
    answers.retain(|a| a.answered_at >= cutoff);
    if answers.len() > ANSWERS_MAX {
        let drop = answers.len() - ANSWERS_MAX;
        answers.drain(0..drop);
    }
}

/// Append an answer to the inbox (read-modify-write; single-writer via lease).
///
/// Also PRUNES stale answers (see [`prune_answers`]) so the file stays bounded. pmtui is
/// the sole writer of `answers.json` (the single-writer invariant — pmd only READS it), so
/// rewriting the pruned set here is race-free.
pub fn append_answer(paths: &ProjectPaths, answer: &Answer) -> Result<()> {
    let mut answers: Vec<Answer> = read_json_or(&paths.answers(), Vec::new())?;
    answers.push(answer.clone());
    prune_answers(&mut answers);
    write_json_atomic(&paths.answers(), &answers)
}

#[cfg(test)]
mod answer_prune_tests {
    use super::*;

    fn ans(stop: &str, at: Epoch) -> Answer {
        Answer {
            stop_id: stop.to_string(),
            answer: "ok".to_string(),
            note: None,
            answered_by: "user".to_string(),
            answered_at: at,
        }
    }

    #[test]
    fn prune_drops_only_answers_older_than_the_retention_window() {
        let now: Epoch = 1_700_000_000;
        // Appended in time order (oldest first), like the real file.
        let mut v = vec![
            ans("old", now - ANSWERS_RETENTION_S - 1), // just past the window → dropped
            ans("edge", now - ANSWERS_RETENTION_S),    // exactly at the window → kept
            ans("recent", now - 10),                   // kept
            ans("newest", now),                        // the reference → kept
        ];
        prune_answers(&mut v);
        let ids: Vec<&str> = v.iter().map(|a| a.stop_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["edge", "recent", "newest"],
            "only the answer older than the retention window is pruned"
        );
    }

    #[test]
    fn prune_caps_the_count_dropping_the_oldest() {
        let now: Epoch = 1_700_000_000;
        // ANSWERS_MAX + 50 answers, all within the window (1s apart), oldest first.
        let n = ANSWERS_MAX + 50;
        let mut v: Vec<Answer> = (0..n)
            .map(|i| ans(&format!("s{i}"), now - (n as Epoch) + i as Epoch))
            .collect();
        prune_answers(&mut v);
        assert_eq!(v.len(), ANSWERS_MAX, "count is capped to ANSWERS_MAX");
        assert_eq!(
            v.last().unwrap().stop_id,
            format!("s{}", n - 1),
            "the newest answer is retained"
        );
        assert_eq!(
            v.first().unwrap().stop_id,
            format!("s{}", n - ANSWERS_MAX),
            "the oldest beyond the cap were dropped"
        );
    }

    #[test]
    fn prune_is_a_noop_below_both_bounds() {
        let now: Epoch = 1_700_000_000;
        let mut v = vec![ans("a", now - 100), ans("b", now)];
        let before = v.clone();
        prune_answers(&mut v);
        assert_eq!(v, before, "a small, recent set is left untouched");
    }
}

#[cfg(test)]
mod decider_engine_tests {
    use super::*;
    use crate::registry::Engine;

    #[test]
    fn a_config_without_decider_engine_defaults_to_claude() {
        // A config written before this field existed (or by another tool) must still load,
        // and must read as Claude — the safe/validated default. This is a safety property:
        // the decider auto-approves, so an unknown/missing engine must NOT silently become codex.
        let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860}"#;
        let cfg: Config = serde_json::from_str(json).expect("legacy config must still parse");
        assert_eq!(cfg.decider_engine, Engine::Claude);
    }

    #[test]
    fn decider_engine_round_trips_codex() {
        let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"codex"}"#;
        let cfg: Config =
            serde_json::from_str(json).expect("config with decider_engine must parse");
        assert_eq!(cfg.decider_engine, Engine::Codex);
        let back = serde_json::to_string(&cfg).unwrap();
        assert!(back.contains("\"decider_engine\":\"codex\""), "got {back}");
    }

    #[test]
    fn config_without_decider_model_loads_as_none() {
        let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"codex"}"#;
        let cfg: Config = serde_json::from_str(json).expect("legacy config must parse");
        assert_eq!(cfg.decider_model, None);
    }

    #[test]
    fn decider_model_round_trips() {
        let json = r#"{"autonomy":"autopilot","step_timeout_s":1800,"max_failures":3,"stuck_threshold":3,"coordinator_lease_s":1860,"decider_engine":"claude","decider_model":"global.anthropic.claude-opus-5"}"#;
        let cfg: Config = serde_json::from_str(json).expect("must parse");
        assert_eq!(
            cfg.decider_model.as_deref(),
            Some("global.anthropic.claude-opus-5")
        );
        assert!(
            serde_json::to_string(&cfg)
                .unwrap()
                .contains("\"decider_model\":\"global.anthropic.claude-opus-5\"")
        );
    }
}
