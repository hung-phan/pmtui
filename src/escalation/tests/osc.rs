//! The terminal transport: the OSC 777 bytes it writes, and the escaping that keeps
//! agent-authored text from breaking out of the sequence.

use super::*;
use crate::escalation::transport::osc777;

fn escalation(title: &str, body: &str, severity: Severity) -> Escalation {
    Escalation {
        project: "agent-manager".into(),
        title: title.into(),
        body: body.into(),
        severity,
    }
}

#[test]
fn the_sequence_is_osc_777_with_two_folded_fields() {
    assert_eq!(
        osc777(&escalation(
            "1 decision needs you",
            "Ship now, or hold?",
            Severity::Warn
        )),
        "\x1b]777;notify;pmd · agent-manager — 1 decision needs you;Ship now, or hold?\x1b\\"
    );
}

/// A `0x07` in the text would TERMINATE the sequence early on any terminal that takes BEL
/// as an OSC terminator, leaving the rest to be printed as literal junk. The sequence ends
/// with ST, so a stray BEL must not survive in the body at all.
#[test]
fn a_bel_in_the_body_cannot_end_the_sequence_early() {
    let seq = osc777(&escalation("t", "before\x07after", Severity::Warn));
    assert_eq!(seq.matches('\x07').count(), 0);
    assert!(seq.ends_with("\x1b\\"));
    assert!(seq.contains("before after"));
}

/// An `0x1b` would start a second escape sequence — the injection that matters,
/// because an escalation body is built from stop questions the WORKER wrote. Two escapes
/// belong to the sequence itself (the OSC opener and the ST terminator); a third would be
/// the worker's.
#[test]
fn an_escape_byte_in_agent_authored_text_cannot_start_another_sequence() {
    let seq = osc777(&escalation(
        "t",
        "x\x1b]777;notify;spoofed;body\x07y",
        Severity::Warn,
    ));
    assert_eq!(seq.matches('\x1b').count(), 2);
    assert!(seq.starts_with("\x1b]777;notify;"));
    assert!(seq.ends_with("\x1b\\"));
    assert!(!seq.contains("spoofed;body"));
}

/// A `;` would shift every later field, so a title carrying one would push the body
/// out of its slot and into nothing.
#[test]
fn a_semicolon_cannot_shift_the_fields() {
    let seq = osc777(&escalation("a;b", "c;d", Severity::Warn));
    // Two structural separators (after `777` and after `notify`) plus the one
    // between the folded title and the body.
    assert_eq!(seq.matches(';').count(), 3);
    assert!(seq.contains("a,b"));
    assert!(seq.contains("c,d"));
}

#[test]
fn newlines_in_the_body_collapse_rather_than_breaking_the_line() {
    let seq = osc777(&escalation("t", "one\ntwo\n\nthree", Severity::Warn));
    assert!(!seq.contains('\n'));
    assert!(seq.contains("one two three"));
}

/// OSC 777 has no urgency field, so severity cannot reach the wire; it survives only
/// as the gate below.
#[test]
fn the_sequence_does_not_vary_with_severity() {
    let urgent = osc777(&escalation("t", "b", Severity::Urgent));
    let info = osc777(&escalation("t", "b", Severity::Info));
    assert_eq!(urgent, info);
}

#[test]
fn the_terminal_transport_is_severity_gated_like_the_others() {
    let n = OscNotifier::default();
    assert!(!n.will_send(&escalation("t", "b", Severity::Info)));
    assert!(n.will_send(&escalation("t", "b", Severity::Warn)));
    assert!(n.will_send(&escalation("t", "b", Severity::Urgent)));
    assert!(OscNotifier::new(Severity::Info).will_send(&escalation("t", "b", Severity::Info)));
}

/// Under the test harness stderr is captured, so it is not a terminal and nothing is
/// written — which is exactly the production behavior for a pmd with redirected
/// output. The call still succeeds, because a notification that found no terminal
/// must never fail the caller's sweep.
#[test]
fn writing_without_a_terminal_is_a_successful_no_op() {
    assert!(
        OscNotifier::default()
            .notify(&escalation("t", "b", Severity::Urgent))
            .is_ok()
    );
}
