use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("agent store not found at {path}")]
    StoreNotFound { path: PathBuf },

    #[error("SQLite error in {db}: {source}")]
    Sqlite {
        db: PathBuf,
        #[source]
        source: Box<rusqlite::Error>,
    },

    #[error("session {id} is live (pid {pid:?}); refusing to modify it")]
    SessionLive { id: String, pid: Option<u32> },

    #[error("the {agent} store is busy ({detail}); close that instance and retry")]
    StoreBusy { agent: &'static str, detail: String },

    #[error("{agent} does not support '{op}'")]
    NotSupported { agent: &'static str, op: &'static str },

    #[error("destination already exists at {path}; refusing to overwrite")]
    DestinationExists { path: PathBuf },

    #[error("session {id} is not in the archive")]
    NotArchived { id: String },

    #[error("{msg}")]
    Invalid { msg: String },
}

impl CoreError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CoreError::Io { path: path.into(), source }
    }
}
