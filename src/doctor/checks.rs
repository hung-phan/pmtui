use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use serde::de::DeserializeOwned;

use super::{
    AgentLoopState, Answer, Check, Config, Control, DriverState, Engine, ProjectPaths, Registry,
    RuntimeProbe, session_name, supervisor_session_name,
};
use crate::job::{WakeReport, WakeState};
use crate::state::{TurnSignalHealth, turn_signal_health};

const RUNTIME_JSON_MAX_BYTES: u64 = 1024 * 1024;

pub(super) fn registry(path: &Path, checks: &mut Vec<Check>) -> Option<Registry> {
    match fs::read(path) {
        Ok(data) => match serde_json::from_slice(&data) {
            Ok(registry) => {
                checks.push(Check::pass(
                    "registry",
                    format!("loaded {}", path.display()),
                ));
                Some(registry)
            }
            Err(error) => {
                checks.push(
                    Check::fail("registry", "registry JSON is malformed")
                        .with_detail(error.to_string())
                        .with_remediation(
                            "restore valid registry JSON; pmtui must not overwrite a corrupt registry",
                        ),
                );
                None
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            checks.push(
                Check::warn("registry", "registry does not exist")
                    .with_detail(path.display().to_string())
                    .with_remediation("create or import a session from pmtui"),
            );
            None
        }
        Err(error) => {
            checks.push(
                Check::fail("registry", "registry could not be read")
                    .with_detail(error.to_string())
                    .with_remediation("restore read access to the registry path"),
            );
            None
        }
    }
}

pub(super) fn projects(
    registry: &Registry,
    checks: &mut Vec<Check>,
    expected_sessions: &mut BTreeMap<String, usize>,
    expected_supervisors: &mut BTreeMap<String, usize>,
    worker_engines: &mut HashSet<Engine>,
    decider_engines: &mut HashSet<Engine>,
) {
    let mut counts = BTreeMap::new();
    for project in &registry.projects {
        *counts.entry(project.id.as_str()).or_insert(0_usize) += 1;
    }

    for (index, project) in registry.projects.iter().enumerate() {
        let label = project_label(project.id.as_str(), index);
        let mut issues = Vec::new();
        if project.id.trim().is_empty() {
            issues.push("id is blank".to_string());
        }
        if !project.root.is_absolute() {
            issues.push("root is not absolute".to_string());
        }
        if counts.get(project.id.as_str()).copied().unwrap_or(0) > 1 {
            issues.push(format!("id {:?} is duplicated", project.id));
        }
        if issues.is_empty() {
            checks.push(Check::pass(
                format!("registry.entry.{index}"),
                format!("{label} registry entry is valid"),
            ));
        } else {
            checks.push(
                Check::fail(
                    format!("registry.entry.{index}"),
                    format!("{label} registry entry is invalid"),
                )
                .with_detail(issues.join("; "))
                .with_remediation("repair or remove the invalid entry through pmtui"),
            );
        }

        let engine = project.engine.unwrap_or(Engine::Claude);
        if project.enabled {
            worker_engines.insert(engine);
        }
        expected_sessions.insert(session_name(&project.id, &project.root), index);

        if !project.root.is_dir() {
            let root_check = if project.enabled {
                Check::fail(
                    format!("project.{index}.root"),
                    format!("{label} root is missing"),
                )
            } else {
                Check::warn(
                    format!("project.{index}.root"),
                    format!("{label} disabled root is missing"),
                )
            };
            checks.push(
                root_check
                    .with_detail(project.root.display().to_string())
                    .with_remediation(
                        "restore the project directory or remove the stale registry entry in pmtui",
                    ),
            );
            skip_project_files(index, &label, checks);
            continue;
        }
        checks.push(Check::pass(
            format!("project.{index}.root"),
            format!("{label} root exists"),
        ));

        let inspection = inspect_project_files(
            index,
            &label,
            &ProjectPaths::for_session(&project.root, &project.id),
            engine,
            project.enabled,
            checks,
        );
        if let Some(engine) = inspection.decider_engine {
            decider_engines.insert(engine);
        }
        if let Some(supervisor) = inspection.supervisor {
            expected_supervisors.insert(supervisor, index);
        }
    }
    inspect_orphan_state_dirs(registry, checks);
}

fn project_label(id: &str, index: usize) -> String {
    if id.trim().is_empty() {
        format!("entry {index}")
    } else {
        id.to_owned()
    }
}

fn skip_project_files(index: usize, label: &str, checks: &mut Vec<Check>) {
    for (name, noun) in [
        ("config", "config"),
        ("control", "control"),
        ("goal", "goal"),
        ("state", "ledger"),
        ("driver", "driver record"),
        ("marker", "worker marker"),
        ("checkpoint", "worker checkpoint"),
        ("answers", "answer inbox"),
        (WORKER_SKILL.check, WORKER_SKILL.noun),
        (SPAWN_SKILL.check, SPAWN_SKILL.noun),
    ] {
        checks.push(Check::skip(
            format!("project.{index}.{name}"),
            format!("{label} {noun} was not checked because the root is missing"),
        ));
    }
}

#[derive(Debug, Default)]
struct ProjectInspection {
    decider_engine: Option<Engine>,
    supervisor: Option<String>,
}

fn inspect_project_files(
    index: usize,
    label: &str,
    paths: &ProjectPaths,
    expected_engine: Engine,
    enabled: bool,
    checks: &mut Vec<Check>,
) -> ProjectInspection {
    let mut inspection = ProjectInspection::default();
    let config_id = format!("project.{index}.config");
    let config = match read_optional::<Config>(&paths.config()) {
        Ok(Some(config)) => match config.validate() {
            Ok(()) => {
                if enabled {
                    inspection.decider_engine = Some(config.decider_engine);
                }
                checks.push(Check::pass(config_id, format!("{label} config is valid")));
                Some(config)
            }
            Err(error) => {
                checks.push(
                    Check::fail(config_id, format!("{label} config is invalid"))
                        .with_detail(error)
                        .with_remediation("repair config.json in pmtui before enabling Autopilot"),
                );
                None
            }
        },
        Ok(None) => {
            checks.push(
                Check::warn(config_id, format!("{label} config is missing"))
                    .with_remediation("configure the session in pmtui before enabling Autopilot"),
            );
            None
        }
        Err(error) => {
            checks.push(
                Check::fail(config_id, format!("{label} config could not be parsed"))
                    .with_detail(error)
                    .with_remediation("restore valid config.json before enabling Autopilot"),
            );
            None
        }
    };

    inspect_control(index, label, paths, checks);
    inspect_goal(index, label, paths, enabled, config.as_ref(), checks);

    let state_id = format!("project.{index}.state");
    let ledger = match read_optional::<AgentLoopState>(&paths.pmstate()) {
        Ok(Some(state)) if state.engine != expected_engine => {
            checks.push(
                Check::fail(
                    state_id,
                    format!("{label} ledger engine does not match registry"),
                )
                .with_detail(format!(
                    "registry={expected_engine:?}, ledger={:?}",
                    state.engine
                ))
                .with_remediation(
                    "restore the matching registry engine or explicitly create a new session",
                ),
            );
            Some(state)
        }
        Ok(Some(state)) => {
            checks.push(Check::pass(state_id, format!("{label} ledger is valid")));
            Some(state)
        }
        Ok(None) => {
            checks.push(Check::pass(state_id, format!("{label} has no ledger yet")));
            None
        }
        Err(error) => {
            checks.push(
                Check::fail(state_id, format!("{label} ledger could not be parsed"))
                    .with_detail(error)
                    .with_remediation(
                        "stop Autopilot for this row and restore a valid state.json from backup",
                    ),
            );
            None
        }
    };
    if let Some(inflight) = ledger
        .as_ref()
        .and_then(|state| state.advice_inflight.as_ref())
    {
        if enabled
            && config
                .as_ref()
                .is_some_and(|config| config.autonomy == crate::state::Tier::Autopilot)
        {
            inspection.supervisor = Some(supervisor_session_name(label, &paths.root, inflight.seq));
        } else {
            checks.push(
                Check::warn(
                    format!("project.{index}.decider"),
                    format!("{label} retains decider debt while Autopilot is not active"),
                )
                .with_detail(format!("consult #{}", inflight.seq))
                .with_remediation(
                    "keep the row stopped or Standard; pmd will interrupt the leftover consult before driving it again",
                ),
            );
        }
    }

    inspect_marker(index, label, paths, ledger.as_ref(), checks);
    inspect_turn_signal(index, label, paths, ledger.as_ref(), checks);
    inspect_checkpoint(index, label, paths, checks);
    inspect_answers(index, label, paths, checks);

    inspect_driver(index, label, paths, checks);

    inspect_skill(
        index,
        label,
        &WORKER_SKILL,
        &paths.canonical_worker_skill_file(),
        checks,
    );
    inspect_skill(
        index,
        label,
        &SPAWN_SKILL,
        &paths.canonical_spawn_skill_file(),
        checks,
    );
    inspection
}

/// One skill agent-manager ships into a project root, as doctor names and remediates it.
struct ShippedSkill {
    /// The check-id suffix: `project.<index>.<check>`.
    check: &'static str,
    noun: &'static str,
    body: &'static str,
    refresh: &'static str,
    install: &'static str,
}

const WORKER_SKILL: ShippedSkill = ShippedSkill {
    check: "skill",
    noun: "worker skill",
    body: crate::skills::WORKER_SKILL_MD,
    refresh: "leave Autopilot enabled so pmd can refresh the worker skill",
    install: "leave Autopilot enabled so pmd can install the worker skill",
};

/// The spawn skill ships with every launch that names pmtui, so the next start or restart from
/// pmtui (or a pmd launch beside an installed pmtui) brings it current.
const SPAWN_SKILL: ShippedSkill = ShippedSkill {
    check: "spawn_skill",
    noun: "spawn skill",
    body: crate::skills::SPAWN_SKILL_MD,
    refresh: "restart the session from pmtui so its next launch refreshes the spawn skill",
    install: "start or restart the session from pmtui so its launch installs the spawn skill",
};

/// Compare the canonical copy at `path` with the shipped body: current passes, a stale or missing
/// copy warns with how to get a current one, and an unreadable one fails.
fn inspect_skill(
    index: usize,
    label: &str,
    skill: &ShippedSkill,
    path: &Path,
    checks: &mut Vec<Check>,
) {
    let id = format!("project.{index}.{}", skill.check);
    let noun = skill.noun;
    checks.push(match fs::read(path) {
        Ok(data) if data == skill.body.as_bytes() => {
            Check::pass(id, format!("{label} {noun} is current"))
        }
        Ok(_) => {
            Check::warn(id, format!("{label} {noun} is stale")).with_remediation(skill.refresh)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Check::warn(id, format!("{label} {noun} is missing")).with_remediation(skill.install)
        }
        Err(error) => Check::fail(id, format!("{label} {noun} could not be read"))
            .with_detail(error.to_string())
            .with_remediation("restore access to the project-local .agents skill directory"),
    });
}

fn inspect_marker(
    index: usize,
    label: &str,
    paths: &ProjectPaths,
    ledger: Option<&AgentLoopState>,
    checks: &mut Vec<Check>,
) {
    let id = format!("project.{index}.marker");
    match read_runtime_json::<WakeReport>(&paths.needs_you()) {
        Ok(Some(report)) => {
            let revision = crate::job::marker_revision(&paths.needs_you(), &report).ok();
            let accepted = ledger.is_some_and(|state| {
                state.last_marker_revision.as_ref() == revision.as_ref()
                    || (state.last_marker_revision.is_none()
                        && (state
                            .situation
                            .as_ref()
                            .is_some_and(|situation| situation.seq == report.seq)
                            || (state.last_marker_seq != 0
                                && state.last_marker_seq == report.seq)))
            });
            let disposition = match report.state {
                WakeState::Working => "working",
                WakeState::Monitoring => "monitoring",
                WakeState::Blocked => "blocked",
            };
            if ledger.is_some() && !accepted {
                checks.push(
                    Check::warn(
                        id,
                        format!(
                            "{label} worker marker is pending pmd acceptance ({disposition})"
                        ),
                    )
                        .with_detail(format!(
                            "worker audit seq={}, last accepted audit seq={}, report generation={}",
                            report.seq,
                            ledger.map_or(0, |state| state.last_marker_seq),
                            ledger.map_or(0, |state| state.report_generation)
                        ))
                        .with_remediation(
                            "if the session is blocked, answer or close its stop; otherwise verify pmd is running and can read the marker",
                        ),
                );
            } else {
                checks.push(Check::pass(
                    id,
                    format!(
                        "{label} worker marker is valid ({disposition}, audit seq {})",
                        report.seq
                    ),
                ));
            }
        }
        Ok(None) => checks.push(Check::pass(id, format!("{label} has no worker marker yet"))),
        Err(error) => checks.push(
            Check::warn(id, format!("{label} worker marker could not be parsed"))
                .with_detail(error)
                .with_remediation(
                    "inspect needs-you.json and let the worker replace it atomically with a valid report",
                ),
        ),
    }
}

/// IS THE TURN-END HOOK ACTUALLY FIRING? One byte is appended to `turn-complete` per completed turn,
/// and for claude that file is the ONLY proof a turn ended — the drive path will not end an
/// awaiting-report hold without it, so a dead hook silently costs a session every recovery nudge it
/// should have had and leaves it holding until the report-debt ceiling.
///
/// Comparing the signal against `report_generation` is what makes a dead hook visible: the worker
/// cannot have reported more times than it has finished turns. Many more reports than signal bytes
/// means the hook is not being invoked, which happened unseen on a live codex session — `turn-complete`
/// advanced once across roughly seven accepted reports while `-c notify=[…]` sat correctly in its argv.
///
/// A WARNING, never a failure: the hook is optional by design, a fresh session legitimately has neither
/// file nor reports, and a few reports ahead of the signal is just the ordinary race between a turn
/// ending and its notification landing.
fn inspect_turn_signal(
    index: usize,
    label: &str,
    paths: &ProjectPaths,
    ledger: Option<&AgentLoopState>,
    checks: &mut Vec<Check>,
) {
    let id = format!("project.{index}.turn_signal");
    let reports = ledger.map(|l| l.report_generation).unwrap_or(0);
    // The SAME classifier the report-debt backstop acts on, so this check can never describe a
    // healthier hook than the one the scheduler is holding a session open for.
    match turn_signal_health(&paths.turn_signal(), reports) {
        // Never wired, or no turn has finished yet. Nothing to compare against, and nothing is wrong
        // with a session that has not reported either.
        TurnSignalHealth::Fresh => checks.push(Check::pass(
            id,
            format!("{label} has no turn-end signal yet"),
        )),
        TurnSignalHealth::NeverFired { reports } => checks.push(
            Check::warn(
                id,
                format!("{label} turn-end hook has never fired despite {reports} reports"),
            )
            .with_remediation(
                "restart the session so it relaunches with the turn-end hook; without it an \
                 awaiting-report hold ends only at the report-debt ceiling",
            ),
        ),
        TurnSignalHealth::Partial { turns, reports } => checks.push(
            Check::warn(
                id,
                format!("{label} turn-end hook is firing for only some turns"),
            )
            .with_detail(format!(
                "{turns} turn-end signals against {reports} accepted reports"
            ))
            .with_remediation(
                "the worker is reporting more often than its turn-end hook fires, so recovery \
                 nudges are being missed; restart the session, and check the engine still honours \
                 the hook this build passes it",
            ),
        ),
        TurnSignalHealth::Healthy { turns } => checks.push(Check::pass(
            id,
            format!("{label} turn-end hook has fired {turns} times"),
        )),
    }
}

fn inspect_checkpoint(index: usize, label: &str, paths: &ProjectPaths, checks: &mut Vec<Check>) {
    let id = format!("project.{index}.checkpoint");
    match crate::state::read_checkpoint(&paths.checkpoint()) {
        Ok(Some(checkpoint)) => checks.push(Check::pass(
            id,
            format!("{label} worker checkpoint is valid (seq {})", checkpoint.seq),
        )),
        Ok(None) => checks.push(Check::pass(
            id,
            format!("{label} has no worker checkpoint yet"),
        )),
        Err(error) => checks.push(
            Check::warn(id, format!("{label} worker checkpoint is invalid"))
                .with_detail(format!("{error:#}"))
                .with_remediation(
                    "inspect checkpoint.json and let the worker replace it; checkpoint data is continuity only",
                ),
        ),
    }
}

fn inspect_answers(index: usize, label: &str, paths: &ProjectPaths, checks: &mut Vec<Check>) {
    let id = format!("project.{index}.answers");
    match read_runtime_json::<Vec<Answer>>(&paths.answers()) {
        Ok(Some(answers)) if answers.len() > crate::state::ANSWERS_MAX => checks.push(
            Check::warn(id, format!("{label} answer inbox exceeds its retention bound"))
                .with_detail(format!(
                    "{} answers; expected at most {}",
                    answers.len(),
                    crate::state::ANSWERS_MAX
                ))
                .with_remediation(
                    "answer the next decision through pmtui so the normal bounded rewrite can prune old entries",
                ),
        ),
        Ok(Some(answers)) => checks.push(Check::pass(
            id,
            format!("{label} answer inbox is valid ({} entries)", answers.len()),
        )),
        Ok(None) => checks.push(Check::pass(id, format!("{label} has no answer inbox yet"))),
        Err(error) => checks.push(
            Check::fail(id, format!("{label} answer inbox could not be parsed"))
                .with_detail(error)
                .with_remediation(
                    "restore valid answers.json before answering another dashboard decision",
                ),
        ),
    }
}

fn inspect_driver(index: usize, label: &str, paths: &ProjectPaths, checks: &mut Vec<Check>) {
    let id = format!("project.{index}.driver");
    match read_optional::<DriverState>(&paths.driver()) {
        Ok(Some(_)) => checks.push(Check::pass(id, format!("{label} driver record is valid"))),
        Ok(None) => checks.push(Check::pass(id, format!("{label} has no driver record yet"))),
        Err(error) => checks.push(
            Check::fail(id, format!("{label} driver record could not be parsed"))
                .with_detail(error)
                .with_remediation(
                    "preserve driver.json for diagnosis and restart the session from pmtui",
                ),
        ),
    }
}

fn inspect_control(index: usize, label: &str, paths: &ProjectPaths, checks: &mut Vec<Check>) {
    let id = format!("project.{index}.control");
    match read_optional::<Control>(&paths.control()) {
        Ok(Some(control)) => match control.human_cadence_s {
            Some(cadence)
                if !(crate::job_engine::CADENCE_MIN_S..=crate::job_engine::CADENCE_MAX_S)
                    .contains(&cadence) =>
            {
                checks.push(
                    Check::fail(
                        id,
                        format!("{label} control cadence is outside supported bounds"),
                    )
                    .with_detail(format!(
                        "{cadence}s; expected {}..={}s",
                        crate::job_engine::CADENCE_MIN_S,
                        crate::job_engine::CADENCE_MAX_S
                    ))
                    .with_remediation("reset cadence through pmtui with the c key"),
                );
            }
            _ => checks.push(Check::pass(id, format!("{label} control is valid"))),
        },
        Ok(None) => checks.push(Check::pass(id, format!("{label} has no control requests"))),
        Err(error) => checks.push(
            Check::fail(id, format!("{label} control could not be parsed"))
                .with_detail(error)
                .with_remediation("restore valid control.json before changing Autopilot settings"),
        ),
    }
}

fn inspect_goal(
    index: usize,
    label: &str,
    paths: &ProjectPaths,
    enabled: bool,
    config: Option<&Config>,
    checks: &mut Vec<Check>,
) {
    let id = format!("project.{index}.goal");
    if !enabled || !config.is_some_and(|config| config.autonomy == crate::state::Tier::Autopilot) {
        checks.push(Check::pass(
            id,
            format!("{label} does not require an Autopilot goal"),
        ));
        return;
    }
    match fs::read_to_string(paths.brief()) {
        Ok(goal) if !goal.trim().is_empty() => {
            checks.push(Check::pass(
                id,
                format!("{label} Autopilot goal is present"),
            ));
        }
        Ok(_) => checks.push(
            Check::fail(id, format!("{label} Autopilot goal is blank"))
                .with_remediation("set a non-empty goal in pmtui with the g key"),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => checks.push(
            Check::fail(id, format!("{label} Autopilot goal is missing"))
                .with_remediation("set a non-empty goal in pmtui with the g key"),
        ),
        Err(error) => checks.push(
            Check::fail(id, format!("{label} Autopilot goal could not be read"))
                .with_detail(error.to_string())
                .with_remediation("restore read access, then edit the goal in pmtui"),
        ),
    }
}

fn read_runtime_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let file = match open_runtime_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not open as a regular file: {error}")),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_file() {
        return Err("path is not a regular file".into());
    }
    if metadata.len() > RUNTIME_JSON_MAX_BYTES {
        return Err(format!(
            "file is {} bytes; maximum diagnostic size is {RUNTIME_JSON_MAX_BYTES}",
            metadata.len()
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len().min(RUNTIME_JSON_MAX_BYTES)).unwrap_or(0),
    );
    if let Err(error) = file
        .take(RUNTIME_JSON_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        return Err(error.to_string());
    }
    if bytes.len() as u64 > RUNTIME_JSON_MAX_BYTES {
        return Err(format!(
            "file grew beyond the maximum diagnostic size of {RUNTIME_JSON_MAX_BYTES} bytes"
        ));
    }
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(unix)]
fn open_runtime_file(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))?;
    Ok(fd.into())
}

#[cfg(not(unix))]
fn open_runtime_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn inspect_orphan_state_dirs(registry: &Registry, checks: &mut Vec<Check>) {
    let mut roots: BTreeMap<&Path, BTreeSet<_>> = BTreeMap::new();
    for project in registry
        .projects
        .iter()
        .filter(|project| project.root.is_absolute() && project.root.is_dir())
    {
        roots
            .entry(project.root.as_path())
            .or_default()
            .insert(ProjectPaths::for_session(&project.root, &project.id).state_dir());
    }
    for (root_index, (root, expected)) in roots.into_iter().enumerate() {
        let id = format!("project-state.orphans.{root_index}");
        let sessions_dir = root.join(crate::state::STATE_DIR).join("sessions");
        let entries = match fs::read_dir(&sessions_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                checks.push(Check::pass(id, "no orphan session-state directories"));
                continue;
            }
            Err(error) => {
                checks.push(
                    Check::fail(id, "session-state inventory could not be read")
                        .with_detail(error.to_string())
                        .with_remediation("restore read access to the .project-state directory"),
                );
                continue;
            }
        };
        let mut orphaned = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) if !expected.contains(&entry.path()) => {
                    orphaned.push(entry.file_name().to_string_lossy().into_owned());
                }
                Ok(_) => {}
                Err(error) => orphaned.push(format!("<unreadable entry: {error}>")),
            }
        }
        orphaned.sort();
        checks.push(if orphaned.is_empty() {
            Check::pass(id, "no orphan session-state directories")
        } else {
            Check::warn(
                id,
                format!("{} orphan session-state directorie(s)", orphaned.len()),
            )
            .with_detail(orphaned.join(", "))
            .with_remediation(
                "confirm the sessions are no longer needed, then archive or remove those directories manually",
            )
        });
    }
}

fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    crate::state::read_json_opt(path).map_err(|error| format!("{error:#}"))
}

pub(super) fn binaries(
    probe: &dyn RuntimeProbe,
    worker_engines: &HashSet<Engine>,
    decider_engines: &HashSet<Engine>,
    checks: &mut Vec<Check>,
) {
    for engine in Engine::ALL
        .into_iter()
        .filter(|engine| worker_engines.contains(engine))
    {
        let binary = engine.bin();
        checks.push(if probe.binary_on_path(binary) {
            Check::pass(
                format!("worker.{binary}"),
                format!("{binary} worker executable is on PATH"),
            )
        } else {
            Check::fail(
                format!("worker.{binary}"),
                format!("{binary} worker executable is not on PATH"),
            )
            .with_remediation(format!(
                "install and authenticate {binary}, then rerun pmd doctor"
            ))
        });
    }
    for engine in Engine::ALL
        .into_iter()
        .filter(|engine| decider_engines.contains(engine))
    {
        let binary = engine.bin();
        checks.push(if probe.binary_on_path(binary) {
            Check::pass(
                format!("decider.{binary}"),
                format!("{binary} decider executable is on PATH"),
            )
        } else {
            Check::warn(
                format!("decider.{binary}"),
                format!("{binary} decider executable is not on PATH"),
            )
            .with_remediation(format!(
                "install {binary} or choose an available decider engine in pmtui"
            ))
        });
    }
}

/// The oldest tmux whose `new-session -e KEY=VALUE` gives a managed launch its session
/// environment (`PMTUI_SESSION`, `PMTUI_STATE_DIR`, `PMTUI_BIN`).
const MANAGED_ENV_TMUX: (u32, u32) = (3, 0);

/// Whether the installed tmux (its `tmux -V` line) can start managed sessions with their
/// environment: without it, a worker cannot request a child session with `pmtui spawn`.
fn managed_env(version: &str) -> Check {
    let (major, minor) = MANAGED_ENV_TMUX;
    let remediation = format!("install tmux {major}.{minor} or newer");
    match tmux_release(version) {
        Some(release) if release >= MANAGED_ENV_TMUX => Check::pass(
            "tmux.managed_env",
            "tmux passes the session environment to managed launches (new-session -e)",
        ),
        Some((found_major, found_minor)) => Check::fail(
            "tmux.managed_env",
            format!(
                "tmux {found_major}.{found_minor} is older than {major}.{minor}, so managed \
                 launches cannot pass their session environment (new-session -e)"
            ),
        )
        .with_remediation(remediation),
        None => Check::warn(
            "tmux.managed_env",
            "the tmux version could not be read, so managed-launch support is unverified",
        )
        .with_detail(version)
        .with_remediation(remediation),
    }
}

/// `(major, minor)` from a `tmux -V` line such as `tmux 3.5a`, `tmux next-3.6` or `tmux 2.9`.
fn tmux_release(version: &str) -> Option<(u32, u32)> {
    let start = version.find(|c: char| c.is_ascii_digit())?;
    let (major, rest) = leading_number(&version[start..])?;
    let (minor, _) = leading_number(rest.strip_prefix('.')?)?;
    Some((major, minor))
}

/// The decimal number `text` starts with, and what follows it.
fn leading_number(text: &str) -> Option<(u32, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    Some((text[..end].parse().ok()?, &text[end..]))
}

pub(super) fn tmux(
    probe: &dyn RuntimeProbe,
    socket: &str,
    registry: Option<&Registry>,
    expected_sessions: &BTreeMap<String, usize>,
    expected_supervisors: &BTreeMap<String, usize>,
    checks: &mut Vec<Check>,
) {
    if socket.trim().is_empty() {
        checks.push(
            Check::fail("tmux.socket", "tmux socket name is empty")
                .with_remediation("pass the same non-empty --socket name used by pmtui and pmd"),
        );
        checks.push(Check::skip(
            "tmux.sessions",
            "tmux sessions were not checked",
        ));
        return;
    }
    match probe.tmux_version() {
        Ok(version) => {
            let managed = managed_env(&version);
            checks.push(Check::pass("tmux.version", version));
            checks.push(managed);
        }
        Err(error) => {
            checks.push(
                Check::fail("tmux.version", "tmux is unavailable")
                    .with_detail(error)
                    .with_remediation("install tmux and ensure it is on PATH"),
            );
            checks.push(Check::skip(
                "tmux.sessions",
                "tmux sessions were not checked",
            ));
            return;
        }
    }

    let sessions = match probe.tmux_sessions(socket) {
        Ok(sessions) => sessions,
        Err(error) => {
            checks.push(
                Check::fail("tmux.sessions", "tmux sessions could not be listed")
                    .with_detail(error)
                    .with_remediation(
                        "verify the socket name and tmux socket permissions, then rerun doctor",
                    ),
            );
            return;
        }
    };
    checks.push(Check::pass(
        "tmux.sessions",
        format!("{} session(s) on socket {socket}", sessions.len()),
    ));
    let sessions: BTreeSet<_> = sessions.into_iter().collect();

    if let Some(registry) = registry {
        for (index, project) in registry.projects.iter().enumerate() {
            if !project.enabled {
                continue;
            }
            let expected = session_name(&project.id, &project.root);
            checks.push(if sessions.contains(&expected) {
                Check::pass(
                    format!("tmux.session.{index}"),
                    format!("{} terminal is present", project_label(&project.id, index)),
                )
            } else {
                Check::warn(
                    format!("tmux.session.{index}"),
                    format!(
                        "{} terminal is not running",
                        project_label(&project.id, index)
                    ),
                )
                .with_detail(expected)
                .with_remediation("restart the session from pmtui if it should be running")
            });
        }
    }

    let unregistered: Vec<_> = sessions
        .iter()
        .filter(|session| session.starts_with("pm-") && !expected_sessions.contains_key(*session))
        .cloned()
        .collect();
    checks.push(if unregistered.is_empty() {
        Check::pass("tmux.unregistered", "no unregistered project terminals")
    } else {
        Check::warn(
            "tmux.unregistered",
            format!("{} unregistered project terminal(s)", unregistered.len()),
        )
        .with_detail(unregistered.join(", "))
        .with_remediation(
            "attribute these terminals to the correct registry before restarting pmd on this socket",
        )
    });

    for (session, index) in expected_supervisors {
        let check = if !sessions.contains(session) {
            Check::warn(
                format!("tmux.supervisor.{index}"),
                "ledger records an in-flight decider but its terminal is missing",
            )
            .with_detail(session.clone())
            .with_remediation(
                "keep pmd running so it can recover or escalate the interrupted decision",
            )
        } else {
            match probe.tmux_pane_dead(socket, session) {
                Ok(false) => Check::pass(
                    format!("tmux.supervisor.{index}"),
                    format!("decider terminal {session} is active"),
                ),
                Ok(true) => Check::warn(
                    format!("tmux.supervisor.{index}"),
                    "decider terminal exists but its pane has exited",
                )
                .with_detail(session.clone())
                .with_remediation(
                    "keep pmd running so it can reap the dead pane and escalate the interrupted decision",
                ),
                Err(error) => Check::warn(
                    format!("tmux.supervisor.{index}"),
                    "decider terminal liveness could not be verified",
                )
                .with_detail(error)
                .with_remediation("verify the tmux socket, then rerun pmd doctor"),
            }
        };
        checks.push(check);
    }

    let supervisors: Vec<_> = sessions
        .iter()
        .filter(|session| {
            session.starts_with("pmsup-") && !expected_supervisors.contains_key(*session)
        })
        .cloned()
        .collect();
    checks.push(if supervisors.is_empty() {
        Check::pass("tmux.supervisors.stale", "no stale decider terminals")
    } else {
        Check::warn(
            "tmux.supervisors.stale",
            format!("{} stale decider terminal(s)", supervisors.len()),
        )
        .with_detail(supervisors.join(", "))
        .with_remediation(
            "restart pmd on this registry and socket to reap stale decider terminals without touching project terminals",
        )
    });
}

pub(super) fn notifications(probe: &dyn RuntimeProbe, checks: &mut Vec<Check>) {
    checks.push(if probe.binary_on_path("notify-send") {
        Check::pass(
            "notification.notify-send",
            "notify-send is available (no notification was sent)",
        )
    } else {
        Check::warn(
            "notification.notify-send",
            "notify-send is not on PATH; ledger escalations still work",
        )
        .with_remediation(
            "install notify-send only if desktop escalation notifications are desired",
        )
    });
}
