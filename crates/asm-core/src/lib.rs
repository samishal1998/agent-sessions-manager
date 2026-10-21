//! asm-core: domain model, agent adapters, and operations for the
//! cross-agent session manager. No UI or CLI dependencies live here.
//!
//! This is the library under the `asm` CLI, TUI and web UI, and it can be
//! used on its own. Everything is synchronous. Not yet published, and not
//! yet curated: these are the entry points a caller should start from, and
//! the ones the frontends themselves stand on.
//!
//! - [`ops`]: what to call. List, resolve a reference, rename, archive,
//!   relocate, delete, import, messages, capabilities, doctor. Frontends
//!   never reach past it into an adapter.
//! - [`model`]: the data those return: [`model::Session`] (its JSON is the
//!   one `asm --json` prints), [`model::Project`], [`model::AgentKind`].
//! - [`adapter`]: one reader and writer per agent behind the closed
//!   [`adapter::Adapter`] enum, and the [`adapter::Capabilities`] each
//!   declares. Reach for it to read a store at a path of your own choosing
//!   (`ClaudeAdapter::with_root` and its siblings).
//! - [`index`]: the full-text search index over every transcript.
//! - [`ir`]: the versioned, agent-neutral transcript every adapter exports
//!   and the importer reads (`ops::export_ir`).
//! - [`contract`]: the JSON contract version and the error codes shared by
//!   the CLI and the web API.
//!
//! The rest ([`hub`], [`sync`], [`import`], [`live`], ...) is real and
//! public but moves faster than these.

pub mod adapter;
pub mod archive;
pub mod bulk;
pub mod contract;
pub mod error;
pub mod fmt;
pub mod fsutil;
pub mod git;
pub mod hub;
pub mod import;
pub mod index;
pub mod ir;
pub mod live;
pub mod model;
pub mod ops;
pub mod paths;
pub mod process;
pub mod sync;

pub use error::CoreError;
