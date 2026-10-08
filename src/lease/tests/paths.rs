use super::*;
use std::ffi::OsStr;

#[test]
fn daemon_lock_path_sits_beside_the_registry() {
    let path = daemon_lock_path(Path::new("/home/u/.config/pmd/registry.json"), "pmd");

    assert_eq!(path.parent(), Some(Path::new("/home/u/.config/pmd")));
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pmd-pmd-")
    );
}

#[test]
fn daemon_lock_path_falls_back_to_cwd_when_registry_has_no_parent() {
    let path = daemon_lock_path(Path::new("registry.json"), "pmd");

    assert_eq!(path.parent(), Some(Path::new(".")));
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pmd-pmd-")
    );
}

#[test]
fn daemon_lock_path_differs_by_socket() {
    let registry = Path::new("/home/u/.config/pmd/registry.json");

    assert_ne!(
        daemon_lock_path(registry, "pmd"),
        daemon_lock_path(registry, "alt")
    );
}

#[test]
fn registry_scoped_locks_distinguish_registry_files_in_the_same_directory() {
    let first = Path::new("/home/u/.config/pmd/a.json");
    let second = Path::new("/home/u/.config/pmd/b.json");

    assert_ne!(
        daemon_lock_path(first, "shared"),
        daemon_lock_path(second, "shared")
    );
    assert_ne!(
        daemon_stop_path(first, "shared"),
        daemon_stop_path(second, "shared")
    );
    assert_ne!(
        pmtui_lock_path(first, "shared"),
        pmtui_lock_path(second, "shared")
    );
}

#[test]
fn socket_owner_lock_is_global_for_a_socket() {
    assert_eq!(
        socket_owner_lock_path("shared"),
        socket_owner_lock_path("shared"),
        "one tmux socket may have only one pmd owner"
    );
}

#[test]
fn pmtui_lock_is_beside_the_registry_and_distinct_from_pmd() {
    let registry = Path::new("/home/u/.config/pmd/registry.json");
    let pmtui = pmtui_lock_path(registry, "pmd");

    assert_eq!(pmtui.parent(), Some(Path::new("/home/u/.config/pmd")));
    assert!(
        pmtui
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pmtui-pmd-")
    );
    assert_ne!(
        pmtui,
        daemon_lock_path(registry, "pmd"),
        "pmtui and pmd must not share a lock"
    );
    assert_ne!(
        pmtui_lock_path(registry, "pmd"),
        pmtui_lock_path(registry, "alt")
    );
}

#[test]
fn lock_path_sanitizes_socket_names_without_merging_scopes() {
    let registry = Path::new("/home/u/.config/pmd/registry.json");
    let slash = daemon_lock_path(registry, "team/socket:1");
    let question = daemon_lock_path(registry, "team?socket:1");

    assert!(
        slash
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pmd-team-socket-1-")
    );
    assert_ne!(
        slash, question,
        "the hash must preserve sockets that sanitize to the same filename prefix"
    );
}

#[test]
fn lock_path_uses_a_readable_name_for_an_empty_socket() {
    let path = daemon_lock_path(Path::new("/home/u/.config/pmd/registry.json"), "");

    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pmd-socket-")
    );
}

#[test]
fn runtime_lock_root_prefers_nonempty_xdg_runtime_directory() {
    let root = runtime_lock_root_from(
        Some(OsStr::new("/run/user/1000")),
        Some(OsStr::new("/home/u")),
        Path::new("/tmp"),
    );

    assert_eq!(root, Path::new("/run/user/1000/agent-manager"));
}

#[test]
fn runtime_lock_root_falls_back_from_empty_xdg_to_home() {
    let root = runtime_lock_root_from(
        Some(OsStr::new("")),
        Some(OsStr::new("/home/u")),
        Path::new("/tmp"),
    );

    assert_eq!(root, Path::new("/home/u/.cache/agent-manager/locks"));
}

#[test]
fn runtime_lock_root_falls_back_from_empty_home_to_temp_directory() {
    let root = runtime_lock_root_from(None, Some(OsStr::new("")), Path::new("/private/tmp"));

    assert_eq!(root, Path::new("/private/tmp/agent-manager-locks"));
}

#[test]
fn the_spawn_broker_lock_is_keyed_by_the_registry_alone() {
    assert_eq!(
        spawn_broker_lock_path(Path::new("/home/u/.config/pmd/registry.json")),
        Path::new("/home/u/.config/pmd/registry.json.spawn-broker.lock")
    );
    assert_eq!(
        spawn_broker_lock_path(Path::new("registry.json")),
        Path::new("registry.json.spawn-broker.lock")
    );
}
