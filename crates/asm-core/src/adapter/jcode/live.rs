//! jcode's `run --ndjson` stream.
//!
//! Not yet wired up. jcode keeps its credentials somewhere asm's isolated
//! test home does not reach, so no real stream from it has been captured —
//! and the rule this module follows everywhere else is that a normalizer
//! written from a flag name is a guess, not support. `send_message` stays
//! off for jcode until one run can be recorded and matched.

use crate::live::{AgentLive, LiveEvent};
use crate::model::Session;

use super::JCodeAdapter;

impl AgentLive for JCodeAdapter {
    fn send_command(&self, _session: &Session, _message: &str) -> Option<std::process::Command> {
        None
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        vec![LiveEvent::Raw { line: line.to_string() }]
    }
}
