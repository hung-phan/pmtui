//! The stop/completion proposals a worker hands back. Every field is a PROPOSAL — the
//! harness validates and commits — so the schema is strict (`deny_unknown_fields`).
//! [`StopDraft`] is the shape an agent marker carries into the ledger; [`CompletionDigest`]
//! is the evidence `verify` accepts a code phase from.

use serde::{Deserialize, Serialize};

use crate::pmstate::StopKind;
use crate::state::RiskClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffectScope {
    Local,
    External,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffectReversibility {
    Reversible,
    Irreversible,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffectAuthority {
    Ordinary,
    Privileged,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StopEffect {
    #[serde(default)]
    pub scope: EffectScope,
    #[serde(default)]
    pub reversibility: EffectReversibility,
    #[serde(default)]
    pub authority: EffectAuthority,
    /// The worker supplied effect keys this harness version does not understand. Known axes are
    /// preserved and the uncertainty is shown to the decider/audit rather than erasing evidence.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unrecognized_metadata: bool,
}

impl<'de> Deserialize<'de> for StopEffect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawEffect {
            #[serde(default)]
            scope: EffectScope,
            #[serde(default)]
            reversibility: EffectReversibility,
            #[serde(default)]
            authority: EffectAuthority,
            #[serde(default)]
            unrecognized_metadata: bool,
            #[serde(flatten)]
            extra: std::collections::BTreeMap<String, serde_json::Value>,
        }

        let raw = RawEffect::deserialize(deserializer)?;
        Ok(Self {
            scope: raw.scope,
            reversibility: raw.reversibility,
            authority: raw.authority,
            unrecognized_metadata: raw.unrecognized_metadata || !raw.extra.is_empty(),
        })
    }
}

impl Default for StopEffect {
    fn default() -> Self {
        Self {
            scope: EffectScope::Unknown,
            reversibility: EffectReversibility::Unknown,
            authority: EffectAuthority::Unknown,
            unrecognized_metadata: false,
        }
    }
}

impl StopEffect {
    /// Whether the worker explicitly reported an effect that belongs to the human. Unknown axes
    /// are uncertainty for the goal-aware decider to investigate, not proof that the action is
    /// external or privileged.
    pub fn requires_human(self) -> bool {
        matches!(self.scope, EffectScope::External)
            || matches!(self.reversibility, EffectReversibility::Irreversible)
            || matches!(self.authority, EffectAuthority::Privileged)
    }

    pub fn summary(self) -> String {
        let mut summary = format!(
            "scope={}, reversibility={}, authority={}",
            effect_scope_label(self.scope),
            effect_reversibility_label(self.reversibility),
            effect_authority_label(self.authority)
        );
        if self.unrecognized_metadata {
            summary.push_str(", unrecognized_metadata=true");
        }
        summary
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn effect_scope_label(value: EffectScope) -> &'static str {
    match value {
        EffectScope::Local => "local",
        EffectScope::External => "external",
        EffectScope::Unknown => "unknown",
    }
}

fn effect_reversibility_label(value: EffectReversibility) -> &'static str {
    match value {
        EffectReversibility::Reversible => "reversible",
        EffectReversibility::Irreversible => "irreversible",
        EffectReversibility::Unknown => "unknown",
    }
}

fn effect_authority_label(value: EffectAuthority) -> &'static str {
    match value {
        EffectAuthority::Ordinary => "ordinary",
        EffectAuthority::Privileged => "privileged",
        EffectAuthority::Unknown => "unknown",
    }
}

/// A stop the worker proposes. The harness re-derives the risk floor from `kind`
/// and `effect` (policy) rather than trusting `risk_class` blindly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopDraft {
    pub kind: StopKind,
    #[serde(default)]
    pub effect: StopEffect,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub context_ref: Option<String>,
    pub risk_class: RiskClass,
}

/// Evidence of completed work, used by `verify` to accept a code phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionDigest {
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub commit_ref: Option<String>,
    #[serde(default)]
    pub tests: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
}
