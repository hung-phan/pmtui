//! Discover the MODELS available to each engine's CLI, so the dashboard can offer a pick
//! list. One provider per engine (Claude combines configured allowlists with stable aliases;
//! Codex runs `codex debug models`); adding an engine is one match arm plus one file.
//! Discovery is best-effort: failures never panic or block the dashboard.

use crate::registry::Engine;

mod claude;
mod codex;

/// One selectable model. `label` is shown to the human; `value` is passed to the CLI
/// verbatim (`--model <value>` for claude, `-m <value>` for codex) — already the
/// launch-ready form (claude: provider-specific id or stable alias; codex: slug).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub label: String,
    pub value: String,
}

/// The models available for `engine`, best-effort. Claude can fall back to stable aliases;
/// unrecoverable provider failures return empty and callers render "(default) only".
pub fn available_models(engine: Engine) -> Vec<ModelInfo> {
    match engine {
        Engine::Claude => claude::discover(),
        Engine::Codex => codex::discover(),
    }
}

#[cfg(test)]
mod tests;
