//! The two checks made BEFORE anything is spent on a consult: whether the supervisor is
//! switched on at all, and whether the stop's own words name an act no opinion may
//! approve. Both answer "should we ask?" rather than "what did it say?", and both are
//! applied by the caller to harness-extracted text.

/// Identifier segments that keep an unstructured native terminal dialog with the human
/// and skip its consult. Worker-authored stops use typed effect metadata instead; this
/// lexical guard exists only because terminal UI text has no structured authority field.
///
/// The inflected forms are spelled out deliberately. Segment matching is exact, so
/// without them `"merged to main"` and `"deploying to production"` would slip through
/// on their verbs (the second happens to be caught by `production`, the first by
/// nothing at all). Over-triggering here costs one escalation to a human, which is the
/// direction this floor is supposed to fail in.
const HARD_FLOOR_MARKERS: &[&str] = &[
    // Irreversible / externally-visible git + release actions.
    "publish",
    "publishing",
    "published",
    "deploy",
    "deploying",
    "deployed",
    "merge",
    "merging",
    "merged",
    "land",
    "landing",
    "landed",
    "push",
    "pushing",
    "pushed",
    "release",
    "releasing",
    "released",
    "rebase",
    "rebasing",
    "rebased",
    "force",
    // Secrets and access.
    "credential",
    "credentials",
    "secret",
    "secrets",
    "token",
    "tokens",
    "apikey",
    "password",
    "access",
    "authorize",
    "authorized",
    "grant",
    "granted",
    "admin",
    "administrator",
    "permission",
    "permissions",
    "privilege",
    "privileges",
    "approval",
    "sudo",
    "revoke",
    "revoking",
    "revoked",
    "rotate",
    "rotating",
    "rotated",
    // Money.
    "payment",
    "charge",
    "charging",
    "charged",
    "refund",
    "refunding",
    "refunded",
    // Destructive data / filesystem actions.
    "delete",
    "deleting",
    "deleted",
    "destroy",
    "destroying",
    "destroyed",
    "drop",
    "dropping",
    "dropped",
    "truncate",
    "truncating",
    "truncated",
    "rm",
    "reset",
    "wipe",
    "wiping",
    "wiped",
    "purge",
    "purging",
    "purged",
    "erase",
    "erasing",
    "erased",
    "overwrite",
    "overwriting",
    "overwritten",
    "clobber",
    "clobbering",
    "clobbered",
    // Schema / data migrations — irreversible; also named by `verify::is_risky_surface`,
    // so the decider floor and the code-risk surface stay aligned.
    "migrate",
    "migrating",
    "migrated",
    "migration",
    "migrations",
    // Environment.
    "prod",
    "production",
];

/// The first [`HARD_FLOOR_MARKERS`] entry present as an identifier *segment* of
/// `text`, or `None`.
///
/// Segment matching, not substring matching — the technique is
/// [`crate::verify::is_risky_surface`]'s, reusing the same splitter
/// ([`crate::verify::segments`]) so the two floors cannot drift: text is split on
/// non-alphanumerics AND camelCase humps, and a segment must EQUAL a marker
/// (case-insensitively). That is what keeps `author` from matching `auth`,
/// `product` from matching `prod`, and `dropdown` from matching `drop`.
pub fn hard_floor_hit(text: &str) -> Option<&'static str> {
    let segments: Vec<String> = crate::verify::segments(text)
        .map(|segment| segment.to_ascii_lowercase())
        .collect();
    if segments
        .windows(2)
        .any(|pair| pair[0] == "api" && pair[1] == "key")
    {
        return Some("api_key");
    }
    for segment in segments {
        if let Some(hit) = HARD_FLOOR_MARKERS
            .iter()
            .copied()
            .find(|marker| *marker == segment)
        {
            return Some(hit);
        }
    }
    None
}

/// [`hard_floor_hit`] over a native terminal dialog's whole surface: its question AND every
/// option. Both are checked because either can identify an authority-changing prompt.
pub fn hard_floor_hit_in(question: &str, options: &[String]) -> Option<&'static str> {
    if let Some(hit) = hard_floor_hit(question) {
        return Some(hit);
    }
    options.iter().find_map(|o| hard_floor_hit(o))
}

/// Is the supervisor enabled, given the raw `PM_SUPERVISOR` value?
///
/// Default **ON** (`None` ⇒ `true`), argued: the string it replaces approves without
/// saying what to do, so the worker guesses; and every failure path in this module
/// lands on either "escalate to the human" or "exactly today's static string", so
/// turning it on cannot be worse than leaving it off except for one cheap model call
/// per auto-flow report. `off`/`0`/`false`/`no` (any case, surrounding whitespace
/// ignored) turn it off — an env switch rather than a config field precisely so it
/// needs no schema migration or serde change.
pub fn supervisor_enabled(raw: Option<&str>) -> bool {
    match raw {
        None => true,
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
    }
}

#[cfg(test)]
mod membership_tests {
    use super::*;

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
}
