//! The machine interface's two shared pieces: the JSON contract version and
//! the error vocabulary. The CLI's `--json` and the web API both speak them,
//! so they live here, once.
//!
//! The documented promise is in `reference/json` on the docs site; the short
//! form is that within one `JSON_CONTRACT_VERSION` a shape only ever gains
//! fields.

use serde::Serialize;

use crate::CoreError;
use crate::hub::store::HubError;
use crate::ops::ResolveError;

/// Version of the shapes `--json` and the web API emit. Bumped only when an
/// existing field is removed, renamed or changes meaning, or an existing
/// command's output changes kind (an array becoming an object). Adding a
/// field, a command, a flag or a route never bumps it.
pub const JSON_CONTRACT_VERSION: u32 = 1;

/// Why a command or request failed, as a word a program can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Nothing matches the reference, or the store is not there.
    NotFound,
    /// The reference matches several sessions; `candidates` lists them.
    Ambiguous,
    /// The session is in use by a running agent; nothing was changed.
    LiveSession,
    /// The agent (or its installed version) cannot do this.
    Unsupported,
    /// Reading or writing a file or database failed.
    Io,
    /// The arguments or the request are wrong.
    Usage,
    /// The change would overwrite or contradict what is there.
    Conflict,
    /// A hub or peer did not answer.
    Unreachable,
    /// A credential is missing or wrong: the web API's bearer token, a
    /// hub's machine credential.
    Unauthorized,
    /// Anything else: a bug, or something asm did not expect.
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::Ambiguous => "ambiguous",
            ErrorCode::LiveSession => "live_session",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Io => "io",
            ErrorCode::Usage => "usage",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Unreachable => "unreachable",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Internal => "internal",
        }
    }

    /// The process exit status. 0 is success, 1 is `internal` and 2 is
    /// `usage` (clap's own), so what a shell already tested keeps meaning
    /// what it did.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Internal => 1,
            ErrorCode::Usage => 2,
            ErrorCode::NotFound => 10,
            ErrorCode::Ambiguous => 11,
            ErrorCode::LiveSession => 12,
            ErrorCode::Unsupported => 13,
            ErrorCode::Conflict => 14,
            ErrorCode::Unreachable => 15,
            ErrorCode::Io => 16,
            ErrorCode::Unauthorized => 17,
        }
    }

    /// The HTTP status the web API answers with.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::NotFound => 404,
            ErrorCode::Ambiguous | ErrorCode::Conflict => 409,
            ErrorCode::LiveSession => 423,
            ErrorCode::Unsupported => 501,
            ErrorCode::Usage => 400,
            ErrorCode::Unauthorized => 401,
            ErrorCode::Unreachable => 502,
            ErrorCode::Io | ErrorCode::Internal => 500,
        }
    }
}

/// The `error` object of a failure: `{"error": {"code", "message",
/// "candidates"?}}` once wrapped by [`ErrorBody::envelope`].
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    /// Only for `ambiguous`: the `agent:id` of each session the reference
    /// matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ErrorBody { code, message: message.into(), candidates: None }
    }

    pub fn envelope(&self) -> serde_json::Value {
        serde_json::json!({ "error": self })
    }
}

/// An error that already knows its code, for failures that have no typed
/// error of their own (a bad flag combination, a missing confirmation).
#[derive(Debug)]
pub struct CodedError {
    pub code: ErrorCode,
    pub message: String,
}

impl CodedError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        CodedError { code, message: message.into() }
    }
}

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodedError {}

fn core_code(e: &CoreError) -> ErrorCode {
    match e {
        CoreError::Io { .. } | CoreError::Sqlite { .. } => ErrorCode::Io,
        CoreError::StoreNotFound { .. } | CoreError::NotArchived { .. } => ErrorCode::NotFound,
        CoreError::SessionLive { .. } | CoreError::StoreBusy { .. } => ErrorCode::LiveSession,
        CoreError::NotSupported { .. } | CoreError::Unsupported { .. } => ErrorCode::Unsupported,
        CoreError::DestinationExists { .. } => ErrorCode::Conflict,
        CoreError::Unreachable { .. } => ErrorCode::Unreachable,
        // The grab bag of "this request cannot be done as asked".
        CoreError::Invalid { .. } => ErrorCode::Usage,
    }
}

/// The error body for `error`, looking through its `source` chain (an
/// `anyhow` context keeps what it wraps there) for the first error asm
/// types. Anything untyped is `internal`, with `error`'s own text. `message`
/// is always the outermost text, so a context a command added is kept.
pub fn classify(error: &(dyn std::error::Error + 'static)) -> ErrorBody {
    let message = error.to_string();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = cause {
        if let Some(e) = e.downcast_ref::<ResolveError>() {
            return match e {
                ResolveError::NotFound { .. } => ErrorBody::new(ErrorCode::NotFound, message),
                ResolveError::Ambiguous { candidates, .. } => {
                    ErrorBody { candidates: Some(candidates.clone()), ..ErrorBody::new(ErrorCode::Ambiguous, message) }
                }
                ResolveError::Core(e) => ErrorBody::new(core_code(e), message),
            };
        }
        if let Some(e) = e.downcast_ref::<CoreError>() {
            return ErrorBody::new(core_code(e), message);
        }
        if let Some(e) = e.downcast_ref::<HubError>() {
            return ErrorBody::new(
                match e {
                    HubError::NotFound => ErrorCode::NotFound,
                    HubError::Unauthorized => ErrorCode::Unauthorized,
                    HubError::BadRequest(_) | HubError::MissingBlobs(_) | HubError::ShaMismatch => ErrorCode::Usage,
                    HubError::Busy(_) | HubError::Conflict { .. } | HubError::TooLarge { .. } => ErrorCode::Conflict,
                    HubError::Core(e) => core_code(e),
                },
                message,
            );
        }
        if let Some(e) = e.downcast_ref::<CodedError>() {
            return ErrorBody::new(e.code, message);
        }
        if let Some(e) = e.downcast_ref::<std::io::Error>() {
            let code = if e.kind() == std::io::ErrorKind::NotFound { ErrorCode::NotFound } else { ErrorCode::Io };
            return ErrorBody::new(code, message);
        }
        cause = e.source();
    }
    ErrorBody::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_and_statuses_are_distinct_and_pinned() {
        let all = [
            (ErrorCode::Internal, 1, 500),
            (ErrorCode::Usage, 2, 400),
            (ErrorCode::NotFound, 10, 404),
            (ErrorCode::Ambiguous, 11, 409),
            (ErrorCode::LiveSession, 12, 423),
            (ErrorCode::Unsupported, 13, 501),
            (ErrorCode::Conflict, 14, 409),
            (ErrorCode::Unreachable, 15, 502),
            (ErrorCode::Io, 16, 500),
            (ErrorCode::Unauthorized, 17, 401),
        ];
        for (code, exit, http) in all {
            assert_eq!((code.exit_code(), code.http_status()), (exit, http), "{}", code.as_str());
        }
        let mut exits: Vec<i32> = all.iter().map(|a| a.1).collect();
        exits.sort();
        exits.dedup();
        assert_eq!(exits.len(), all.len(), "no two codes share an exit status");
    }

    #[test]
    fn each_core_error_has_the_code_its_name_says() {
        let p = std::path::PathBuf::from("/x");
        let io = || std::io::Error::other("boom");
        let cases: Vec<(CoreError, ErrorCode)> = vec![
            (CoreError::io(&p, io()), ErrorCode::Io),
            (CoreError::StoreNotFound { path: p.clone() }, ErrorCode::NotFound),
            (CoreError::NotArchived { id: "a".into() }, ErrorCode::NotFound),
            (CoreError::SessionLive { id: "a".into(), pid: None }, ErrorCode::LiveSession),
            (CoreError::StoreBusy { agent: "opencode", detail: "d".into() }, ErrorCode::LiveSession),
            (CoreError::NotSupported { agent: "codex", op: "rename" }, ErrorCode::Unsupported),
            (CoreError::Unsupported { msg: "m".into() }, ErrorCode::Unsupported),
            (CoreError::DestinationExists { path: p }, ErrorCode::Conflict),
            (CoreError::Unreachable { msg: "m".into() }, ErrorCode::Unreachable),
            (CoreError::Invalid { msg: "m".into() }, ErrorCode::Usage),
        ];
        for (error, code) in cases {
            assert_eq!(classify(&error).code, code, "{error}");
        }
    }

    #[test]
    fn ambiguity_keeps_its_candidates_and_the_envelope_is_one_object() {
        let e = ResolveError::Ambiguous { query: "ab".into(), candidates: vec!["codex:abc".into(), "codex:abd".into()] };
        let body = classify(&e);
        assert_eq!(body.code, ErrorCode::Ambiguous);
        assert_eq!(
            body.envelope(),
            serde_json::json!({"error": {"code": "ambiguous", "message": e.to_string(), "candidates": ["codex:abc", "codex:abd"]}})
        );
        let plain = ErrorBody::new(ErrorCode::NotFound, "gone").envelope();
        assert!(plain["error"].get("candidates").is_none(), "only an ambiguity has candidates");
    }

    #[test]
    fn a_resolve_failure_inside_a_core_error_chain_is_still_found() {
        let wrapped = ResolveError::from(CoreError::SessionLive { id: "x".into(), pid: Some(1) });
        assert_eq!(classify(&wrapped).code, ErrorCode::LiveSession);
        assert_eq!(classify(&CodedError::new(ErrorCode::Usage, "bad")).code, ErrorCode::Usage);
        assert_eq!(classify(&std::io::Error::other("x")).code, ErrorCode::Io);
        assert_eq!(classify(&std::fmt::Error).code, ErrorCode::Internal);
    }
}
