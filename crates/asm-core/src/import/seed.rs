//! Seed mode: distill a session into a narrative handoff document that
//! becomes the first user message of a fresh target session. The receiving
//! model doesn't need to replay actions — it needs to understand them.

use crate::ir::{IrMessage, IrPart, IrRole, IrSession};

/// Character budget for the whole narrative. When over budget, middle turns
/// are elided (head + tail kept) and the elision is stated in the document.
const BUDGET: usize = 30_000;
const PER_TEXT: usize = 2_000;

pub fn seed_document(ir: &IrSession) -> String {
    let mut turns: Vec<String> = Vec::new();
    for message in &ir.messages {
        if let Some(turn) = render_turn(message) {
            turns.push(turn);
        }
    }

    let header = format!(
        "# Imported session handoff\n\n\
         This conversation was imported from {agent} (session {id}) by asm. \
         The transcript below is a condensed narrative of what happened — tool \
         actions are described, not replayable. Continue the work from where it \
         left off.\n\n\
         - Title: {title}\n- Project: {project}\n\n## Conversation\n",
        agent = ir.source.agent,
        id = ir.source.native_id,
        title = ir.title.as_deref().unwrap_or("(untitled)"),
        project = ir.project_path.0,
    );

    let total: usize = turns.iter().map(String::len).sum();
    let body = if total <= BUDGET {
        turns.join("\n")
    } else {
        // Keep the first quarter and last half of the budget's worth.
        let mut head = Vec::new();
        let mut head_len = 0;
        for turn in &turns {
            if head_len + turn.len() > BUDGET / 4 {
                break;
            }
            head_len += turn.len();
            head.push(turn.clone());
        }
        let mut tail = Vec::new();
        let mut tail_len = 0;
        for turn in turns.iter().rev() {
            if tail_len + turn.len() > BUDGET / 2 {
                break;
            }
            tail_len += turn.len();
            tail.push(turn.clone());
        }
        tail.reverse();
        let elided = turns.len() - head.len() - tail.len();
        format!(
            "{}\n\n[... {elided} turns elided for length ...]\n\n{}",
            head.join("\n"),
            tail.join("\n")
        )
    };
    format!("{header}{body}")
}

fn render_turn(message: &IrMessage) -> Option<String> {
    let speaker = match message.role {
        IrRole::User => "User",
        IrRole::Assistant => "Assistant",
        IrRole::System => "System",
    };
    let mut chunks: Vec<String> = Vec::new();
    for part in &message.parts {
        match part {
            IrPart::Text { text } => chunks.push(clip(text, PER_TEXT)),
            IrPart::Reasoning { .. } => {} // internal; not part of the handoff
            IrPart::ToolCall { name, input, .. } => {
                chunks.push(format!("→ {name}: {}", clip(&describe_input(input), 200)));
            }
            IrPart::ToolResult { output, is_error, .. } => {
                let status = if *is_error { "error" } else { "ok" };
                let lines = output.lines().count();
                chunks.push(format!("  ↳ [{status}, {lines} lines]"));
            }
            IrPart::File { path, .. } => {
                if let Some(path) = path {
                    chunks.push(format!("→ attached file {}", path.0));
                }
            }
            IrPart::Agent { name, description, transcript } => {
                chunks.push(format!(
                    "→ subagent '{name}' ({}) ran, {} turns",
                    description.as_deref().unwrap_or("no description"),
                    transcript.len()
                ));
            }
            IrPart::Unknown => {}
        }
    }
    if chunks.is_empty() {
        return None;
    }
    Some(format!("**{speaker}:** {}\n", chunks.join("\n")))
}

fn describe_input(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(map) => {
            // The most informative single field, else compact JSON.
            for key in ["command", "file_path", "path", "pattern", "query", "url", "description"] {
                if let Some(v) = map.get(key).and_then(serde_json::Value::as_str) {
                    return v.to_string();
                }
            }
            serde_json::Value::Object(map.clone()).to_string()
        }
        other => other.to_string(),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}
