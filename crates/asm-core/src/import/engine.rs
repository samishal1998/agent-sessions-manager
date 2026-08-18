//! Orchestrates one import: source session → IR → (mode) → target adapter.

use std::path::PathBuf;

use serde::Serialize;

use crate::CoreError;
use crate::adapter::{Adapter, AgentRead, AgentWrite};
use crate::ir::{ExtBag, IrMessage, IrPart, IrRole, IrSession};
use crate::model::{AgentKind, Session, SessionRef};

use super::loss::LossReport;
use super::{seed, tested_versions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    Full,
    Seed,
}

#[derive(Debug, Clone)]
pub struct ImportOpts {
    pub mode: ImportMode,
    /// Target project directory. Defaults to the source session's project.
    pub project: Option<PathBuf>,
    /// Generate everything but write nothing.
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct ImportOutcome {
    pub mode: ImportMode,
    /// The created (or pre-existing) target session.
    pub target: Option<SessionRef>,
    /// True when the deterministic target id already existed — the session
    /// was imported before; nothing was written.
    pub in_sync: bool,
    /// Dry runs: the generated native document instead of a write.
    pub dry_run_artifact: Option<String>,
    /// How to open the imported session in its new agent.
    pub resume_hint: Option<String>,
    pub warnings: Vec<String>,
    pub loss: LossReport,
}

pub fn import(
    source: &Session,
    target: AgentKind,
    opts: &ImportOpts,
) -> Result<ImportOutcome, CoreError> {
    if source.handle.agent == target {
        return Err(CoreError::Invalid {
            msg: "session is already in that agent; use `asm move` to change its project".into(),
        });
    }
    let adapters = Adapter::available();
    let source_adapter = adapters
        .iter()
        .find(|a| a.kind() == source.handle.agent)
        .ok_or(CoreError::NotSupported { agent: "source", op: "export" })?;
    let target_adapter = adapters
        .iter()
        .find(|a| a.kind() == target)
        .ok_or(CoreError::NotSupported { agent: "target", op: "import" })?;

    let mut ir = source_adapter.export_ir(source)?;
    if let Some(project) = &opts.project {
        let project = project.canonicalize().map_err(|e| CoreError::io(project, e))?;
        ir.project_path = crate::ir::PortablePath::from_path(&project);
    }

    let mut warnings = Vec::new();
    if let Some(warning) = tested_versions::drift_warning(target) {
        warnings.push(warning);
    }

    // Seed mode replaces the conversation with a one-message narrative, so
    // the target adapter can only measure the wrapper. Take the census off
    // the ORIGINAL session first, or the loss report claims full fidelity
    // for the mode that loses the most.
    let census = Census::of(&ir);

    let ir = match opts.mode {
        ImportMode::Full => ir,
        ImportMode::Seed => seed_wrap(&ir),
    };

    let mut outcome = target_adapter.import_ir(&ir, opts)?;
    outcome.mode = opts.mode;
    outcome.warnings.extend(warnings);
    if opts.mode == ImportMode::Seed && !outcome.in_sync {
        census.apply_to(&mut outcome.loss);
    }
    Ok(outcome)
}

/// What the source session contained, measured before any mode rewrites it.
struct Census {
    messages: usize,
    opaque_reasoning: usize,
    tool_calls: usize,
    agent_transcripts: usize,
    extension_classes: Vec<String>,
}

impl Census {
    fn of(ir: &IrSession) -> Census {
        let mut census = Census {
            messages: ir.messages.len(),
            opaque_reasoning: 0,
            tool_calls: 0,
            agent_transcripts: 0,
            extension_classes: Vec::new(),
        };
        for message in &ir.messages {
            for part in &message.parts {
                match part {
                    IrPart::Reasoning { opaque: true, .. } => census.opaque_reasoning += 1,
                    IrPart::ToolCall { .. } => census.tool_calls += 1,
                    IrPart::Agent { .. } => census.agent_transcripts += 1,
                    _ => {}
                }
            }
        }
        if let Some(ext) = ir.extensions.get("claude-code")
            && let Some(counts) = ext.get("skipped_record_counts").and_then(|v| v.as_object())
        {
            census.extension_classes = counts.keys().cloned().collect();
        }
        census
    }

    fn apply_to(&self, loss: &mut LossReport) {
        loss.messages_in = self.messages;
        loss.reasoning_dropped = self.opaque_reasoning;
        loss.agent_transcripts_dropped = self.agent_transcripts;
        loss.extension_classes_dropped = self.extension_classes.clone();
        loss.notes.retain(|note| !note.starts_with("a subagent run"));
        loss.notes.push(format!(
            "seed mode: {} messages condensed into one handoff document; {} tool calls are \
             described in prose rather than replayable",
            self.messages, self.tool_calls
        ));
    }
}

/// Wrap the whole session into a single-user-message IR carrying the seed
/// narrative — the target adapter then imports it like any tiny session.
fn seed_wrap(ir: &IrSession) -> IrSession {
    let document = seed::seed_document(ir);
    IrSession {
        ir_version: ir.ir_version,
        source: ir.source.clone(),
        title: ir.title.clone().map(|t| format!("{t} (handoff)")),
        slug: ir.slug.clone(),
        project_path: ir.project_path.clone(),
        created: ir.created,
        updated: ir.updated,
        model: ir.model.clone(),
        usage: crate::model::Usage::default(),
        messages: vec![IrMessage {
            role: IrRole::User,
            timestamp: ir.updated.or(ir.created),
            parts: vec![IrPart::Text { text: document }],
            source_id: Some("seed".to_string()),
            extensions: ExtBag::new(),
        }],
        extensions: ExtBag::new(),
    }
}
