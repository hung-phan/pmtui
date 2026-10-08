use std::fmt::Write;

use super::{CheckSummary, DoctorReport};

pub(super) fn text(report: &DoctorReport) -> String {
    let mut output = format!(
        "pmd doctor: {}\nregistry: {}\nsocket: {}\n\n",
        report.status,
        report.registry.display(),
        report.socket
    );
    for check in &report.checks {
        let _ = writeln!(output, "[{}] {}: {}", check.status, check.id, check.summary);
        if let Some(detail) = &check.detail {
            let _ = writeln!(output, "       {detail}");
        }
        if let Some(remediation) = &check.remediation {
            let _ = writeln!(output, "       recovery: {remediation}");
        }
    }
    let _ = write!(output, "\n{}", summary_text(report.summary));
    output
}

fn summary_text(summary: CheckSummary) -> String {
    format!(
        "{} {}, {} {}, {} {}, {} {}",
        summary.pass,
        noun(summary.pass, "passed", "passed"),
        summary.warn,
        noun(summary.warn, "warning", "warnings"),
        summary.fail,
        noun(summary.fail, "failed", "failed"),
        summary.skip,
        noun(summary.skip, "skipped", "skipped")
    )
}

fn noun(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}
