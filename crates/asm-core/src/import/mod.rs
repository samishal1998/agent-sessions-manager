//! The cross-agent import engine: source session → IR → target agent.
//!
//! Two modes, never silently switched:
//! - `Full`: translate the transcript into the target's native records so
//!   the session resumes in place. Only as faithful as the two formats
//!   allow — a loss report always says exactly what was dropped.
//! - `Seed`: distill the session into a narrative handoff document that
//!   becomes the first user message of a fresh target session. Survives
//!   any format churn; loses resume-in-place granularity.
//!
//! Idempotency: target ids are derived deterministically from source ids,
//!   so re-importing the same session is detected ("in sync"), never
//!   duplicated.

pub mod engine;
pub mod idempotency;
pub mod loss;
pub mod seed;
pub mod tested_versions;
pub mod toolmap;

pub use engine::{ImportMode, ImportOpts, ImportOutcome, import};
pub use loss::LossReport;
