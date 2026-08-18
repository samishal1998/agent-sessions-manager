//! asm-core: domain model, agent adapters, and operations for the
//! cross-agent session manager. No UI or CLI dependencies live here.

pub mod adapter;
pub mod archive;
pub mod error;
pub mod fmt;
pub mod fsutil;
pub mod git;
pub mod import;
pub mod index;
pub mod ir;
pub mod model;
pub mod ops;
pub mod paths;
pub mod sync;

pub use error::CoreError;
