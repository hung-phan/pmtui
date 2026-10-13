//! TRAE models come from `traex models --json`, a JSON array whose `name` is the launch value.
//! Discovery is best-effort: a missing CLI, non-zero exit, or malformed output yields no choices.

use super::ModelInfo;
use serde::Deserialize;

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    config_name: String,
    #[serde(default)]
    real_name: String,
    #[serde(default)]
    description: String,
}

/// Parse `traex models --json` output. PURE (no process spawn) so it is unit-tested on fixtures.
pub(super) fn parse(body: &str) -> Vec<ModelInfo> {
    let Ok(entries) = serde_json::from_str::<Vec<Entry>>(body) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|entry| !entry.name.trim().is_empty())
        .map(|entry| {
            let name = entry.name;
            let title = if !entry.config_name.trim().is_empty() {
                entry.config_name
            } else if entry.real_name.trim().is_empty() {
                name.clone()
            } else {
                entry.real_name
            };
            let label = if entry.description.trim().is_empty() {
                title
            } else {
                format!("{title} - {}", entry.description)
            };
            ModelInfo { label, value: name }
        })
        .collect()
}

pub(super) fn discover() -> Vec<ModelInfo> {
    match std::process::Command::new("traex")
        .args(["models", "--json"])
        .output()
    {
        Ok(out) if out.status.success() => parse(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}
