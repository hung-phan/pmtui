mod checks;
mod render;

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use serde::Serialize;

use crate::job::AgentLoopState;
use crate::registry::{Engine, Registry};
use crate::state::{Answer, Config, Control, DriverState, ProjectPaths};
use crate::tmux::{session_name, supervisor_session_name};

pub const DOCTOR_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Check {
    fn new(id: impl Into<String>, status: CheckStatus, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status,
            summary: summary.into(),
            detail: None,
            remediation: None,
        }
    }

    pub fn pass(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Pass, summary)
    }

    pub fn warn(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Warn, summary)
    }

    pub fn fail(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Fail, summary)
    }

    pub fn skip(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Skip, summary)
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverallStatus {
    Pass,
    Warn,
    Fail,
}

impl fmt::Display for OverallStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CheckSummary {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub skip: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub pmd_version: &'static str,
    pub status: OverallStatus,
    pub registry: PathBuf,
    pub socket: String,
    pub checks: Vec<Check>,
    pub summary: CheckSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorOptions {
    pub registry: PathBuf,
    pub socket: String,
}

impl DoctorReport {
    pub fn new(registry: PathBuf, socket: String, checks: Vec<Check>) -> Self {
        let mut summary = CheckSummary::default();
        for check in &checks {
            match check.status {
                CheckStatus::Pass => summary.pass += 1,
                CheckStatus::Warn => summary.warn += 1,
                CheckStatus::Fail => summary.fail += 1,
                CheckStatus::Skip => summary.skip += 1,
            }
        }
        let status = if summary.fail > 0 {
            OverallStatus::Fail
        } else if summary.warn > 0 {
            OverallStatus::Warn
        } else {
            OverallStatus::Pass
        };
        Self {
            schema_version: DOCTOR_SCHEMA_VERSION,
            pmd_version: env!("CARGO_PKG_VERSION"),
            status,
            registry,
            socket,
            checks,
            summary,
        }
    }

    pub fn exit_code(&self) -> u8 {
        if self.status == OverallStatus::Fail {
            1
        } else {
            0
        }
    }

    pub fn render_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn render_text(&self) -> String {
        render::text(self)
    }
}

pub fn run(options: &DoctorOptions) -> DoctorReport {
    inspect_with(options, &NativeProbe::default())
}

pub fn command(options: &DoctorOptions, json: bool) -> anyhow::Result<u8> {
    let report = run(options);
    let mut stdout = io::stdout().lock();
    write_report(&report, json, &mut stdout)
}

fn write_report(report: &DoctorReport, json: bool, writer: &mut impl Write) -> anyhow::Result<u8> {
    let output = if json {
        report.render_json()?
    } else {
        report.render_text()
    };
    writeln!(writer, "{output}").context("write doctor report")?;
    Ok(report.exit_code())
}

pub(crate) trait RuntimeProbe {
    fn binary_on_path(&self, binary: &str) -> bool;
    fn tmux_version(&self) -> Result<String, String>;
    fn tmux_sessions(&self, socket: &str) -> Result<Vec<String>, String>;
    fn tmux_pane_dead(&self, socket: &str, session: &str) -> Result<bool, String>;
}

#[derive(Debug, Clone)]
pub(crate) struct NativeProbe {
    tmux: PathBuf,
    path: Option<std::ffi::OsString>,
}

impl Default for NativeProbe {
    fn default() -> Self {
        Self {
            tmux: PathBuf::from("tmux"),
            path: std::env::var_os("PATH"),
        }
    }
}

impl NativeProbe {
    #[cfg(test)]
    pub(crate) fn with_tmux(tmux: PathBuf) -> Self {
        Self {
            tmux,
            path: std::env::var_os("PATH"),
        }
    }
}

impl RuntimeProbe for NativeProbe {
    fn binary_on_path(&self, binary: &str) -> bool {
        binary_in_path(binary, self.path.as_deref())
    }

    fn tmux_version(&self) -> Result<String, String> {
        let output = Command::new(&self.tmux)
            .arg("-V")
            .output()
            .map_err(|error| format!("could not run tmux: {error}"))?;
        if !output.status.success() {
            return Err(command_error("tmux -V", &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn tmux_sessions(&self, socket: &str) -> Result<Vec<String>, String> {
        let output = Command::new(&self.tmux)
            .args(["-L", socket, "list-sessions", "-F", "#{session_name}"])
            .output()
            .map_err(|error| format!("could not list tmux sessions: {error}"))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("no sessions") {
            return Ok(Vec::new());
        }
        Err(command_error("tmux list-sessions", &output))
    }

    fn tmux_pane_dead(&self, socket: &str, session: &str) -> Result<bool, String> {
        let target = format!("={session}:");
        let output = Command::new(&self.tmux)
            .args([
                "-L",
                socket,
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_dead}",
            ])
            .output()
            .map_err(|error| format!("could not inspect tmux pane: {error}"))?;
        if !output.status.success() {
            return Err(command_error("tmux display-message", &output));
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "0" => Ok(false),
            "1" => Ok(true),
            value => Err(format!(
                "tmux returned an invalid pane_dead value: {value:?}"
            )),
        }
    }
}

fn command_error(command: &str, output: &std::process::Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        format!("{command} exited with {}", output.status)
    } else {
        format!("{command} failed: {detail}")
    }
}

pub(crate) fn binary_in_path(binary: &str, path: Option<&OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|dir| executable_file(&dir.join(binary)))
}

#[cfg(unix)]
fn executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

pub(crate) fn inspect_with(options: &DoctorOptions, probe: &dyn RuntimeProbe) -> DoctorReport {
    let mut checks = vec![Check::pass(
        "version",
        format!("pmd {}", env!("CARGO_PKG_VERSION")),
    )];
    let registry = checks::registry(&options.registry, &mut checks);
    let mut expected_sessions = BTreeMap::new();
    let mut expected_supervisors = BTreeMap::new();
    let mut decider_engines = HashSet::new();
    let mut worker_engines = HashSet::new();

    if let Some(registry) = &registry {
        checks::projects(
            registry,
            &mut checks,
            &mut expected_sessions,
            &mut expected_supervisors,
            &mut worker_engines,
            &mut decider_engines,
        );
    }
    checks::binaries(probe, &worker_engines, &decider_engines, &mut checks);
    checks::tmux(
        probe,
        &options.socket,
        registry.as_ref(),
        &expected_sessions,
        &expected_supervisors,
        &mut checks,
    );
    checks::notifications(probe, &mut checks);

    DoctorReport::new(options.registry.clone(), options.socket.clone(), checks)
}

#[cfg(test)]
mod tests;
