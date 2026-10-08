//! Path resolution for a project's state: [`ProjectPaths`] turns a project root (and,
//! for an agent-loop session, a session id) into the well-known file names under
//! `.project-state/`. Pure path work with no I/O, kept together so the layout of that
//! directory is decided in exactly one place.

use std::path::PathBuf;

/// Directory name holding a project's durable state.
pub const STATE_DIR: &str = ".project-state";

/// Resolves the well-known file paths under a project root.
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub root: PathBuf,
    /// When set, state resolves under `.project-state/sessions/<seg>/` instead of
    /// `.project-state/` — so multiple agent-loop sessions can share one `root`
    /// (project folder) without clobbering each other. `root` stays the real
    /// project folder (the worker's cwd). `None` for phase/interactive projects.
    session_seg: Option<String>,
}

impl ProjectPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            session_seg: None,
        }
    }
    /// Paths for one agent-loop SESSION under a shared project `root`: state lives
    /// at `<root>/.project-state/sessions/<sanitized id>/` while `root` stays the
    /// worker's cwd. Lets multiple sessions coexist in one folder, each with its
    /// own ledger, driver.json, lease, and done-signals.
    pub fn for_session(root: impl Into<PathBuf>, session_id: &str) -> Self {
        // Sanitize for a readable prefix AND append a hash of the RAW id, so two
        // distinct ids that sanitize alike (e.g. "a.b" vs "a/b") never share a
        // subtree — mirroring `tmux::job_session_name`'s disambiguation.
        let seg = format!("{}-{}", sanitize_session_seg(session_id), fnv8(session_id));
        Self {
            root: root.into(),
            session_seg: Some(seg),
        }
    }
    pub fn state_dir(&self) -> PathBuf {
        let base = self.root.join(STATE_DIR);
        match &self.session_seg {
            Some(seg) => base.join("sessions").join(seg),
            None => base,
        }
    }
    pub fn config(&self) -> PathBuf {
        self.state_dir().join("config.json")
    }
    /// Human-owned runtime requests consumed by pmd.
    pub fn control(&self) -> PathBuf {
        self.state_dir().join("control.json")
    }
    /// `state.json` — the harness ledger (spec §5), written only by the harness.
    pub fn pmstate(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }
    pub fn driver(&self) -> PathBuf {
        self.state_dir().join("driver.json")
    }
    pub fn stops(&self) -> PathBuf {
        self.state_dir().join("stops.json")
    }
    pub fn answers(&self) -> PathBuf {
        self.state_dir().join("answers.json")
    }
    /// `brief.md` — the confirmed ask + goal/constraints, written at intake and
    /// quoted by every later phase prompt. Stable after intake.
    pub fn brief(&self) -> PathBuf {
        self.state_dir().join("brief.md")
    }
    /// `directive.md` — a standing, restrictive-only operating-directive from the human
    /// who owns the session (e.g. "stop auto-approving test edits"). Read fresh at consult
    /// time into `Consult::directive` and surfaced as the trusted DIRECTIVE fence; NEVER a
    /// nudge input (the worker-nudge firewall). Empty/absent ⇒ no directive. Sibling of
    /// `brief.md`; the human sets and rescinds it from pmtui.
    pub fn directive(&self) -> PathBuf {
        self.state_dir().join("directive.md")
    }
    /// `decisions.md` — the source-attributed decision log the harness feeds back
    /// into each phase worker as binding context.
    pub fn decisions(&self) -> PathBuf {
        self.state_dir().join("decisions.md")
    }
    /// `raw.jsonl` — the full-fidelity machine JSONL of every disposed decision (Milestone
    /// B). Append-only, size-rotated; the harness is the sole writer. Any reader MUST parse
    /// leniently and tolerate a torn final line (no append-atomic primitive exists).
    pub fn raw_jsonl(&self) -> PathBuf {
        self.state_dir().join("raw.jsonl")
    }
    /// The single rotated generation of [`Self::raw_jsonl`] (`raw.jsonl.1`).
    pub fn raw_jsonl_rotated(&self) -> PathBuf {
        self.state_dir().join("raw.jsonl.1")
    }
    /// The single rotated generation of [`Self::decisions`] (`decisions.md.1`), so even the
    /// sparse human file cannot grow unbounded over a multi-day session.
    pub fn decisions_rotated(&self) -> PathBuf {
        self.state_dir().join("decisions.md.1")
    }
    /// Per-step artifacts directory (done-signals and logs).
    pub fn steps_dir(&self) -> PathBuf {
        self.state_dir().join("steps")
    }
    pub fn done_signal(&self, step_id: u64) -> PathBuf {
        self.steps_dir().join(format!("{step_id}.done"))
    }
    pub fn step_log(&self, step_id: u64) -> PathBuf {
        self.steps_dir().join(format!("{step_id}.log"))
    }
    /// Done-signal for supervisor consult `seq` (m20). A namespace of its own
    /// (`advice-<seq>.done`) rather than [`ProjectPaths::done_signal`]'s, so a consult
    /// and a phase step can never read each other's exit code.
    pub fn advice_done_signal(&self, seq: u64) -> PathBuf {
        self.steps_dir().join(format!("advice-{seq}.done"))
    }
    /// Tee'd combined output of supervisor consult `seq` — the ONLY place its reply is
    /// read from (the harness never trusts the pane for this).
    pub fn advice_log(&self, seq: u64) -> PathBuf {
        self.steps_dir().join(format!("advice-{seq}.log"))
    }
    /// Where a **codex** decider consult writes its final message (`--output-last-message`),
    /// which the reap reads as the verdict. Its own `advice-<seq>.last` namespace, like
    /// [`ProjectPaths::advice_log`], so a consult's verdict file can never collide with its log
    /// or done-signal. (The claude path reads its verdict from the tee'd log and never touches
    /// this file.)
    pub fn advice_last_message(&self, seq: u64) -> PathBuf {
        self.steps_dir().join(format!("advice-{seq}.last"))
    }

    /// A spawned JOB child's own four files, each in its own namespace under `steps/` so a job can
    /// never read a phase step's or a consult's exit code by accident. One session owns at most one
    /// job — it IS the job — so none of these is sequenced.
    ///
    /// The job is a one-shot headless run wrapped by [`crate::tmux::Driver::spawn_step`]: the wrapper
    /// tees combined output into [`ProjectPaths::job_log`] and lands the run's exit code in
    /// [`ProjectPaths::job_done_signal`], which is what makes "the job ended" a fact rather than an
    /// inference about liveness.
    pub fn job_done_signal(&self) -> PathBuf {
        self.steps_dir().join("job.done")
    }
    /// Tee'd combined output of the job run: the stream the result is read from, and the tail the
    /// dashboard shows while it runs.
    pub fn job_log(&self) -> PathBuf {
        self.steps_dir().join("job.log")
    }
    /// The JSON Schema that shapes a job's final payload into the fields a receipt carries.
    ///
    /// This PATH is codex's (`--output-schema` takes a file). claude is handed the schema DOCUMENT inline,
    /// because `--json-schema` rejects a path — so a claude job writes this file and never reads it.
    pub fn job_schema(&self) -> PathBuf {
        self.steps_dir().join("job-schema.json")
    }
    /// Where **codex** writes its final message (`-o`), read as the job's result. The claude path
    /// reads its result from the tee'd log and never touches this file.
    pub fn job_last_message(&self) -> PathBuf {
        self.steps_dir().join("job.last")
    }
    /// Daemon working directory (wrapper scripts, etc.).
    pub fn daemon_dir(&self) -> PathBuf {
        self.state_dir().join(".daemon")
    }
    /// Per-session chat marker (`{daemon_dir}/chat.json`) — written by `pmtui`
    /// while a human is attached to this session's interactive REPL, and read by
    /// the daemon's pre-spawn interlock (`crate::chat_lock`) so the poll does not
    /// launch a wake that would collide with the human's resume of the SAME
    /// conversation id. Per-session (under `daemon_dir()`) so sessions sharing one
    /// project folder never collide.
    pub fn chat_lock(&self) -> PathBuf {
        self.daemon_dir().join("chat.json")
    }
    /// Short critical section around one complete pane input (paste + Enter).
    pub fn input_lock(&self) -> PathBuf {
        self.daemon_dir().join("input.lock")
    }
    /// Per-session TURN-COMPLETE signal (`{daemon_dir}/turn-complete`), APPENDED to (one
    /// byte per completed turn) by the launched engine's turn-end hook — claude's `Stop`
    /// hook, codex's `notify` on `agent-turn-complete` (both injected in
    /// `worker::build_loop_command`). The daemon never writes it; it reads the file's SIZE
    /// as a monotonic count of completed turns and compares it against the count at its last
    /// nudge to know the agent has gone idle (a definitive signal the pane heuristic cannot
    /// give mid-stream). ABSENT ⇒ the hook never fired (old build / misconfig / not-yet-run)
    /// ⇒ the daemon falls back to the `idle_fingerprint` content-stability gate. Per-session
    /// (under `daemon_dir()`) so folder-sharing sessions never collide.
    pub fn turn_signal(&self) -> PathBuf {
        self.daemon_dir().join("turn-complete")
    }
    /// The persistent agent's decision marker (Slice 2). At any decision point the agent OVERWRITES this with a
    /// `WakeReport` JSON carrying a monotonic `seq`. The harness only STATS + READS it — the agent is the SOLE
    /// writer (inverse of the single-writer ledger). Sibling of the per-session `state.json`.
    pub fn needs_you(&self) -> PathBuf {
        self.state_dir().join("needs-you.json")
    }
    /// Bounded continuity state written only by the persistent worker. It is supplemental memory,
    /// never the human-owned goal or pmd's authoritative ledger.
    pub fn checkpoint(&self) -> PathBuf {
        self.state_dir().join("checkpoint.json")
    }
    /// Record of a live interactive session, written by `pmtui` (design: the TUI
    /// owns interactive sessions).
    pub fn session(&self) -> PathBuf {
        self.state_dir().join("session.json")
    }
    /// The canonical skills directory shared by Claude and Codex.
    pub fn canonical_skills_dir(&self) -> PathBuf {
        self.root.join(".agents/skills")
    }
    /// The canonical project skill file shared by Claude and Codex.
    pub fn canonical_worker_skill_file(&self) -> PathBuf {
        self.root.join(crate::skills::WORKER_SKILL_REL_PATH)
    }
    /// The project-local Claude configuration directory.
    pub fn claude_root_dir(&self) -> PathBuf {
        self.root.join(".claude")
    }
    /// Claude's project skills directory.
    pub fn claude_skills_dir(&self) -> PathBuf {
        self.root.join(crate::skills::CLAUDE_SKILLS_REL_PATH)
    }
    /// Claude's compatibility skill-directory symlink for the worker skill.
    pub fn claude_project_skill_dir(&self) -> PathBuf {
        self.claude_skill_dir(crate::skills::WORKER_SKILL_NAME)
    }
    /// Claude's compatibility skill-directory symlink for the shipped skill `name`.
    pub fn claude_skill_dir(&self, name: &str) -> PathBuf {
        self.claude_skills_dir().join(name)
    }
    /// The canonical pmtui-spawn skill file, shared by Claude and Codex and by every session in
    /// this project root.
    pub fn canonical_spawn_skill_file(&self) -> PathBuf {
        self.root.join(crate::skills::SPAWN_SKILL_REL_PATH)
    }
    /// Claude's compatibility skill-directory symlink for the pmtui-spawn skill.
    pub fn claude_spawn_skill_dir(&self) -> PathBuf {
        self.claude_skill_dir(crate::skills::SPAWN_SKILL_NAME)
    }
    /// Claude's project skill file as reached through its compatibility symlink.
    pub fn claude_project_skill_file(&self) -> PathBuf {
        self.claude_project_skill_dir().join("SKILL.md")
    }
    /// Codex's native repository skill file in the worker cwd.
    pub fn codex_project_skill_file(&self) -> PathBuf {
        self.canonical_worker_skill_file()
    }
    /// The canonical project skill path for either worker engine.
    pub fn native_worker_skill_file(&self, _engine: crate::registry::Engine) -> PathBuf {
        self.canonical_worker_skill_file()
    }
}

/// Reduce a session id to a filesystem-safe, readable path-segment PREFIX (ASCII
/// alphanumeric, `-`, `_`); every other char becomes `-`, and an empty result
/// falls back to `session`. Not injective on its own — [`ProjectPaths::for_session`]
/// appends [`fnv8`] of the raw id to guarantee distinct ids get distinct subtrees.
fn sanitize_session_seg(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let t = s.trim_matches('-').to_string();
    if t.is_empty() { "session".into() } else { t }
}

/// FNV-1a 32-bit hex — disambiguates session segments so ids that sanitize alike
/// still get distinct state subtrees (matches `tmux`'s name hashing).
fn fnv8(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_path_is_a_state_dir_sibling_of_brief() {
        let p = ProjectPaths::for_session("/tmp/proj", "sess-1");
        assert_eq!(p.directive(), p.state_dir().join("directive.md"));
        assert_eq!(p.directive().parent(), p.brief().parent());
    }

    #[test]
    fn advice_last_message_is_a_per_seq_sibling_of_the_advice_log() {
        let p = ProjectPaths::new(std::path::PathBuf::from("/tmp/proj"));
        assert_eq!(
            p.advice_last_message(7),
            p.steps_dir().join("advice-7.last")
        );
        // Distinct from the log and the done-signal so a codex verdict file cannot collide with them.
        assert_ne!(p.advice_last_message(7), p.advice_log(7));
        assert_ne!(p.advice_last_message(7), p.advice_done_signal(7));
    }
}
