use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::{Arc, Barrier};

use tempfile::tempdir;

use super::super::files::create_temp;
use super::*;

/// A distinct, valid request id per `n`.
fn id(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn on_disk(dir: &Path, id: &str) -> SpawnRequest {
    serde_json::from_slice(&fs::read(request_path(dir, id)).unwrap()).unwrap()
}

/// One line per entry, so a whole scan compares in a single assertion.
fn summary(entries: &[ScanEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry {
            ScanEntry::Valid(req) => format!("valid {}", req.request_id),
            ScanEntry::Invalid { request_id, .. } => format!("invalid {request_id}"),
            ScanEntry::Skipped { name, .. } => format!("skipped {name}"),
        })
        .collect()
}

fn invalid_reason<'a>(entries: &'a [ScanEntry], wanted: &str) -> &'a str {
    entries
        .iter()
        .find_map(|entry| match entry {
            ScanEntry::Invalid { request_id, reason } if request_id == wanted => {
                Some(reason.as_str())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no invalid entry for {wanted}"))
}

#[cfg(target_os = "linux")]
fn mkfifo(path: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
}

#[test]
fn request_ids_are_canonical_lowercase_uuids_only() {
    for valid in [ID, OTHER_ID, "00000000-0000-0000-0000-000000000000"] {
        assert!(is_valid_request_id(valid), "{valid:?} should be valid");
    }
    let upper = ID.to_uppercase();
    let unhyphenated = ID.replace('-', "");
    let too_long = format!("{ID}0");
    let padded = format!(" {ID}");
    let newline = format!("{ID}\n");
    let braced = format!("{{{ID}}}");
    for invalid in [
        "",
        upper.as_str(),
        unhyphenated.as_str(),
        too_long.as_str(),
        &ID[..35],
        padded.as_str(),
        newline.as_str(),
        braced.as_str(),
        "550e8400-e29b-41d4-a716-44665544000g",
        "550e840-0e29b-41d4-a716-446655440000",
        "550e8400_e29b_41d4_a716_446655440000",
        "../../../../../../../../../etc/passwd",
    ] {
        assert!(
            !is_valid_request_id(invalid),
            "{invalid:?} should be invalid"
        );
    }
}

#[test]
fn publish_creates_0700_dir_and_final_file_without_temp_leftovers() {
    let root = tempdir().unwrap();
    let state_dir = root.path().join(".project-state/sessions/service-1a2b3c4d");
    let dir = requests_dir(&state_dir);
    let req = request(ID, "service", "Fix it");

    assert_eq!(dir, state_dir.join("spawn-requests"));
    assert_eq!(publish_request(&dir, &req).unwrap(), Publish::Published);

    assert_eq!(mode(&dir), 0o700);
    let path = request_path(&dir, ID);
    assert_eq!(path, dir.join(format!("{ID}.request.json")));
    assert_eq!(mode(&path), 0o600);
    assert_eq!(on_disk(&dir, ID), req);
    assert_eq!(names_in(&dir), [format!("{ID}.request.json")]);
}

#[test]
fn publish_tightens_an_existing_requests_dir_to_0700() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    publish_request(&dir, &request(ID, "service", "Fix it")).unwrap();

    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn second_publish_of_the_same_id_returns_already_present_with_the_first_content() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    let first = request(ID, "service", "first");
    let second = SpawnRequest {
        args: args("second"),
        created_at: first.created_at + 60,
        ..first.clone()
    };

    assert_eq!(publish_request(&dir, &first).unwrap(), Publish::Published);
    assert_eq!(
        publish_request(&dir, &second).unwrap(),
        Publish::AlreadyPresent(Box::new(first.clone()))
    );

    assert_eq!(
        on_disk(&dir, ID),
        first,
        "the first request is never overwritten"
    );
    assert_eq!(names_in(&dir), [format!("{ID}.request.json")]);
}

#[test]
fn an_existing_request_is_found_before_anything_is_staged() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    let first = request(ID, "service", "first");
    publish_request(&dir, &first).unwrap();
    // Nothing may be written into the directory: not even a temp that is removed again, which
    // would fail on a full disk and hide the request that is already there.
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs::File::open(&dir).unwrap().set_modified(past).unwrap();
    let modified = || fs::metadata(&dir).unwrap().modified().unwrap();
    assert_eq!(modified(), past);

    assert_eq!(
        publish_request(&dir, &request(ID, "service", "second")).unwrap(),
        Publish::AlreadyPresent(Box::new(first.clone()))
    );
    assert_eq!(modified(), past, "no temp file was staged");

    // A directory it cannot write to (as a full disk would refuse the temp) still reports it.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let found = publish_request(&dir, &request(ID, "service", "third"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(found.unwrap(), Publish::AlreadyPresent(Box::new(first)));
}

#[test]
fn publish_never_overwrites_under_a_race() {
    for round in 0..16 {
        let root = tempdir().unwrap();
        let dir = requests_dir(root.path());
        let barrier = Arc::new(Barrier::new(2));
        let racers: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|message| {
                let (dir, barrier) = (dir.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    let req = request(ID, "service", message);
                    barrier.wait();
                    let outcome = publish_request(&dir, &req).unwrap();
                    (req, outcome)
                })
            })
            .collect();
        let results: Vec<(SpawnRequest, Publish)> = racers
            .into_iter()
            .map(|racer| racer.join().unwrap())
            .collect();

        let winners: Vec<&SpawnRequest> = results
            .iter()
            .filter(|(_, outcome)| *outcome == Publish::Published)
            .map(|(req, _)| req)
            .collect();
        assert_eq!(winners.len(), 1, "round {round}: exactly one publish wins");
        let winner = winners[0];
        for (req, outcome) in &results {
            if req != winner {
                assert_eq!(
                    *outcome,
                    Publish::AlreadyPresent(Box::new(winner.clone())),
                    "round {round}: the loser sees the winner's request"
                );
            }
        }
        assert_eq!(&on_disk(&dir, ID), winner, "round {round}");
        assert_eq!(
            names_in(&dir),
            [format!("{ID}.request.json")],
            "round {round}"
        );
    }
}

#[test]
fn publish_rejects_an_invalid_id_before_touching_the_filesystem() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());

    let error = publish_request(&dir, &request("../escape", "service", "Fix it")).unwrap_err();

    assert!(format!("{error:#}").contains("request id"), "{error:#}");
    assert!(!dir.exists());
    assert_eq!(names_in(root.path()), Vec::<String>::new());
}

#[test]
fn publish_refuses_a_symlinked_requests_dir() {
    let root = tempdir().unwrap();
    let real = root.path().join("elsewhere");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
    let dir = requests_dir(root.path());
    symlink(&real, &dir).unwrap();

    let error = publish_request(&dir, &request(ID, "service", "Fix it")).unwrap_err();

    assert!(
        format!("{error:#}").contains("not a real directory"),
        "{error:#}"
    );
    assert_eq!(names_in(&real), Vec::<String>::new());
    assert_eq!(mode(&real), 0o755, "the symlink target is never chmodded");
}

#[test]
fn publish_fails_cleanly_when_it_cannot_serialize_or_create_the_dir() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    let unserializable = SpawnRequest {
        args: SpawnArgs {
            dir: Some(OsStr::from_bytes(b"/work/\xff").into()),
            ..args("Fix it")
        },
        ..request(ID, "service", "Fix it")
    };

    assert!(publish_request(&dir, &unserializable).is_err());
    assert!(
        !dir.exists(),
        "nothing is created for a request that cannot be written"
    );

    let file = root.path().join("state-dir-is-a-file");
    fs::write(&file, b"").unwrap();
    let error =
        publish_request(&requests_dir(&file), &request(ID, "service", "Fix it")).unwrap_err();
    assert!(format!("{error:#}").contains("create"), "{error:#}");
}

#[test]
fn publish_reports_an_unreadable_existing_request_instead_of_overwriting_it() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir(&dir).unwrap();
    fs::write(request_path(&dir, ID), b"{ truncated").unwrap();

    assert!(publish_request(&dir, &request(ID, "service", "Fix it")).is_err());
    assert_eq!(fs::read(request_path(&dir, ID)).unwrap(), b"{ truncated");
    assert_eq!(names_in(&dir), [format!("{ID}.request.json")]);

    // A planted symlink at the final name is neither followed nor replaced.
    let target = root.path().join("target.json");
    fs::write(
        &target,
        serde_json::to_vec(&request(OTHER_ID, "service", "x")).unwrap(),
    )
    .unwrap();
    symlink(&target, request_path(&dir, OTHER_ID)).unwrap();
    assert!(publish_request(&dir, &request(OTHER_ID, "service", "Fix it")).is_err());
    assert!(
        fs::symlink_metadata(request_path(&dir, OTHER_ID))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        serde_json::from_slice::<SpawnRequest>(&fs::read(&target).unwrap()).unwrap(),
        request(OTHER_ID, "service", "x")
    );
    assert_eq!(names_in(&dir).len(), 2, "no temp file is left behind");
}

#[test]
fn a_stale_or_planted_temp_is_replaced_never_followed() {
    let root = tempdir().unwrap();
    let stale = root.path().join(".stale.tmp");
    fs::write(&stale, b"stale bytes").unwrap();

    create_temp(&stale).unwrap().write_all(b"fresh").unwrap();

    assert_eq!(fs::read(&stale).unwrap(), b"fresh");
    assert_eq!(mode(&stale), 0o600);

    let target = root.path().join("target");
    fs::write(&target, b"keep").unwrap();
    let planted = root.path().join(".planted.tmp");
    symlink(&target, &planted).unwrap();

    create_temp(&planted).unwrap().write_all(b"new").unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert!(
        !fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&planted).unwrap(), b"new");
    assert!(create_temp(&root.path().join("missing/dir/.x.tmp")).is_err());
}

#[test]
fn scan_skips_bad_names_and_ignores_receipts_and_temps() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    let req = request(ID, "service", "Fix it");
    publish_request(&dir, &req).unwrap();
    write_receipt(&dir, &receipt(ID, ReceiptState::Claimed)).unwrap();
    let body = serde_json::to_vec(&req).unwrap();
    let upper = format!("{}.request.json", ID.to_uppercase());
    let unhyphenated = format!("{}.request.json", ID.replace('-', ""));
    for name in [
        upper.as_str(),
        unhyphenated.as_str(),
        "not-a-uuid.request.json",
    ] {
        fs::write(dir.join(name), &body).unwrap();
    }
    for ignored in [
        format!(".{OTHER_ID}.4242.0.tmp"),
        format!(".{OTHER_ID}.receipt.json.tmp.4242.0"),
        format!("{OTHER_ID}.receipt.json"),
        format!("{OTHER_ID}.request.json.bak"),
        "notes.txt".to_string(),
    ] {
        fs::write(dir.join(ignored), b"{").unwrap();
    }
    fs::write(dir.join(OsStr::from_bytes(b"\xff.request.json")), &body).unwrap();

    let scanned = scan_requests(&dir, "service");

    assert_eq!(
        summary(&scanned),
        [
            format!("skipped {upper}"),
            format!("valid {ID}"),
            format!("skipped {unhyphenated}"),
            "skipped not-a-uuid.request.json".to_string(),
            "skipped \u{fffd}.request.json".to_string(),
        ]
    );
    assert_eq!(scanned[1], ScanEntry::Valid(Box::new(req)));
    for entry in &scanned {
        if let ScanEntry::Skipped { reason, .. } = entry {
            assert!(reason.contains("UUID"), "{reason}");
        }
    }
}

#[test]
fn scan_marks_symlink_oversize_bad_json_wrong_parent_and_stem_mismatch_invalid() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir(&dir).unwrap();
    let body = |id: &str| serde_json::to_vec(&request(id, "service", "Fix it")).unwrap();
    let padded = |id: &str, len: u64| {
        let mut bytes = body(id);
        bytes.resize(len as usize, b' ');
        bytes
    };

    let outside = root.path().join("outside.json");
    fs::write(&outside, body(&id(1))).unwrap();
    symlink(&outside, request_path(&dir, &id(1))).unwrap();
    fs::write(
        request_path(&dir, &id(2)),
        padded(&id(2), MAX_REQUEST_BYTES + 1),
    )
    .unwrap();
    fs::write(
        request_path(&dir, &id(3)),
        padded(&id(3), MAX_REQUEST_BYTES),
    )
    .unwrap();
    let whole = body(&id(4));
    fs::write(request_path(&dir, &id(4)), &whole[..whole.len() / 2]).unwrap();
    fs::write(
        request_path(&dir, &id(5)),
        serde_json::to_vec(&request(&id(5), "someone-else", "Fix it")).unwrap(),
    )
    .unwrap();
    fs::write(request_path(&dir, &id(6)), body(&id(7))).unwrap();
    let future = SpawnRequest {
        schema_version: 2,
        ..request(&id(8), "service", "Fix it")
    };
    fs::write(
        request_path(&dir, &id(8)),
        serde_json::to_vec(&future).unwrap(),
    )
    .unwrap();
    fs::create_dir(request_path(&dir, &id(9))).unwrap();
    fs::write(request_path(&dir, &id(10)), body(&id(10))).unwrap();

    let scanned = scan_requests(&dir, "service");

    assert_eq!(
        summary(&scanned),
        [
            format!("invalid {}", id(1)),
            format!("invalid {}", id(2)),
            format!("valid {}", id(3)),
            format!("invalid {}", id(4)),
            format!("invalid {}", id(5)),
            format!("invalid {}", id(6)),
            format!("invalid {}", id(8)),
            format!("invalid {}", id(9)),
            format!("valid {}", id(10)),
        ]
    );
    for (n, keyword) in [
        (1, "symlink"),
        (2, "larger than 65536 bytes"),
        (4, "JSON"),
        (5, "parent_session"),
        (6, "file name"),
        (8, "schema_version 2"),
        (9, "not a regular file"),
    ] {
        let reason = invalid_reason(&scanned, &id(n));
        assert!(reason.contains(keyword), "id({n}): {reason:?}");
    }
    assert_eq!(
        scanned[2],
        ScanEntry::Valid(Box::new(request(&id(3), "service", "Fix it"))),
        "a request of exactly the size cap is accepted"
    );
}

#[test]
fn scan_of_a_missing_symlinked_or_non_directory_requests_dir() {
    let root = tempdir().unwrap();
    assert!(scan_requests(&root.path().join("absent"), "service").is_empty());

    let real = root.path().join("real");
    publish_request(&real, &request(ID, "service", "Fix it")).unwrap();
    let link = root.path().join("link");
    symlink(&real, &link).unwrap();
    let file = root.path().join("file");
    fs::write(&file, b"").unwrap();

    for dir in [link.clone(), file.clone(), file.join(REQUESTS_DIR)] {
        let scanned = scan_requests(&dir, "service");
        assert_eq!(
            summary(&scanned),
            [format!("skipped {}", dir.display())],
            "{}",
            dir.display()
        );
    }
    assert!(matches!(
        &scan_requests(&link, "service")[0],
        ScanEntry::Skipped { reason, .. } if reason.contains("not a real directory")
    ));
}

#[test]
fn receipt_round_trip_and_request_removal_keeps_the_receipt() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    publish_request(&dir, &request(ID, "service", "Fix it")).unwrap();
    assert_eq!(read_receipt(&dir, ID).unwrap(), None);

    let claimed = receipt(ID, ReceiptState::Claimed);
    write_receipt(&dir, &claimed).unwrap();
    assert_eq!(read_receipt(&dir, ID).unwrap(), Some(claimed));

    let ready = ready_receipt(ID);
    write_receipt(&dir, &ready).unwrap();
    assert_eq!(read_receipt(&dir, ID).unwrap(), Some(ready.clone()));
    assert_eq!(
        receipt_path(&dir, ID),
        dir.join(format!("{ID}.receipt.json"))
    );
    assert_eq!(
        names_in(&dir),
        [format!("{ID}.receipt.json"), format!("{ID}.request.json")],
        "receipt writes leave no temp files"
    );

    // Retention removes the request only: the receipt stays as the request's tombstone.
    remove_request(&dir, ID).unwrap();
    assert_eq!(names_in(&dir), [format!("{ID}.receipt.json")]);
    remove_request(&dir, ID).unwrap();
    remove_request(&root.path().join("absent"), ID).unwrap();
    assert_eq!(read_receipt(&dir, ID).unwrap(), Some(ready));
}

#[test]
fn receipt_reads_reject_symlinks_oversize_bad_json_and_another_id() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir(&dir).unwrap();
    let body = serde_json::to_vec(&receipt(ID, ReceiptState::Ready)).unwrap();

    let outside = root.path().join("outside.json");
    fs::write(&outside, &body).unwrap();
    symlink(&outside, receipt_path(&dir, ID)).unwrap();
    assert!(
        read_receipt(&dir, ID).is_err(),
        "a symlinked receipt is refused"
    );
    fs::remove_file(receipt_path(&dir, ID)).unwrap();

    let mut oversize = body.clone();
    oversize.resize(MAX_REQUEST_BYTES as usize + 1, b' ');
    fs::write(receipt_path(&dir, ID), oversize).unwrap();
    let error = read_receipt(&dir, ID).unwrap_err();
    assert!(format!("{error:#}").contains("larger than"), "{error:#}");

    fs::write(receipt_path(&dir, ID), &body[..body.len() - 1]).unwrap();
    assert!(read_receipt(&dir, ID).is_err(), "truncated JSON is refused");

    fs::write(
        receipt_path(&dir, ID),
        serde_json::to_vec(&receipt(OTHER_ID, ReceiptState::Ready)).unwrap(),
    )
    .unwrap();
    let error = read_receipt(&dir, ID).unwrap_err();
    assert!(format!("{error:#}").contains(OTHER_ID), "{error:#}");
}

#[cfg(target_os = "linux")]
#[test]
fn fifo_requests_and_receipts_are_rejected_without_blocking() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir(&dir).unwrap();
    mkfifo(&request_path(&dir, ID));
    mkfifo(&receipt_path(&dir, ID));

    let (tx, rx) = mpsc::channel();
    let reader_dir = dir.clone();
    let reader = std::thread::spawn(move || {
        let scanned = scan_requests(&reader_dir, "service");
        let receipt = read_receipt(&reader_dir, ID);
        tx.send((scanned, receipt.is_err())).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_millis(500));
    // Unblock a reader stuck opening a FIFO so a regression fails instead of hanging. A
    // non-blocking writer open succeeds only where a reader waits (ENXIO elsewhere), so this
    // itself never blocks; it repeats because the reader may reach the second FIFO later.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !reader.is_finished() && Instant::now() < deadline {
        for path in [request_path(&dir, ID), receipt_path(&dir, ID)] {
            drop(rustix::fs::open(
                &path,
                rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.join().unwrap();

    let (scanned, receipt_refused) = result.expect("reading a FIFO blocked");
    assert!(receipt_refused, "a FIFO receipt is refused");
    assert!(invalid_reason(&scanned, ID).contains("not a regular file"));
}

#[test]
fn receipt_writes_refuse_a_symlinked_or_missing_dir() {
    let root = tempdir().unwrap();
    let real = root.path().join("elsewhere");
    fs::create_dir(&real).unwrap();
    let link = requests_dir(root.path());
    symlink(&real, &link).unwrap();

    assert!(write_receipt(&link, &receipt(ID, ReceiptState::Claimed)).is_err());
    assert_eq!(names_in(&real), Vec::<String>::new());
    assert!(
        write_receipt(
            &root.path().join("absent"),
            &receipt(ID, ReceiptState::Claimed)
        )
        .is_err()
    );
}

#[test]
fn receipt_io_and_removal_refuse_an_invalid_id() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir(&dir).unwrap();
    let escape = "../escape";

    assert!(read_receipt(&dir, escape).is_err());
    assert!(write_receipt(&dir, &receipt(escape, ReceiptState::Claimed)).is_err());
    assert!(remove_request(&dir, escape).is_err());
    assert_eq!(names_in(root.path()), [REQUESTS_DIR.to_string()]);
}

/// A cancel marker is created in a directory that may not exist yet, is idempotent, and is removed
/// again once acted on — the same no-clobber publish a request gets, with nothing inside it.
#[test]
fn a_cancel_marker_is_published_once_and_removed_again() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());

    assert!(!cancel_requested(&dir, ID), "nothing asked for yet");
    assert!(
        publish_cancel(&dir, ID).unwrap(),
        "the first ask creates it"
    );
    assert!(
        !publish_cancel(&dir, ID).unwrap(),
        "the second is the same ask"
    );
    assert!(cancel_requested(&dir, ID));
    assert_eq!(fs::read(cancel_path(&dir, ID)).unwrap(), Vec::<u8>::new());

    remove_cancel(&dir, ID).unwrap();
    assert!(!cancel_requested(&dir, ID));
    remove_cancel(&dir, ID).expect("removing a marker that is already gone is fine");
}

/// TWO CANCELS AT ONCE still write one marker, and exactly one caller is told it created it — the same
/// no-clobber link a request gets, under the same race.
#[test]
fn two_cancels_racing_create_exactly_one_marker() {
    for round in 0..16 {
        let root = tempdir().unwrap();
        let dir = requests_dir(root.path());
        let barrier = Arc::new(Barrier::new(2));
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let (dir, barrier) = (dir.clone(), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    publish_cancel(&dir, ID).unwrap()
                })
            })
            .collect();
        let created = racers
            .into_iter()
            .map(|racer| racer.join().expect("a racer finished"))
            .filter(|created| *created)
            .count();
        assert_eq!(created, 1, "round {round}: exactly one cancel creates it");
        assert!(cancel_requested(&dir, ID));
    }
}

/// A cancel that CANNOT be written says so rather than reporting a stop nobody recorded.
#[test]
fn an_unwritable_cancel_is_reported() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();

    let error = publish_cancel(&dir, ID).unwrap_err();

    assert!(format!("{error:#}").contains("publish"), "{error:#}");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!cancel_requested(&dir, ID));
}

/// THE DIRECTORY IS HOSTILE, as everywhere else here: an invalid id is refused, and a marker that is
/// not a regular file is ignored rather than obeyed — a dangling symlink must not stop a job.
#[test]
fn a_cancel_marker_that_is_not_a_file_is_ignored() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir_all(&dir).unwrap();

    assert!(publish_cancel(&dir, "../escape").is_err());
    assert!(!cancel_requested(&dir, "../escape"));
    assert!(remove_cancel(&dir, "not-a-uuid").is_err());

    symlink("/nonexistent/target", cancel_path(&dir, ID)).unwrap();
    assert!(
        !cancel_requested(&dir, ID),
        "a symlink is not a cancel anyone wrote here"
    );
    fs::create_dir(cancel_path(&dir, OTHER_ID)).unwrap();
    assert!(!cancel_requested(&dir, OTHER_ID), "nor is a directory");
}

#[test]
fn remove_request_reports_a_real_removal_error() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    fs::create_dir_all(request_path(&dir, ID)).unwrap();

    let error = remove_request(&dir, ID).unwrap_err();

    assert!(format!("{error:#}").contains("remove"), "{error:#}");
    assert!(request_path(&dir, ID).exists());
}

#[test]
fn listing_names_requests_without_opening_them() {
    let root = tempdir().unwrap();
    let dir = requests_dir(root.path());
    publish_request(&dir, &request(ID, "service", "Fix it")).unwrap();
    // A request no one may read: listing it must not need to open it.
    let locked = request_path(&dir, OTHER_ID);
    fs::write(&locked, b"{}").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    fs::write(dir.join("not-a-uuid.request.json"), b"{").unwrap();
    write_receipt(&dir, &receipt(ID, ReceiptState::Claimed)).unwrap();

    let listed = list_requests(&dir);

    assert_eq!(
        listed[..2],
        [
            Listed::Request(ID.to_string()),
            Listed::Request(OTHER_ID.to_string()),
        ]
    );
    assert!(matches!(
        &listed[2],
        Listed::Skipped { name, reason }
            if name == "not-a-uuid.request.json" && reason.contains("UUID")
    ));
    assert_eq!(listed.len(), 3);
    assert!(list_requests(&root.path().join("absent")).is_empty());

    assert_eq!(
        load_request_entry(&dir, ID, "service").unwrap().request_id,
        ID
    );
    assert!(
        load_request_entry(&dir, ID, "sibling")
            .unwrap_err()
            .contains("parent_session")
    );
    assert!(load_request_entry(&dir, OTHER_ID, "service").is_err());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
}
