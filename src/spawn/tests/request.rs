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
