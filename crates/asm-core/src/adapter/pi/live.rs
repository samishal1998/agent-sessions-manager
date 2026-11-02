//! Sending to a pi or omp session.
//!
//! Not wired up: both have a JSON print mode, but no stream from either has
//! been recorded to normalize against, and a normalizer written from a flag
//! name is a guess, not support.

use crate::live::{AgentLive, LiveEvent};
use crate::model::Session;

use super::PiAdapter;

impl AgentLive for PiAdapter {
    fn send_command(&self, _session: &Session, _message: &str) -> Option<std::process::Command> {
        None
    }

    fn parse_event(&self, line: &str) -> Vec<LiveEvent> {
        vec![LiveEvent::Raw { line: line.to_string() }]
    }
}
