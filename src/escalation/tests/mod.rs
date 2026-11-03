//! Unit tests for escalation, split by the production module or transport
//! behavior they exercise. Shared stop and delivery-outcome fixtures live here
//! so each test file stays focused on what reaches the human.

use super::*;
use crate::state::{RiskClass, Stop};

mod fanout;
mod message;
mod osc;
mod program;
mod transport_set;
mod wire;

fn stop(id: &str, kind: &str, rc: RiskClass) -> Stop {
    Stop {
        id: id.into(),
        kind: kind.into(),
        risk_class: rc,
        question: "which path?".into(),
        options: vec![],
        context_ref: None,
        status: "awaiting_reply".into(),
    }
}

/// A program transport aimed at `bin` instead of the program its wire names, so a
/// test can point it at a fake that succeeds, refuses, or does not exist.
fn program(bin: &str) -> ProgramNotifier {
    ProgramNotifier::with_bin(bin, Severity::Warn, Wire::default())
}

fn exited(code: i32, stderr: &str) -> std::io::Result<std::process::Output> {
    use std::os::unix::process::ExitStatusExt;

    Ok(std::process::Output {
        // Wait-status encoding: exit code in the high byte.
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

fn unspawnable() -> std::io::Result<std::process::Output> {
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No such file or directory (os error 2)",
    ))
}

fn record(n: &ProgramNotifier, out: std::io::Result<std::process::Output>) {
    n.health.record(&n.bin, out);
}
