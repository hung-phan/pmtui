//! The persistent session's own lifecycle: launch the ONE interactive agent when it
//! is not alive, and decide WHICH conversation it runs on (the ledger's id, else an
//! adopted registry seed, else a freshly minted uuid). They are one unit because
//! you cannot launch without first answering "which conversation is this?", and the
//! uuid minting is here because pinning a `--session-id` is the only reason this
//! harness needs a uuid at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::clock::Epoch;
use crate::job::{AgentLoopState, AutopilotEventKind, JobRun};
use crate::registry::Engine;
use crate::tmux::Driver;
use crate::worker::{self, Resume};

use super::JobScheduler;

/// Cold-start grace after launching the persistent session: the launching tick parks
/// `Monitoring` this many seconds out and does NOT nudge, so a still-booting `claude`
/// (no prompt yet) is never typed into mid-startup.
pub(super) const LAUNCH_GRACE_S: i64 = 8;

/// Outcome of [`JobScheduler::ensure_session`]: whether this tick launched the
/// persistent session (so the caller applies cold-start grace and does NOT nudge) or
/// found it already alive (so the caller may nudge if the pane is idle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnsureOutcome {
    JustLaunched,
    AlreadyUp,
}

impl JobScheduler {
    /// Resolve the conversation id the persistent session runs on and, if it isn't
    /// already alive, launch it. Write-ordering (single-writer-ledger invariant): the
    /// `driver.json` handle is recorded BEFORE the ledger `run` flip, and if the
    /// ledger save fails the just-launched session is terminated so no live process
    /// exists that the ledger doesn't know about. Returns `JustLaunched` (caller
    /// applies cold-start grace) or `AlreadyUp`.
    pub(super) fn ensure_session(
        &mut self,
        driver: &dyn Driver,
        now: Epoch,
        base: &AgentLoopState,
    ) -> Result<EnsureOutcome> {
        let session = self.loop_session();
        let alive = driver.is_alive(&session).unwrap_or(false);
        // Install the worker skill for THIS session BEFORE the already-up early return, so an
        // already-alive ADOPTED pane still gets the file on the first sweep — a session pmd did
        // not itself launch is never left without it. Idempotent: (re)write on a launch so it
        // stays current with the shipped body. This is a plain file write with NO
        // ledger/driver.json interaction, so slotting it here preserves the
        // driver.json-before-ledger write ordering the launch path below relies on. NEVER touches
        // the user's `AGENTS.md`.
        // Retry a failed install; once it succeeds the in-memory flag keeps ordinary sweeps free of
        // repeated file I/O.
        if !alive || !self.worker_skill_installed {
            self.install_worker_skill();
        }
        if alive {
            return Ok(EnsureOutcome::AlreadyUp);
        }
        // The spawn skill ships only with a terminal pmd itself starts, and only when that
        // terminal names pmtui (`PMTUI_BIN`): an adopted pane keeps its launcher's environment.
        self.install_spawn_skill();
        // Resolve (and, for a first-time claude session with no id, mint+pin) the
        // conversation id, persisting any mint/adopt onto the ledger below. The
        // returned [`Resume`] distinguishes a fresh mint (CREATE via `--session-id`)
        // from an existing id / adopted seed (RESUME via `--resume`), so a relaunch
        // after any death/restart never re-CREATEs an existing conversation.
        let mut next = base.clone();
        let resume = self.resolve_conversation_id(&mut next);
        // The turn-end hook (M71) is injected UNLESS disabled via `PM_TURN_HOOK`
        // (=off/0/false) — a kill-switch, because a claude/codex build that rejected the
        // injected `--settings`/`-c` flag would fail the LAUNCH itself, which is worse than
        // the busy-detection bug it fixes; with the hook off the daemon degrades to the
        // `idle_fingerprint` content-stability gate. When on, the engine appends one byte to
        // `paths.turn_signal()` on every completed turn (see `worker::build_loop_command`).
        let turn_signal = self.paths.turn_signal();
        let hook_on = worker::turn_hook_enabled();
        // Directory trust remains the engine's own prompt. pmd never injects a
        // profile or modifies the user's Codex configuration.
        let argv = worker::build_loop_command(
            self.engine,
            &resume,
            std::slice::from_ref(&self.work_dir),
            hook_on.then_some(turn_signal.as_path()),
            self.worker_model.as_deref(),
        );
        // The old pane's decision is stale before a replacement worker is started. Termination
        // must succeed first; otherwise retry later without exposing a new pane to old advice.
        if let Some(abandoned) = self.abandon_advice(driver, base)? {
            next.finish_decider_run(
                abandoned.decider_seq,
                now,
                crate::job::DeciderOutcome::Interrupted {
                    reason: "the worker terminal relaunched before the decider returned".into(),
                },
            );
            if let Some(report_seq) = abandoned.report_seq {
                next.finish_turn_review(report_seq, crate::job::TurnReviewOutcome::Interrupted);
            }
        }
        driver
            .launch_interactive(&session, &self.work_dir, &argv, &self.managed_env())
            .with_context(|| {
                format!(
                    "launch persistent loop session {session} for {}",
                    self.project_id
                )
            })?;
        // Record the live handle in `driver.json` BEFORE flipping the ledger `run`.
        self.write_driver_running(0, &session, now, now);
        // Cold-start grace + persist the (possibly minted) conversation id in ONE
        // terminate-guarded save.
        let until = now + LAUNCH_GRACE_S;
        next.run = JobRun::Monitoring { until };
        next.updated_at = now;
        // Void any "awaiting a report from my last nudge" hold on the LEDGER: the nudge that set
        // the nudge-generation debt belongs to the PREVIOUS (now dead) pane, so
        // `base.awaiting_report()` (the
        // marker hasn't advanced past that nudge) would otherwise stay true across the relaunch
        // and HOLD the fresh agent — never nudging it, then escalating a misleading "busy with no
        // progress" Stuck after `DEFAULT_STALL_BUSY_S`. A just-booted agent owes no report yet.
        next.nudged_at_seq = None;
        next.nudged_at_report_generation = None;
        next.finish_turn_without_report(now, crate::job::TurnNoReportReason::Relaunched);
        next.record_event(now, AutopilotEventKind::Launched);
        if let Err(e) = self.save_ledger(&mut next) {
            let _ = driver.terminate(&session);
            return Err(e).with_context(|| {
                format!(
                    "persist ledger after launching loop session for {}",
                    self.project_id
                )
            });
        }
        self.run = JobRun::Monitoring { until };
        // Fresh session (cold start): any prior stall window is void — a just-launched
        // agent has made no observations to stall on yet — and so is any armed
        // confirmation: Idle observations of the PREVIOUS (now dead) pane say nothing
        // about the booting one, which must earn both of its own.
        self.busy_since = None;
        self.reset_idle_gate();
        // A freshly-booted agent is idle at its prompt regardless of how many turns the
        // PREVIOUS incarnation completed, so drop the turn-count baseline (the signal FILE
        // persists across a relaunch; without this, a stale baseline equal to the file size
        // would read as "still mid-turn" and the boot prompt would never get its first
        // nudge). `turn_signal_status` then reports `Unknown` until the first nudge
        // re-baselines it — the fingerprint fallback covers that one decision.
        self.turns_at_nudge = None;
        Ok(EnsureOutcome::JustLaunched)
    }

    /// Write the canonical worker `SKILL.md`; Claude additionally gets its exact per-skill alias.
    /// A failure is retried on a later sweep. NEVER creates/modifies the user's `AGENTS.md`.
    fn install_worker_skill(&mut self) {
        let canonical = self.paths.canonical_worker_skill_file();
        let canonical_parent_ready = canonical
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .is_ok();
        let canonical_written = canonical_parent_ready
            && crate::state::write_text_atomic(&canonical, crate::skills::WORKER_SKILL_MD).is_ok();
        let engine_ready = match self.engine {
            crate::registry::Engine::Claude => {
                crate::skills::ensure_claude_worker_skill_link(&self.paths).is_ok()
            }
            crate::registry::Engine::Codex => true,
        };
        self.worker_skill_installed = canonical_written && engine_ready;
        if !self.worker_skill_installed {
            eprintln!(
                "worker-skill native install failed for {} ({})",
                self.project_id,
                self.engine.label()
            );
        }
    }

    /// Ship the pmtui-spawn skill with the terminal about to launch when pmd names pmtui in it.
    /// Best effort: a failure is logged and never blocks the launch; `pmd doctor` reports the
    /// missing or stale skill. NEVER creates/modifies the user's `AGENTS.md`.
    fn install_spawn_skill(&self) {
        if let Err(error) = crate::skills::install_spawn_skill_for_launch(
            &self.work_dir,
            self.engine,
            &self.managed_env(),
        ) {
            eprintln!(
                "spawn-skill install failed for {} ({}): {error:#}",
                self.project_id,
                self.engine.label()
            );
        }
    }

    /// Resolve how the persistent session anchors its conversation, writing any
    /// mint/adopt onto `next` (the ledger to be persisted) and reporting CREATE vs
    /// RESUME via [`Resume`]:
    /// - the ledger's own `conversation_id`: RESUMES (`Continue`) when CONFIRMED — this
    ///   covers a relaunch after the session died and a pmd restart (restore → re-drive),
    ///   so a proven conversation is always revivable, never re-created. But when
    ///   `resume_unconfirmed` is set it CREATES (`Fresh{Some}`) once (see the fallback
    ///   below), then clears the flag;
    /// - else an adopted registry seed is resolved by PROBING claude's transcript store
    ///   ([`JobScheduler::seed_conversation_exists`]): confidently ABSENT ⇒ CREATE the seed
    ///   directly (`Fresh{Some}`, no doomed resume — the create→autopilot-without-chatting
    ///   case); EXISTS or unknown ⇒ RESUME (`Continue`) marked UNCONFIRMED (the human's chat
    ///   may have created it — preserve that context — and the one-shot fallback below covers
    ///   a resume that turns out dead);
    /// - else claude mints+pins a fresh uuid and CREATES it (`Fresh{Some}`); codex has
    ///   no caller-chosen id, so a fresh interactive session (`Fresh{None}`) whose id
    ///   is captured later.
    ///
    /// THE ONE-SHOT RESUME→CREATE FALLBACK (state machine). `ensure_session` returns
    /// `AlreadyUp` BEFORE calling this whenever the tmux session is alive, so this is
    /// reached ONLY when the session is NOT alive. Therefore reaching the ledger-cid
    /// branch with `resume_unconfirmed == true` means the PRIOR seed-resume launch is
    /// dead — a create→autopilot seed that claude rejected with "No conversation found"
    /// and exited on, which tmux tore down. So: tick A (not alive, cid None, seed) →
    /// RESUME + set unconfirmed; claude dies; tick B (not alive, cid Some, unconfirmed)
    /// → CREATE (`--session-id <seed>`) + clear unconfirmed; claude creates it and runs.
    /// If the seed WAS real (chat had turned), the first resume stays alive → `AlreadyUp`
    /// → this never re-resolves, and a later marker bump clears the flag (`dispose_report`).
    /// Self-heals the rare created-but-crashed-pre-turn case: the create hits "already in
    /// use", dies, but the flag is already cleared, so the next relaunch resumes.
    fn resolve_conversation_id(&mut self, next: &mut AgentLoopState) -> Resume {
        if let Some(id) = next.conversation_id.clone() {
            // Reached only when the session is NOT alive (AlreadyUp early-returns above),
            // so an unconfirmed cid here means the prior seed-resume launch died: the seed
            // named a conversation that does not exist. Fall back to CREATE with the same
            // id, ONCE — clear the flag so the next relaunch resumes rather than looping
            // create→create. A confirmed cid always RESUMES (the common relaunch / pmd-
            // restart / post-turn path; re-creating a live conversation would error).
            if next.resume_unconfirmed {
                next.resume_unconfirmed = false;
                return Resume::Fresh {
                    session_id: Some(id),
                };
            }
            return Resume::Continue(id);
        }
        if let Some(seed) = self.registry_seed.clone() {
            // Adopt the human's chat seed. Prefer a DETERMINISTIC answer over the
            // optimistic resume: probe claude's on-disk transcript store for this
            // conversation (see `seed_conversation_exists`). This is what removes the
            // 1-2 visible "No conversation found" launches on the first Standard→autopilot
            // switch — instead of always resuming and falling back on death, we launch the
            // right command the first time.
            next.conversation_id = Some(seed.clone());
            match self.seed_conversation_exists(&seed) {
                // Confidently ABSENT (the projects dir exists but this transcript does
                // not) — a create→autopilot seed that was never chatted into. CREATE it
                // directly; no doomed resume. Leave `resume_unconfirmed` FALSE so that if
                // this create somehow does not take, the next relaunch RESUMES rather than
                // re-CREATING — never the create→create loop this whole area guards against.
                Some(false) => Resume::Fresh {
                    session_id: Some(seed),
                },
                // The transcript EXISTS (the human chatted — resume to keep that context)
                // OR existence can't be determined (probe error, no `projects` dir yet, or
                // codex). RESUME, but mark UNCONFIRMED so a dead resume falls back to CREATE
                // exactly once via the one-shot state machine in the ledger-cid branch above.
                _ => {
                    next.resume_unconfirmed = true;
                    Resume::Continue(seed)
                }
            }
        } else {
            self.mint_or_fresh(next)
        }
    }

    /// The tail of [`JobScheduler::resolve_conversation_id`] for a session with NO ledger
    /// cid and NO registry seed: claude mints+pins a fresh uuid and CREATES it; codex has
    /// no caller-chosen id, so a fresh interactive session whose id is captured later.
    fn mint_or_fresh(&self, next: &mut AgentLoopState) -> Resume {
        match self.engine {
            // A freshly minted uuid is CREATED, never a stale seed, so it stays CONFIRMED
            // (`resume_unconfirmed` untouched at its `false` default): a later relaunch
            // resumes it.
            Engine::Claude => {
                let id = mint_uuid_v4();
                next.conversation_id = Some(id.clone());
                Resume::Fresh {
                    session_id: Some(id),
                }
            }
            // Codex cannot be HANDED an id, and is not given one here either: by the time
            // `ensure_session` runs, `JobScheduler::tick` has already reconciled whatever its turn
            // hook reported into the ledger, so a session with a known conversation takes the
            // ledger branch above and never arrives here. Reaching this arm means no id has been
            // reported yet — no turn has completed — and a fresh interactive session is the only
            // honest answer. ONE adoption point; a second one here was unreachable, which is
            // exactly what a mutation test showed.
            Engine::Codex => Resume::Fresh { session_id: None },
        }
    }

    /// Does claude's on-disk transcript for conversation `id` (under THIS session's
    /// `work_dir`) exist? Answers the "did the human ever chat into this seed?" question
    /// deterministically, so [`JobScheduler::resolve_conversation_id`] can launch the
    /// right command the first time instead of optimistically resuming and failing:
    ///
    /// - `Some(true)`  — the transcript file is present ⇒ a real conversation ⇒ RESUME.
    /// - `Some(false)` — claude's `projects` dir is present but this transcript is not ⇒
    ///   a confident "never chatted" ⇒ CREATE directly (no failed `--resume`).
    /// - `None`        — can't say confidently (no `projects` dir yet, an IO error, or a
    ///   non-claude engine) ⇒ the caller keeps the optimistic-resume + one-shot fallback.
    ///
    /// The probe is advisory: it can only ever REMOVE a failed launch, never add one — a
    /// wrong `Some(false)` (e.g. a future slug-encoding change) at worst makes the create
    /// hit "already in use" and self-heal into a resume next relaunch (the same ≤1-failure
    /// path as before), and it is NEVER consulted for a CONFIRMED ledger cid.
    ///
    /// `pub(crate)` so the probe's three outcomes can be unit-tested directly (a single
    /// FakeDriver tick can't distinguish the `Some(true)` resume arm from the `None`
    /// fallback — both resume — so the resolve-level test alone under-covers it).
    pub(crate) fn seed_conversation_exists(&self, id: &str) -> Option<bool> {
        // Delegates to the free [`claude_conversation_exists`] so the daemon and pmtui's
        // manual-resume probe share ONE definition of "does this transcript exist" — a
        // divergence there is exactly the class of bug (harness misreading on-disk state)
        // that has bitten this project before.
        claude_conversation_exists(self.claude_home.as_deref(), &self.work_dir, self.engine, id)
    }
}

/// claude's `projects` directory for a given home override: `claude_home` if set, else
/// `CLAUDE_CONFIG_DIR`, else `$HOME/.claude` — each with `projects` appended. `None` when
/// `$HOME` is unset and no override/`CLAUDE_CONFIG_DIR` is present.
fn claude_projects_dir_at(claude_home: Option<&Path>) -> Option<PathBuf> {
    let home = if let Some(h) = claude_home {
        h.to_path_buf()
    } else if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(std::env::var("HOME").ok()?).join(".claude")
    };
    Some(home.join("projects"))
}

/// Does claude have an on-disk transcript for conversation `id` started in `work_dir`? The
/// free-function form of [`JobScheduler::seed_conversation_exists`] (which delegates here),
/// so pmtui's MANUAL-resume path ([`crate`]'s `App::start_undriven_session`) can make the
/// same resume-vs-create decision the daemon does: a recorded conversation id whose
/// transcript is GONE must be CREATED with `--session-id`, not `--resume`d into a dead/empty
/// session — the dashboard bug where pressing Enter to resume a paused Standard row that
/// pointed at a ghost id did nothing.
///
/// `claude_home` overrides the projects-dir base (for tests); `None` → `CLAUDE_CONFIG_DIR`
/// → `$HOME/.claude`. Outcomes:
/// - `Some(true)`  — the transcript file is present ⇒ RESUME.
/// - `Some(false)` — the `projects` dir is present but this transcript is not ⇒ a confident
///   "never chatted / ghost id" ⇒ CREATE directly (no failed `--resume`).
/// - `None`        — can't say confidently (codex, no `projects` dir yet, or an IO error) ⇒
///   the caller keeps the optimistic-resume path.
///
/// Advisory: it can only ever turn a doomed `--resume` into a create, never the reverse.
pub fn claude_conversation_exists(
    claude_home: Option<&Path>,
    work_dir: &Path,
    engine: Engine,
    id: &str,
) -> Option<bool> {
    if engine != Engine::Claude {
        // codex stores rollouts elsewhere (no per-id claude transcript); fall back.
        return None;
    }
    let projects = claude_projects_dir_at(claude_home)?;
    // Only a PRESENT projects dir yields a confident answer. If claude has never run on this
    // machine (or the path is unreadable) we defer to the optimistic path.
    if !projects.is_dir() {
        return None;
    }
    let file = projects
        .join(claude_project_slug(work_dir))
        .join(format!("{id}.jsonl"));
    file.try_exists().ok()
}

/// claude derives a conversation's transcript directory from its cwd by replacing every
/// non-alphanumeric character with `-` (e.g. `/tmp/p.q` → `-tmp-p-q`,
/// `/workplace/phahng/agent-manager` → `-workplace-phahng-agent-manager`). Verified against
/// a live `~/.claude/projects` store. `pub(crate)` so the probe unit test can assert the
/// exact encoding without a live claude.
pub(crate) fn claude_project_slug(work_dir: &Path) -> String {
    work_dir
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Mint a version-4-formatted UUID for a claude `--session-id`, without pulling in
/// a crate: 16 bytes of best-effort entropy (nanos ^ pid ^ a process-lifetime
/// counter, mixed through splitmix64) with the version/variant bits set. Uniqueness
/// among the sessions one machine mints is all that is required (the id is our
/// chosen resume anchor, not a security token). `pub` so pmtui can mint a
/// byte-compatible `claude --session-id` for its create-and-chat path (S2).
pub fn mint_uuid_v4() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let ctr = CTR
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let seed = nanos ^ ((std::process::id() as u64) << 32) ^ ctr;
    let a = splitmix64(seed);
    let b = splitmix64(seed ^ 0xD1B5_4A32_D192_ED03);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_le_bytes());
    bytes[8..].copy_from_slice(&b.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 10xx
    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
