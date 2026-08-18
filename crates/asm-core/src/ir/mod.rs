//! The Session IR: a versioned, documented, agent-neutral representation of
//! a session. It is what `asm export` emits, what the archive stores, and
//! what cross-agent import converts through.
//!
//! Fidelity contract: everything the IR models converts cleanly; everything
//! it does not model rides in per-agent `extensions` bags so an A→B→A
//! round-trip can restore it. Opaque provider payloads (signed/encrypted
//! reasoning) are summarized, never carried — they are provider-bound by
//! construction.

mod v1;

pub use v1::*;
