//! Every shipped skill is written in Simplified Technical English (ASD-STE100).
//!
//! The standard's load-bearing rule is the one worth enforcing by machine: a procedural sentence
//! carries ONE instruction and stays short. Prose that drifts back to 40-word sentences with
//! stacked clauses is what made these files read as jargon, and the drift is invisible in review
//! because each individual edit looks reasonable.
//!
//! Two mechanical rules, chosen because neither needs judgement:
//!
//! - no prose sentence is longer than `MAX_WORDS`; and
//! - no em dash in prose, because the aside is how a long sentence evades a word limit. A short
//!   sentence plus a second short sentence says the same thing and survives translation.
//!
//! SHORTCUT: the scanner skips every line indented four spaces or more, which is how the schema
//! blocks and the deepest sub-bullets are excluded without parsing Markdown. So it checks the bulk
//! of the prose rather than all of it. Upgrade it only if a long sentence ever hides there.

use super::{
    COORDINATOR_SKILL_MD, DECIDER_SKILL_MD, MESSAGE_SKILL_MD, SPAWN_SKILL_MD, WORKER_SKILL_MD,
};

/// ASD-STE100 allows 20 words in a procedural sentence and 25 in a descriptive one. These files
/// are procedures, so they take the lower bound.
const MAX_WORDS: usize = 20;

/// The project-local dev skill is not embedded in the binary, so it is read at compile time here
/// rather than through `src/skills.rs`. It holds an agent to the same standard as the shipped five.
const UI_TESTING_SKILL_MD: &str = include_str!("../../../.agents/skills/pmtui-ui-testing/SKILL.md");

fn shipped() -> [(&'static str, &'static str); 6] {
    [
        ("agent-manager-worker", WORKER_SKILL_MD),
        ("agent-manager-decider", DECIDER_SKILL_MD),
        ("agent-manager-coordinator", COORDINATOR_SKILL_MD),
        ("pmtui-spawn", SPAWN_SKILL_MD),
        ("pmtui-message", MESSAGE_SKILL_MD),
        ("pmtui-ui-testing", UI_TESTING_SKILL_MD),
    ]
}

/// Remove inline code spans, then the emphasis markers. A command, a field name or an example of
/// output is not prose: it is quoted material, and its punctuation is not the author's.
fn strip_code_and_emphasis(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for ch in text.chars() {
        match ch {
            '`' => in_code = !in_code,
            '*' | '_' => {}
            _ if !in_code => out.push(ch),
            _ => {}
        }
    }
    out
}

/// The prose chunks of one skill: a frontmatter description, a paragraph, or one list item, with
/// its wrapped continuation lines joined back into a single string.
fn prose_chunks(md: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    let mut fences_seen = 0usize;

    let push_current = |current: &mut String, chunks: &mut Vec<String>| {
        let text = strip_code_and_emphasis(current).trim().to_string();
        if !text.is_empty() {
            chunks.push(text);
        }
        current.clear();
    };

    for (index, line) in md.lines().enumerate() {
        // The frontmatter fence itself, and the YAML keys inside it: only `description` is prose.
        if line == "---" {
            fences_seen += 1;
            push_current(&mut current, &mut chunks);
            continue;
        }
        if fences_seen == 1 {
            if let Some(description) = line.strip_prefix("description: ") {
                chunks.push(strip_code_and_emphasis(description).trim().to_string());
            }
            continue;
        }
        if index == 0 {
            continue; // nothing before the opening fence
        }
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            push_current(&mut current, &mut chunks);
            continue;
        }
        let skip = in_fence
            || line.starts_with("    ")
            || line.trim().is_empty()
            || line.starts_with('#')
            || line.trim_start().starts_with('|')
            || line.contains("\": ");
        if skip {
            push_current(&mut current, &mut chunks);
            continue;
        }
        // A list item or a numbered step begins a chunk; anything else continues the one open.
        let trimmed = line.trim_start();
        let starts_item = trimmed.starts_with("- ")
            || trimmed
                .split_once(". ")
                .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if starts_item {
            push_current(&mut current, &mut chunks);
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(trimmed.trim_start_matches("- "));
    }
    push_current(&mut current, &mut chunks);
    chunks
}

/// Split a chunk into sentences. A list item without a full stop is one sentence.
fn sentences(chunk: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = chunk.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        let ends = matches!(ch, '.' | '?' | '!' | ':' | ';')
            && chars.peek().is_none_or(|next| next.is_whitespace());
        if ends {
            out.push(current.trim().to_string());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out.retain(|sentence| sentence.chars().any(char::is_alphabetic));
    out
}

#[test]
fn every_shipped_skill_keeps_its_sentences_short() {
    let mut long: Vec<String> = Vec::new();
    for (name, md) in shipped() {
        for chunk in prose_chunks(md) {
            for sentence in sentences(&chunk) {
                let words = sentence.split_whitespace().count();
                if words > MAX_WORDS {
                    long.push(format!("{name}: {words} words: {sentence}"));
                }
            }
        }
    }
    assert!(
        long.is_empty(),
        "ASD-STE100 allows {MAX_WORDS} words in a procedural sentence; these carry more than one \
         instruction and must be split:\n{}",
        long.join("\n")
    );
}

#[test]
fn no_shipped_skill_stacks_clauses_behind_an_em_dash() {
    let mut found: Vec<String> = Vec::new();
    for (name, md) in shipped() {
        for chunk in prose_chunks(md) {
            if chunk.contains('—') {
                found.push(format!("{name}: {chunk}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "an em-dash aside is a second idea in the same sentence; write two sentences:\n{}",
        found.join("\n")
    );
}

/// NON-VACUITY: the scanner must actually read these files. A chunking bug that returned nothing
/// would make both tests above pass forever.
#[test]
fn the_scanner_sees_the_prose_it_checks() {
    for (name, md) in shipped() {
        let chunks = prose_chunks(md);
        assert!(
            chunks.len() > 10,
            "{name}: only {} prose chunks found, so the scanner is not reading the file",
            chunks.len()
        );
        let words: usize = chunks
            .iter()
            .map(|chunk| chunk.split_whitespace().count())
            .sum();
        assert!(words > 200, "{name}: only {words} prose words found");
    }
    // And it must still find a violation when one exists.
    let bad = "---\nname: x\ndescription: d\n---\n\nThis one sentence is far too long for the \
               standard because it keeps adding clause after clause without ever stopping for a \
               full stop anywhere.\n";
    let sentence = &sentences(&prose_chunks(bad)[1])[0];
    assert!(sentence.split_whitespace().count() > MAX_WORDS);
}
