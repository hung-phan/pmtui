//! Which environment variable, if any, says a desktop notification has somewhere to go.
//!
//! Its own file because the rule is one small pure function with four answers, and testing it through
//! `NativeProbe` would mean mutating the process environment — which edition 2024 forbids and which
//! would steer every other test in the binary.

use std::ffi::OsStr;

use super::super::desktop_session_from;

fn os(value: &str) -> Option<&OsStr> {
    Some(OsStr::new(value))
}

/// THE BUS ADDRESS WINS when it is there, because it is the thing `notify-send` actually speaks to.
/// A Wayland or X11 session is accepted in its place: D-Bus autolaunches from either, so delivery can
/// work even with no bus address exported. Each answer NAMES its variable, so the report can say which
/// fact it is standing on rather than just being green.
#[test]
fn each_desktop_signal_is_accepted_and_named_in_priority_order() {
    assert_eq!(
        desktop_session_from(os("unix:path=/run/bus"), None, None),
        Some("DBUS_SESSION_BUS_ADDRESS")
    );
    assert_eq!(
        desktop_session_from(None, os("wayland-0"), None),
        Some("WAYLAND_DISPLAY")
    );
    assert_eq!(desktop_session_from(None, None, os(":0")), Some("DISPLAY"));
    assert_eq!(
        desktop_session_from(os("unix:path=/run/bus"), os("wayland-0"), os(":0")),
        Some("DBUS_SESSION_BUS_ADDRESS"),
        "with all three, the one that is actually spoken to is reported"
    );
}

/// NOTHING SET MEANS NOTHING TO SEND TO — the SSH case, which the check used to call a pass purely
/// because `notify-send` was installed.
#[test]
fn no_desktop_signal_at_all_is_no_session() {
    assert_eq!(desktop_session_from(None, None, None), None);
}

/// AN EMPTY VALUE IS NOT A SESSION. `DISPLAY=` is exported empty by some login shells and service
/// managers, and treating "the name exists" as "a desktop exists" is the same mistake one level down:
/// it would hand back a pass for a variable that names nothing.
#[test]
fn an_exported_but_empty_value_does_not_count() {
    assert_eq!(desktop_session_from(os(""), os(""), os("")), None);
    assert_eq!(
        desktop_session_from(os(""), None, os(":0")),
        Some("DISPLAY"),
        "and an empty one does not mask a real one beside it"
    );
}
