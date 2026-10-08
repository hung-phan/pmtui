mod consult;
mod pane_text;
mod preflight;
mod prompt;
mod reply;

use super::Consult;

const NONCE: &str = "n0nce-abc123";

fn consult_with_options() -> Consult {
    Consult {
        nonce: NONCE.to_string(),
        goal: "Keep the docs build green.".to_string(),
        question: "Which formatter should I use for the changelog?".to_string(),
        options: vec!["prettier".to_string(), "dprint".to_string()],
        reported_effect: None,
        situation: String::new(),
        directive: String::new(),
    }
}

fn consult_free() -> Consult {
    Consult {
        nonce: NONCE.to_string(),
        goal: "Keep the docs build green.".to_string(),
        question: "What wording should the new section header use?".to_string(),
        options: vec![],
        reported_effect: None,
        situation: String::new(),
        directive: String::new(),
    }
}

/// The `claude --output-format json` envelope as it lands in the combined log.
fn envelope(inner: &str) -> String {
    let escaped = serde_json::to_string(inner).expect("test reply serializes");
    format!(
        "{{\"is_error\":false,\"subtype\":\"success\",\"type\":\"result\",\
         \"result\":{escaped}}}"
    )
}
