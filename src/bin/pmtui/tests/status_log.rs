//! The status log FILE: where it lands, what a line looks like, and what it does when the write
//! cannot happen.

use crate::status_log::{
    MAX_LOG_BYTES, MAX_LOG_FILES, append, generation_path, iso8601_utc, log_path,
};

/// The log belongs to the dashboard's own directory, so a scratch registry gets a scratch log. This is
/// the property that keeps a test run — or a throwaway `--registry /tmp/x.json` dashboard — from
/// appending to the human's real `~/.config/pmd/pmtui.log`.
#[test]
fn the_log_lands_beside_the_registry_it_narrates() {
    assert_eq!(
        log_path(std::path::Path::new("/home/dev/.config/pmd/registry.json")),
        std::path::PathBuf::from("/home/dev/.config/pmd/pmtui.log")
    );
    assert_eq!(
        log_path(std::path::Path::new("/tmp/scratch/registry.json")),
        std::path::PathBuf::from("/tmp/scratch/pmtui.log")
    );
    // A bare filename has no directory part: the log joins it wherever that is.
    assert_eq!(
        log_path(std::path::Path::new("registry.json")),
        std::path::PathBuf::from("pmtui.log")
    );
}

/// One line per call, timestamped, appended — never truncated, because the whole point of moving the
/// log to a file was keeping what the twenty-entry pane threw away.
#[test]
fn every_line_is_appended_with_a_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/pmtui.log");

    append(&path, "created test (standard)");
    append(&path, "could not pause alpha");

    let text = std::fs::read_to_string(&path).expect("the log was created, directory and all");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text:?}");
    for (line, expected) in lines
        .iter()
        .zip(["created test (standard)", "could not pause alpha"])
    {
        let (stamp, rest) = line.split_once(' ').expect("timestamp then text");
        assert_eq!(rest, expected);
        assert_eq!(stamp.len(), 20, "an ISO-8601 UTC instant: {stamp:?}");
        assert!(
            stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T',
            "{stamp:?}"
        );
        // A real clock, not the epoch fallback: a log of undated lines would be useless.
        assert!(stamp > "2020-01-01T00:00:00Z", "{stamp:?}");
    }
}

/// ROTATION KEEPS AT MOST `MAX_LOG_FILES`, AND DELETES THE REST. A dashboard left running for months
/// would otherwise append into the human's config directory without limit, so at `MAX_LOG_BYTES` the
/// live log shifts down to `.1`, each older generation shifts one further, and whatever falls off the
/// end is removed. Both halves matter: rotating keeps recent history readable, and deleting the oldest
/// is what makes the cap a cap rather than an ever-growing `.3`, `.4`, …
#[test]
fn rotation_keeps_at_most_three_logs_and_deletes_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pmtui.log");
    let names = || {
        let mut found: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        found
    };
    // Fill the live log past the cap, so the NEXT append rotates it.
    let fill = |marker: &str| {
        // Enough copies to cross the cap whatever the cap and the marker are, rather than a magic
        // repeat count that silently stops filling if either changes.
        let line = format!("{marker} generation\n");
        let times = usize::try_from(MAX_LOG_BYTES).unwrap() / line.len() + 1;
        std::fs::write(&path, line.repeat(times)).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() >= MAX_LOG_BYTES);
    };
    let holds = |n: usize, marker: &str| {
        let text = std::fs::read_to_string(generation_path(&path, n))
            .unwrap_or_else(|e| panic!("generation {n} unreadable: {e}"));
        assert!(text.contains(marker), "generation {n} is not {marker:?}");
    };

    // Just under the cap: nothing rotates, and the new line joins what is there.
    let almost = "x".repeat(usize::try_from(MAX_LOG_BYTES).unwrap() - 1);
    std::fs::write(&path, &almost).unwrap();
    append(&path, "kept below the cap");
    let live = std::fs::read_to_string(&path).unwrap();
    assert!(live.contains("kept below the cap") && live.starts_with("xxx"));
    assert_eq!(names(), vec!["pmtui.log".to_string()], "rotated early");

    // First rotation: the full log becomes `.1` and the live file starts fresh.
    fill("first");
    append(&path, "after one rotation");
    holds(0, "after one rotation");
    holds(1, "first");
    assert_eq!(names(), vec!["pmtui.log", "pmtui.log.1"]);

    // Second: `.1` shifts to `.2`, and we are at the cap on file count.
    fill("second");
    append(&path, "after two rotations");
    holds(0, "after two rotations");
    holds(1, "second");
    holds(2, "first");
    assert_eq!(names(), vec!["pmtui.log", "pmtui.log.1", "pmtui.log.2"]);

    // Third: the OLDEST is deleted rather than becoming a fourth file.
    fill("third");
    append(&path, "after three rotations");
    holds(0, "after three rotations");
    holds(1, "third");
    holds(2, "second");
    assert_eq!(
        names().len(),
        MAX_LOG_FILES,
        "rotation must not grow past {MAX_LOG_FILES} files: {:?}",
        names()
    );
    assert!(
        !std::fs::read_to_string(generation_path(&path, 2))
            .unwrap()
            .contains("first"),
        "the oldest generation was kept"
    );
}

/// A write that cannot happen is silent. The dashboard has to draw whether or not the line landed, and
/// the line the human is reading is on the keybar regardless — so this must not panic.
#[test]
fn a_log_that_cannot_be_written_is_dropped_rather_than_fatal() {
    let dir = tempfile::tempdir().unwrap();
    // A FILE where the log's parent directory would have to be: `create_dir_all` and the open both
    // fail, which is the shape of every real failure here (a read-only home, bad permissions).
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();

    append(&blocker.join("pmtui.log"), "swallowed");

    assert_eq!(std::fs::read(&blocker).unwrap(), b"not a directory");
}

/// The civil-calendar conversion, at the dates that break a naive one: the epoch itself, a leap day,
/// the century that is NOT a leap year, and day boundaries either side of midnight.
#[test]
fn iso8601_utc_formats_the_awkward_dates() {
    for (secs, expected) in [
        (0, "1970-01-01T00:00:00Z"),
        (86_399, "1970-01-01T23:59:59Z"),
        (86_400, "1970-01-02T00:00:00Z"),
        // 2000 IS a leap year (divisible by 400), so 29 February exists.
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_709_210_096, "2024-02-29T12:34:56Z"),
        // 2100 is not (divisible by 100, not 400), so 28 February is followed by 1 March.
        (4_107_456_000, "2100-02-28T00:00:00Z"),
        (4_107_542_400, "2100-03-01T00:00:00Z"),
        (1_767_225_600, "2026-01-01T00:00:00Z"),
        (1_772_323_199, "2026-02-28T23:59:59Z"),
    ] {
        assert_eq!(iso8601_utc(secs), expected, "at {secs}");
    }
}
