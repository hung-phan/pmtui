//! The status log, on disk.
//!
//! The dashboard used to keep a bounded in-memory log and draw it in a ` STATUS ` pane under the
//! session list. That pane spent rows the list wanted, could not appear at all below `WIDE_W`
//! columns, dropped everything past the newest twenty lines, and was gone the moment pmtui exited.
//! The human asked for *"a better way to view the log itself"* and chose a file over a pane, so the
//! commentary now goes somewhere that keeps all of it, survives the session, and can be read with
//! the tool already built for reading logs: `tail -f`.
//!
//! BEST EFFORT BY DESIGN. Every failure here is swallowed: the dashboard must draw whether or not a
//! line landed, the line the human is looking at is on the keybar regardless, and a commentary entry
//! is not worth an error of its own. That is stated loudly because silence about a write failure is
//! only defensible when it is deliberate.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log's name inside the dashboard's own directory.
const LOG_FILE: &str = "pmtui.log";

/// Where the status log lives for the dashboard that owns `registry`: beside the registry itself.
///
/// DERIVED from the registry rather than a fixed absolute path, because a scratch registry under
/// `/tmp` must get a scratch log next to it — otherwise every test run and every throwaway dashboard
/// would append to the human's real log. In production the registry is `~/.config/pmd/registry.json`,
/// so this is `~/.config/pmd/pmtui.log`.
pub(crate) fn log_path(registry: &Path) -> PathBuf {
    match registry.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(LOG_FILE),
        // A bare `registry.json` with no directory part: the log joins it in the working directory.
        _ => PathBuf::from(LOG_FILE),
    }
}

/// The size at which the log ROTATES. A dashboard left running for months would otherwise append into
/// the human's config directory forever.
///
/// One MiB is about fifteen thousand status lines, far more history than anyone reads back in one
/// sitting.
pub(crate) const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// How many log files are kept AT MOST, the live one included: `pmtui.log`, `pmtui.log.1`,
/// `pmtui.log.2`. Older generations are deleted as they fall off the end, which is what bounds the
/// footprint at [`MAX_LOG_BYTES`] × this — about 3 MiB — however long pmtui runs.
pub(crate) const MAX_LOG_FILES: usize = 3;

/// The `n`th rotated generation beside the live log: `1` is `pmtui.log.1`, and `0` is the live file
/// itself.
pub(crate) fn generation_path(path: &Path, n: usize) -> PathBuf {
    if n == 0 {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Rotate once the live log has reached [`MAX_LOG_BYTES`]: the oldest generation is DELETED, the rest
/// shift down one, and the live file becomes `.1` so the next append starts a fresh one.
///
/// By rename rather than by trimming in place: a rename is atomic and O(1) whatever the file's size,
/// so a reader never sees a half-rewritten log, and the generation that just rolled off the live file
/// is still there to read. Deleting the oldest is the half that makes the cap a cap — `.3`, `.4`, …
/// accumulating forever is the thing rotation is supposed to prevent.
fn rotate_if_full(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return; // absent or unreadable: nothing to rotate, and the append below meets the real error
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    // The oldest generation we may keep is `MAX_LOG_FILES - 1`; anything at that index now would be
    // pushed past the end, so it goes.
    let _ = fs::remove_file(generation_path(path, MAX_LOG_FILES - 1));
    for n in (1..MAX_LOG_FILES - 1).rev() {
        let _ = fs::rename(generation_path(path, n), generation_path(path, n + 1));
    }
    let _ = fs::rename(path, generation_path(path, 1));
}

/// Append one timestamped line to the log at `path`, creating the file (and its directory) if needed
/// and rotating at [`MAX_LOG_BYTES`]. Failures are dropped — see the module note.
pub(crate) fn append(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    rotate_if_full(path);
    let stamped = format!("{} {line}\n", iso8601_utc(unix_seconds()));
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(stamped.as_bytes()));
}

/// Seconds since the epoch, or `0` when the clock reads before it — only a badly-set clock manages
/// that, and a wrong timestamp still beats a missing log line.
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Hand-rolled rather than pulling in a date crate: one timestamp format does not justify a
/// dependency in a build that runs `cargo audit` and `--locked`, and the civil-calendar conversion
/// below is a published algorithm pinned by tests (the epoch, a leap day, a century non-leap year).
pub(crate) fn iso8601_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let time_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
///
/// It shifts the era to start on 1 March, which puts a leap day at the END of a year's day count and
/// is what removes every February special case.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let marked_month = (5 * day_of_year + 2) / 153; // [0, 11], 0 = March
    let day = day_of_year - (153 * marked_month + 2) / 5 + 1; // [1, 31]
    let month = if marked_month < 10 {
        marked_month + 3
    } else {
        marked_month - 9
    }; // [1, 12]
    let year = year_of_era as i64 + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
