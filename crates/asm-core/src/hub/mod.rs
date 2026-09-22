//! Moving sessions between machines through a hub.
//!
//! One machine runs `asm hub serve`; the others `asm join` it with a token
//! it prints, and `asm push` / `asm pull` sessions through it. The hub is an
//! archive, not a peer: it stores what machines upload and never runs an
//! agent or writes into an agent's store, so a headless server can be one.
//!
//! What crosses is each session's **native** files under its **original
//! id** — the import engine cannot do this job, because it always derives a
//! new id. Where two machines hold the same session, the rule is:
//!
//! - a session's identity is `(agent, native id)`; its path only decides
//!   where a pull lands;
//! - every push names the hub revision it was based on, and the hub refuses
//!   one that is not the current head — so two machines that both continued
//!   a session are told, never silently merged;
//! - timestamps are shown, never used to pick a winner.

pub mod actions;
pub mod bundle;
pub mod client;
pub mod manifest;
pub mod state;
pub mod store;
