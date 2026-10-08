//! Codex's models come from `codex debug models` → JSON with a `models` array. Each entry
//! carries a large `base_instructions` blob we ignore; we keep slug + display_name for
//! entries with `visibility == "list"`. The slug is what `codex -m` wants, so it is the
//! stored value. Best-effort: any error/non-zero exit yields an empty list.

use super::ModelInfo;
use serde::Deserialize;

#[derive(Deserialize)]
struct Models {
    #[serde(default)]
    models: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    visibility: String,
    #[serde(default = "default_true")]
    supported_in_api: bool,
}
fn default_true() -> bool {
    true
}

/// Parse `codex debug models` JSON. PURE (no process spawn) so it is unit-tested on fixtures.
/// Keeps only `visibility == "list"` && `supported_in_api`, dropping any with an empty slug;
/// label falls back to the slug when display_name is empty.
pub(super) fn parse(body: &str) -> Vec<ModelInfo> {
    let Ok(m) = serde_json::from_str::<Models>(body) else {
        return Vec::new();
    };
    m.models
        .into_iter()
        .filter(|e| e.visibility == "list" && e.supported_in_api && !e.slug.trim().is_empty())
        .map(|e| {
            let label = if e.display_name.trim().is_empty() {
                e.slug.clone()
            } else {
                e.display_name
            };
            ModelInfo {
                label,
                value: e.slug,
            }
        })
        .collect()
}

pub(super) fn discover() -> Vec<ModelInfo> {
    match std::process::Command::new("codex")
        .args(["debug", "models"])
        .output()
    {
        Ok(out) if out.status.success() => parse(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}
