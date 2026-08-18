//! The loss report: every import states exactly what did not survive.

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct LossReport {
    pub messages_in: usize,
    pub messages_out: usize,
    /// Opaque (signed/encrypted) reasoning blocks that could not cross —
    /// provider-bound by construction.
    pub reasoning_dropped: usize,
    pub tools_mapped: usize,
    /// Tool names kept verbatim because the target has no equivalent
    /// (inert-but-readable history), with occurrence counts.
    pub tools_kept_unmapped: BTreeMap<String, usize>,
    /// Subagent transcripts that could not nest in the target format.
    pub agent_transcripts_dropped: usize,
    /// Source-specific record classes left behind (state records, file
    /// history, snapshots, ...).
    pub extension_classes_dropped: Vec<String>,
    pub notes: Vec<String>,
}

impl LossReport {
    pub fn human_summary(&self) -> String {
        let mut lines = vec![format!(
            "{} of {} messages converted",
            self.messages_out, self.messages_in
        )];
        if self.reasoning_dropped > 0 {
            lines.push(format!(
                "{} opaque reasoning blocks dropped (provider-bound; summaries only)",
                self.reasoning_dropped
            ));
        }
        if self.tools_mapped > 0 {
            lines.push(format!("{} tool calls mapped to target tool names", self.tools_mapped));
        }
        if !self.tools_kept_unmapped.is_empty() {
            let names: Vec<String> = self
                .tools_kept_unmapped
                .iter()
                .map(|(name, count)| format!("{name} ×{count}"))
                .collect();
            lines.push(format!("unmapped tool names kept verbatim: {}", names.join(", ")));
        }
        if self.agent_transcripts_dropped > 0 {
            lines.push(format!(
                "{} subagent transcripts not carried (no equivalent in target)",
                self.agent_transcripts_dropped
            ));
        }
        if !self.extension_classes_dropped.is_empty() {
            lines.push(format!(
                "source-only record classes left behind: {}",
                self.extension_classes_dropped.join(", ")
            ));
        }
        lines.extend(self.notes.iter().cloned());
        lines.join("\n")
    }
}
