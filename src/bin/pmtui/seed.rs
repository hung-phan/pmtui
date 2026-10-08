//! Bringing a session into existence on disk: where its state lives
//! (`entry_state_paths`, which MUST match the daemon's), how its id is minted, and the
//! initial `config.json`/`control.json`/`brief.md` a create writes. Pure and file-only, so a
//! create is unit-tested without a TTY.

use crate::*;

/// The on-disk state paths pmtui reads/writes for a project. A `Mode::AgentLoop`
/// session keeps its state under `.project-state/sessions/<id>/` (so multiple
/// sessions share a folder, each with its own ledger/config/answers/lease). This MUST match the
/// daemon's `src/daemon/row.rs::entry_paths` — the `JobScheduler` reads/writes an
/// agent-loop session there, so pmtui has to look/act on the same subtree (else it
/// renders the session Fresh, and `a`/`m` write files the loop never reads).
pub(crate) fn entry_state_paths(p: &ProjectEntry) -> ProjectPaths {
    ProjectPaths::for_session(&p.root, &p.id)
}

/// Keep only id-safe chars; fall back to a default if nothing survives.
pub(crate) fn sanitize_id(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "session".into() } else { t }
}

/// Atomically reserve a registry id with no preserved session history (`base`, then `base-2`,
/// `base-3`, …). Closing a row intentionally retains its state directory, so reusing that id would
/// make a new lifecycle inherit the old ledger, marker, conversation, and checkpoint. `create_dir`
/// is the cross-registry claim: two dashboards racing on one root cannot both reserve the same
/// session subtree.
pub(crate) fn reserve_unique_id(reg: &Registry, root: &Path, base: &str) -> Result<String> {
    let sessions_dir = root.join(state::STATE_DIR).join("sessions");
    std::fs::create_dir_all(&sessions_dir)
        .with_context(|| format!("create {}", sessions_dir.display()))?;
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        n += 1;
        if reg.projects.iter().any(|project| project.id == candidate) {
            continue;
        }
        let state_dir = ProjectPaths::for_session(root, &candidate).state_dir();
        match std::fs::create_dir(&state_dir) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", state_dir.display()));
            }
        }
    }
}

/// Write the initial on-disk state for a **agent-loop** session under its
/// PER-SESSION paths (`paths` is a [`ProjectPaths::for_session`]): its
/// `config.json` (tier + decider settings), `control.json` (human cadence and
/// wake requests), and `brief.md` (the goal). pmd creates `state.json` on the
/// first driven tick, preserving the single-writer ledger invariant.
/// Pure and file-only so it can be unit-tested without a TTY.
// One seed writes the whole per-session state, so its inputs (tier + both engine/model pairs +
// brief + cadence + clock) are legitimately several; a params struct would only add indirection.
#[allow(clippy::too_many_arguments)]
pub(crate) fn seed_agent_loop(
    paths: &ProjectPaths,
    tier: Tier,
    _engine: Engine,
    // The engine the decider runs on for this session — seeded from the create form's Decider
    // field (Claude on Standard, where it is inert since the decider only runs on a row pmd drives).
    decider_engine: Engine,
    // The model the decider runs on — seeded from the create form's Decider Model field. `None` =
    // the decider engine's own default (the claude arm keeps its env→const fallback, the codex arm
    // omits `-m`). Autopilot-only at the create form, and inert on Standard for the same reason as
    // `decider_engine` — the decider only runs on a row pmd drives.
    decider_model: Option<String>,
    brief: &str,
    // `None` = UNSET, which is what a Standard session gets: the cadence is how often pmd nudges,
    // so it means nothing until autopilot is on, and the create form does not ask for it there
    // (user: *"if it is standard, we don't need to have cadence too"*). `job_engine` falls back to
    // its own default for an unset one, so nothing is left undefined — and `m`'s prompt asks for a
    // real value at the moment it starts to matter.
    cadence_s: Option<u64>,
    _now: Epoch,
) -> Result<()> {
    std::fs::create_dir_all(paths.state_dir())
        .with_context(|| format!("create {}", paths.state_dir().display()))?;

    // config.json: the chosen tier + the standard harness defaults
    // (JobScheduler::tick requires a readable config).
    let cfg = Config {
        autonomy: tier,
        step_timeout_s: 1800,
        max_failures: 3,
        stuck_threshold: 3,
        coordinator_lease_s: 1860,
        decider_engine,
        decider_model,
    };
    state::write_json_atomic(&paths.config(), &cfg)?;
    state::write_control(
        paths,
        &state::Control {
            human_cadence_s: cadence_s,
            wake_generation: 0,
        },
    )?;

    // brief.md: the goal, read by the harness-owned wake prompt each wake.
    //
    // ATOMIC (tmp+rename) and NOT negotiable: `JobScheduler::nudge` re-reads this file
    // on EVERY heartbeat with `read_to_string(..).unwrap_or_default()`. A plain
    // `fs::write` truncates-then-fills, so a nudge landing inside that window would
    // read an empty/partial brief, and `unwrap_or_default()` would turn that into the
    // "(No goal is recorded on disk …)" fallback — the agent silently loses its mandate
    // for that wake, with nothing in any log to explain it. tmp+rename means a
    // concurrent reader always sees one whole version of the file.
    state::write_text_atomic(&paths.brief(), brief)
        .with_context(|| format!("write {}", paths.brief().display()))?;

    // pmd creates state.json on its first driven tick. pmtui never writes the ledger.
    Ok(())
}
