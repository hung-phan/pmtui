//! The agent-owned request: what the child should be, and who asked for it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::clock::Epoch;
use crate::registry::Engine;

/// The arguments of one spawn request, as the agent passed them. Defaults (directory,
/// agent, model) are resolved later by the dashboard from the parent row, so an absent
/// value stays `None` here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnArgs {
    /// The child's one-time initial Message.
    pub message: String,
    pub title: Option<String>,
    pub name: Option<String>,
    pub dir: Option<PathBuf>,
    pub agent: Option<Engine>,
    pub model: Option<String>,
}

impl SpawnArgs {
    /// Trim every string; empty strings become None (message stays, trimmed); dir kept as given.
    pub fn normalized(&self) -> SpawnArgs {
        SpawnArgs {
            message: self.message.trim().to_string(),
            title: trimmed(self.title.as_deref()),
            name: trimmed(self.name.as_deref()),
            dir: self.dir.clone(),
            agent: self.agent,
            model: trimmed(self.model.as_deref()),
        }
    }

    /// Lowercase hex sha256 of `serde_json::to_vec(&self.normalized())` (struct field order is stable).
    ///
    /// Serialization fails only for a directory that is not valid UTF-8, which no request file
    /// can carry (JSON is UTF-8); such args hash their lossy form so this never panics.
    pub fn args_hash(&self) -> String {
        let normalized = self.normalized();
        let bytes = match serde_json::to_vec(&normalized) {
            Ok(bytes) => bytes,
            Err(_) => {
                let lossy = SpawnArgs {
                    dir: normalized
                        .dir
                        .as_deref()
                        .map(|dir| PathBuf::from(dir.to_string_lossy().into_owned())),
                    ..normalized
                };
                serde_json::to_vec(&lossy).unwrap_or_default()
            }
        };
        format!("{:x}", Sha256::digest(bytes))
    }
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// One published request. The file is written once by the agent and never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub schema_version: u32,
    /// A lowercase UUID; also the file stem.
    pub request_id: String,
    /// The session whose state directory holds the request.
    pub parent_session: String,
    pub created_at: Epoch,
    pub args: SpawnArgs,
}
