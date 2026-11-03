//! Asking for transports by name: the parse, the default set, and why that default
//! is not every transport.

use super::*;
use crate::escalation::transport::{DEFAULT_TRANSPORTS, Transport, Wire};

#[test]
fn every_transport_round_trips_through_its_name() {
    for t in Transport::ALL {
        assert_eq!(Transport::parse(t.name()), Some(*t), "{t:?}");
    }
}

#[test]
fn the_names_are_the_ones_the_help_text_offers() {
    assert_eq!(
        Transport::ALL.iter().map(|t| t.name()).collect::<Vec<_>>(),
        ["log", "notify-send", "cmux", "osc"]
    );
}

/// Every name is distinct, or one of them would be unreachable.
#[test]
fn no_two_transports_share_a_name() {
    let mut names: Vec<_> = Transport::ALL.iter().map(|t| t.name()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before);
}

/// Each `Wire` arm must be reachable by name, so adding a wire without naming it is
/// caught here rather than by a user whose `--notify cmux` does nothing.
#[test]
fn every_wire_is_reachable_as_a_transport() {
    for wire in [Wire::NotifySend, Wire::Cmux] {
        assert!(
            Transport::ALL.contains(&Transport::Program(wire)),
            "{wire:?} has no name"
        );
    }
}

#[test]
fn a_list_parses_in_the_order_given() {
    assert_eq!(
        Transport::parse_list("log,cmux,osc").unwrap(),
        [
            Transport::Log,
            Transport::Program(Wire::Cmux),
            Transport::Osc
        ]
    );
}

#[test]
fn surrounding_space_and_empty_entries_are_tolerated() {
    assert_eq!(
        Transport::parse_list(" log , osc ,").unwrap(),
        [Transport::Log, Transport::Osc]
    );
    assert_eq!(Transport::parse_list("").unwrap(), []);
}

/// An unknown name is an ERROR, not a skip: a caller who asked for a transport and
/// silently did not get it would read every missing notification as that transport
/// failing, which is the opposite of the truth.
#[test]
fn an_unknown_name_names_itself_rather_than_being_dropped() {
    assert_eq!(Transport::parse_list("log,bogus,osc"), Err("bogus".into()));
    assert_eq!(Transport::parse("Cmux"), None, "names are case-sensitive");
}

/// The default is the set that shipped before the others existed. The two that reach
/// a specific product are opt-in BECAUSE THE INTEGRATION SUITE RUNS THE REAL `pmd`
/// BINARY: on by default, a test that drove a session to an escalation fired a real
/// notification into the developer's own terminal.
#[test]
fn the_default_set_excludes_every_transport_with_effects_outside_this_process() {
    assert_eq!(
        DEFAULT_TRANSPORTS,
        [Transport::Log, Transport::Program(Wire::NotifySend)]
    );
    assert!(!DEFAULT_TRANSPORTS.contains(&Transport::Program(Wire::Cmux)));
    assert!(!DEFAULT_TRANSPORTS.contains(&Transport::Osc));
}

/// `log` is in the default set because it is the reason a failed toast is never a
/// lost escalation — every other transport's give-up rule leans on it.
#[test]
fn the_default_set_always_records() {
    assert!(DEFAULT_TRANSPORTS.contains(&Transport::Log));
}

#[test]
fn each_transport_builds_its_own_notifier() {
    let e = Escalation {
        project: "proj".into(),
        title: "1 decision needs you".into(),
        body: "which?".into(),
        severity: Severity::Urgent,
    };
    // Only that it builds and delivers without panicking: `log` writes a line, and
    // the rest find no server, no program and no terminal under the harness.
    for t in Transport::ALL {
        assert!(t.notifier(Severity::Warn).notify(&e).is_ok(), "{t:?}");
    }
}

/// The warning is user-visible text, so it is pinned rather than trusted. It must
/// name the program that failed, say why, and say that the escalation survived.
#[test]
fn the_broken_transport_warning_names_the_program_and_keeps_the_escalation() {
    use crate::escalation::transport::broken_transport_warning;

    let warning = broken_transport_warning("cmux", "exit status: 1 (i/o timeout)");
    assert!(warning.contains("`cmux`"), "{warning}");
    assert!(warning.contains("i/o timeout"), "{warning}");
    assert!(
        warning.contains("still recorded in the session ledger"),
        "a reader must learn the escalation was not lost: {warning}"
    );
    // It must NOT claim a family it does not know. The same bookkeeping serves
    // `notify-send`, `cmux` and anything added later, and saying "desktop" sent a
    // reader looking for a D-Bus problem that was never there.
    assert!(!warning.to_lowercase().contains("desktop"), "{warning}");
}

#[test]
fn the_broken_transport_warning_names_whichever_program_failed() {
    use crate::escalation::transport::broken_transport_warning;

    for transport in Transport::ALL {
        if let Transport::Program(wire) = transport {
            let warning = broken_transport_warning(wire.program(), "cannot run it");
            assert!(warning.contains(wire.program()), "{warning}");
        }
    }
}
