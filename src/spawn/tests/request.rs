use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::*;

fn full_args() -> SpawnArgs {
    SpawnArgs {
        message: "Fix the flaky fork test".into(),
        title: Some("Fix flaky test".into()),
        name: Some("flaky".into()),
        dir: Some(PathBuf::from("/work/service")),
        agent: Some(Engine::Claude),
        model: Some("opus".into()),
        ..SpawnArgs::default()
    }
}

#[test]
fn normalization_trims_and_drops_empty_optionals() {
    let raw = SpawnArgs {
        message: "  \n Fix the flaky fork test\n\nKeep it small. \t".into(),
        title: Some("  Fix flaky test  ".into()),
        name: Some("   ".into()),
        dir: Some(PathBuf::from(" /work/service ")),
        agent: Some(Engine::Codex),
        model: Some(String::new()),
        ..SpawnArgs::default()
    };

    let normalized = raw.normalized();

    assert_eq!(
        normalized,
        SpawnArgs {
            // Only the ends are trimmed; the Message body keeps its inner layout.
            message: "Fix the flaky fork test\n\nKeep it small.".into(),
            title: Some("Fix flaky test".into()),
            name: None,
            // The directory is kept exactly as given; the broker resolves it.
            dir: Some(PathBuf::from(" /work/service ")),
            agent: Some(Engine::Codex),
            model: None,
            ..SpawnArgs::default()
        }
    );
    assert_eq!(
        normalized.normalized(),
        normalized,
        "normalization is idempotent"
    );
    assert_eq!(
        args("   ").normalized().message,
        "",
        "an empty Message stays a string"
    );
}

#[test]
fn hash_is_stable_across_whitespace_only_differences() {
    let tidy = SpawnArgs {
        name: None,
        model: None,
        ..full_args()
    };
    let padded = SpawnArgs {
        message: "\n  Fix the flaky fork test \t".into(),
        title: Some(" Fix flaky test ".into()),
        name: Some("  ".into()),
        model: Some(String::new()),
        ..full_args()
    };

    assert_eq!(tidy.args_hash(), padded.args_hash());

    // The hash is the lowercase hex sha256 of the normalized args in struct field order;
    // pin those canonical bytes so a field reorder or rename cannot silently change every
    // stored `LaunchRecord.args_hash` and break replay matching.
    let canonical = serde_json::to_string(&padded.normalized()).unwrap();
    assert_eq!(
        canonical,
        r#"{"message":"Fix the flaky fork test","title":"Fix flaky test","name":null,"dir":"/work/service","agent":"claude","model":null}"#
    );
    assert_eq!(
        tidy.args_hash(),
        format!("{:x}", Sha256::digest(canonical.as_bytes()))
    );
    assert_eq!(tidy.args_hash().len(), 64);
    assert!(
        tidy.args_hash()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

#[test]
fn hash_differs_when_any_field_differs() {
    let base = full_args();
    let variants = [
        SpawnArgs {
            message: "Fix the other test".into(),
            ..base.clone()
        },
        SpawnArgs {
            title: Some("Another title".into()),
            ..base.clone()
        },
        SpawnArgs {
            title: None,
            ..base.clone()
        },
        SpawnArgs {
            name: Some("other".into()),
            ..base.clone()
        },
        SpawnArgs {
            name: None,
            ..base.clone()
        },
        // The title moved into the name slot is a different request.
        SpawnArgs {
            title: None,
            name: Some("Fix flaky test".into()),
            ..base.clone()
        },
        SpawnArgs {
            dir: Some(PathBuf::from("/work/other")),
            ..base.clone()
        },
        // The directory is not trimmed, so padding it is a real difference.
        SpawnArgs {
            dir: Some(PathBuf::from(" /work/service")),
            ..base.clone()
        },
        SpawnArgs {
            dir: None,
            ..base.clone()
        },
        SpawnArgs {
            agent: Some(Engine::Codex),
            ..base.clone()
        },
        SpawnArgs {
            agent: None,
            ..base.clone()
        },
        SpawnArgs {
            model: Some("sonnet".into()),
            ..base.clone()
        },
        SpawnArgs {
            model: None,
            ..base.clone()
        },
        // A SESSION IS A DIFFERENT ASK. Same message, same folder, same agent: what differs is
        // whether the child reports a result and exits, so the same id must not match both.
        SpawnArgs {
            kind: LaunchKind::Chat,
            ..base.clone()
        },
    ];

    let mut seen = HashSet::from([base.args_hash()]);
    for variant in variants {
        assert!(
            seen.insert(variant.args_hash()),
            "{variant:?} hashed like an earlier variant"
        );
    }
}

#[test]
fn hash_of_a_non_utf8_dir_is_deterministic_and_never_panics() {
    let with_dir = |bytes: &[u8]| SpawnArgs {
        dir: Some(PathBuf::from(OsStr::from_bytes(bytes))),
        ..full_args()
    };

    let first = with_dir(b"/work/\xffservice").args_hash();

    assert_eq!(first, with_dir(b"/work/\xffservice").args_hash());
    assert_eq!(first.len(), 64);
    assert_ne!(first, full_args().args_hash());
}

#[test]
fn request_json_matches_the_spec_shape_and_absent_optionals_parse_as_none() {
    let req = request(ID, "service", "Fix it");
    let value = serde_json::to_value(&req).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "schema_version": 1,
            "request_id": ID,
            "parent_session": "service",
            "created_at": 1_790_000_000,
            "args": {
                "message": "Fix it",
                "title": null,
                "name": null,
                "dir": null,
                "agent": null,
                "model": null
            }
        })
    );
    let terse = format!(
        r#"{{"schema_version":1,"request_id":"{ID}","parent_session":"service","created_at":1790000000,"args":{{"message":"Fix it"}}}}"#
    );
    assert_eq!(serde_json::from_str::<SpawnRequest>(&terse).unwrap(), req);
    let codex = r#"{"message":"m","agent":"codex","dir":"/work"}"#;
    let parsed: SpawnArgs = serde_json::from_str(codex).unwrap();
    assert_eq!(parsed.agent, Some(Engine::Codex));
    assert_eq!(parsed.dir, Some(PathBuf::from("/work")));
}

/// EVERY JOB REQUEST STILL HASHES AS IT DID BEFORE THE FIELD EXISTED, because the hash is fixed at
/// staging and matched on recovery: a field that changed one existing request's bytes would orphan a
/// child staged by the previous build and recovered by this one. The guarantee is mechanical —
/// serialization skips the kind for a job — and the canonical-bytes assertion in
/// `hash_is_stable_across_whitespace_only_differences` is the other half of it.
#[test]
fn a_job_request_carries_no_kind_on_the_wire_and_an_absent_one_is_a_job() {
    let job = full_args();
    assert_eq!(job.kind, LaunchKind::Job, "the default ask is a job");
    let wire = serde_json::to_string(&job).unwrap();
    assert!(
        !wire.contains("kind"),
        "a job request is byte-identical to one written before the field: {wire}"
    );

    // A request file from that build parses as a job. Read as `Chat` it would stand up a session
    // nobody asked for, which is why this does not use `LaunchKind`'s own default.
    let old: SpawnArgs = serde_json::from_str(
        r#"{"message":"Fix it","title":null,"name":null,"dir":null,"agent":null,"model":null}"#,
    )
    .expect("a v1 request still parses");
    assert_eq!(old.kind, LaunchKind::Job);
    assert!(!old.is_chat());

    // A session says so, and round-trips.
    let session = SpawnArgs {
        kind: LaunchKind::Chat,
        ..full_args()
    };
    let wire = serde_json::to_string(&session).unwrap();
    assert!(wire.contains(r#""kind":"chat""#), "{wire}");
    let back: SpawnArgs = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, session);
    assert!(back.is_chat());
    assert_ne!(
        session.args_hash(),
        full_args().args_hash(),
        "the two asks must not share one hash"
    );
}
