//! Claude's configured models come from ~/.claude/settings.json: `availableModels` (short
//! slugs shown to the human) plus `modelOverrides` (slug → provider-specific id). When no
//! allowlist is configured on a direct Anthropic setup, stable subscription aliases keep the
//! picker useful. Provider-backed setups require an explicit list. Values resolve through
//! `modelOverrides`, falling back to the alias; malformed settings fail closed.

use super::ModelInfo;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

const SUBSCRIPTION_MODEL_ALIASES: [&str; 3] = ["sonnet", "opus", "haiku"];

fn subscription_aliases() -> Vec<String> {
    SUBSCRIPTION_MODEL_ALIASES
        .into_iter()
        .map(str::to_string)
        .collect()
}

const PROVIDER_SWITCHES: [&str; 5] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
];
const CUSTOM_PROVIDER_ENDPOINTS: [&str; 1] = ["ANTHROPIC_BASE_URL"];

fn flag_enabled(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
}

fn provider_configured(values: &BTreeMap<String, String>) -> bool {
    for name in PROVIDER_SWITCHES {
        if let Some(value) = values.get(name)
            && flag_enabled(value)
        {
            return true;
        }
    }
    for name in CUSTOM_PROVIDER_ENDPOINTS {
        if let Some(value) = values.get(name)
            && !value.trim().is_empty()
        {
            return true;
        }
    }
    false
}

#[derive(Default)]
enum AvailableModels {
    #[default]
    Absent,
    Present(Vec<String>),
}

fn deserialize_available_models<'de, D>(deserializer: D) -> Result<AvailableModels, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer).map(AvailableModels::Present)
}

#[derive(Deserialize, Default)]
struct Settings {
    #[serde(
        default,
        rename = "availableModels",
        deserialize_with = "deserialize_available_models"
    )]
    available_models: AvailableModels,
    #[serde(default, rename = "modelOverrides")]
    model_overrides: BTreeMap<String, String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// Locate ~/.claude/settings.json — `$CLAUDE_CONFIG_DIR/settings.json` if set, else
/// `$HOME/.claude/settings.json`. Returns None if neither env is set.
fn settings_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR")
        && !dir.trim().is_empty()
    {
        return Some(std::path::Path::new(&dir).join("settings.json"));
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(
        std::path::Path::new(&home)
            .join(".claude")
            .join("settings.json"),
    )
}

fn parse_with_process_provider(body: &str, process_uses_provider: bool) -> Vec<ModelInfo> {
    let Ok(settings) = serde_json::from_str::<Settings>(body) else {
        return Vec::new();
    };
    let settings_use_provider = provider_configured(&settings.env);
    let slugs = match settings.available_models {
        AvailableModels::Absent if !process_uses_provider && !settings_use_provider => {
            subscription_aliases()
        }
        AvailableModels::Absent => Vec::new(),
        AvailableModels::Present(slugs) => slugs,
    };
    slugs
        .into_iter()
        .filter(|slug| !slug.trim().is_empty())
        .filter_map(|slug| {
            let value = settings
                .model_overrides
                .get(&slug)
                .cloned()
                .unwrap_or_else(|| slug.clone());
            (!value.trim().is_empty()).then_some(ModelInfo { label: slug, value })
        })
        .collect()
}

/// Parse a settings.json body into the ordered model list. PURE (no I/O) so it is unit-tested
/// on fixtures. An explicit `availableModels` allowlist is authoritative; when the field is absent
/// on a direct Anthropic setup, use stable subscription aliases. Provider-backed settings require
/// an explicit list. Resolves each slug to its override or itself and drops empty slugs or values.
#[cfg(test)]
pub(super) fn parse(body: &str) -> Vec<ModelInfo> {
    parse_with_process_provider(body, false)
}

pub(super) fn discover() -> Vec<ModelInfo> {
    let mut process_env = BTreeMap::new();
    for name in PROVIDER_SWITCHES {
        if let Ok(value) = std::env::var(name) {
            process_env.insert(name.to_string(), value);
        }
    }
    for name in CUSTOM_PROVIDER_ENDPOINTS {
        if let Ok(value) = std::env::var(name) {
            process_env.insert(name.to_string(), value);
        }
    }
    let process_uses_provider = provider_configured(&process_env);
    let Some(path) = settings_path() else {
        return parse_with_process_provider("{}", process_uses_provider);
    };
    match std::fs::read_to_string(&path) {
        Ok(body) => parse_with_process_provider(&body, process_uses_provider),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            parse_with_process_provider("{}", process_uses_provider)
        }
        Err(_) => Vec::new(),
    }
}
