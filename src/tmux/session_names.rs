//! Stable tmux names for one project session and its transient supervisor consults.

use std::path::Path;

/// The one persistent tmux terminal for a project session. pmtui attaches to it;
/// pmd optionally drives it while autonomy is enabled.
pub fn session_name(id: &str, root: &Path) -> String {
    let key = format!("{id}\u{0}{}", root.to_string_lossy());
    format!("pm-{}-{}", sanitize_name(id), hash8(&key))
}

pub fn job_session_name(id: &str, root: &Path, seq: u64) -> String {
    format!("{}-job-{seq}", session_name(id, root))
}

/// A detached supervisor consult remains separate from the project terminal.
pub fn supervisor_session_name(id: &str, root: &Path, seq: u64) -> String {
    let key = format!("{id}\u{0}{}", root.to_string_lossy());
    format!("pmsup-{}-{}-{}", sanitize_name(id), hash8(&key), seq)
}

fn sanitize_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn hash8(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}
