use super::super::{hard_floor_hit, hard_floor_hit_in, supervisor_enabled};

#[test]
fn hard_floor_matches_segments_without_matching_identifier_substrings() {
    for hit in [
        "Shall I deploy the new build?",
        "ready to MERGE to main?",
        "which api_key should I use?",
        "need a credential for the S3 bucket",
        "delete the old rows?",
        "run rm on the temp dir?",
        "point it at prod?",
        "push to origin?",
        "rotateSigningKey now?",
        "DROP TABLE users;",
        "should I issue a refund?",
        "git reset --hard?",
        "force it through?",
        "grant AdministratorAccess?",
    ] {
        assert!(hard_floor_hit(hit).is_some(), "should hit: {hit}");
    }

    for miss in [
        "who is the author of this file?",
        "which product name should the header use?",
        "use a dropdown or a radio group?",
        "is the landmark test still needed?",
        "call it `merger` or `combiner`?",
        "rename `resetter` to `clearer`?",
        "should the form field be required?",
        "name it `tokenizer` or `lexer`?",
        "wrap it in a `Charger` struct?",
        "pick a name for the `Pusher` trait?",
    ] {
        assert_eq!(hard_floor_hit(miss), None, "must not hit: {miss}");
    }
    assert_eq!(hard_floor_hit("author"), None);
    assert_eq!(hard_floor_hit("auth"), None);
}

#[test]
fn hard_floor_checks_options_as_well_as_the_question() {
    assert_eq!(hard_floor_hit("Ready to ship?"), None);
    assert_eq!(
        hard_floor_hit_in(
            "Ready to ship?",
            &["deploy now".to_string(), "wait".to_string()]
        ),
        Some("deploy")
    );
    assert_eq!(
        hard_floor_hit_in("Which colour?", &["red".to_string(), "blue".to_string()]),
        None
    );
}

#[test]
fn supervisor_kill_switch_defaults_on_and_accepts_off_spellings() {
    assert!(supervisor_enabled(None));
    for on in ["", "on", "1", "true", "yes", "anything"] {
        assert!(supervisor_enabled(Some(on)), "{on:?}");
    }
    for off in ["off", "OFF", " off ", "0", "false", "FALSE", "no"] {
        assert!(!supervisor_enabled(Some(off)), "{off:?}");
    }
}

// Moved out of `advise/preflight.rs`, beside the rest of this module's tests.
use super::super::preflight::HARD_FLOOR_MARKERS;

#[test]
fn every_hard_floor_marker_hits_the_floor_bare_and_inside_a_host() {
    // Iterate the ACTUAL list so a silently-dropped or misspelled entry (one that no longer
    // equals its own lowercased segment, or a segment-splitter regression) fails here rather
    // than quietly shrinking the deterministic safety floor.
    for m in HARD_FLOOR_MARKERS {
        assert_eq!(
            hard_floor_hit(m),
            Some(*m),
            "bare marker `{m}` must hit its own floor"
        );
        let snake = format!("do_{m}_now");
        assert!(
            hard_floor_hit(&snake).is_some(),
            "`{m}` must be found as a segment of `{snake}`"
        );
        let camel = {
            let mut c = m.chars();
            let head = c
                .next()
                .map(|f| f.to_uppercase().collect::<String>())
                .unwrap_or_default();
            format!("do{head}{}Now", c.as_str())
        };
        assert!(
            hard_floor_hit(&camel).is_some(),
            "`{m}` must be found as a camelCase hump of `{camel}`"
        );
    }
}
