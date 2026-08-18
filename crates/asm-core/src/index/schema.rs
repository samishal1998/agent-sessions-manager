//! The index schema. Disposable by design: `DROP` then `CREATE` is the
//! migration strategy, because everything here is derived data that can be
//! rebuilt from the agents' own stores.

pub const DROP: &str = "
DROP TABLE IF EXISTS message_fts;
DROP TABLE IF EXISTS indexed_session;
DROP TABLE IF EXISTS meta;
";

pub const CREATE: &str = "
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- One row per session that has been indexed, carrying the fingerprint that
-- decides whether it needs re-extracting.
CREATE TABLE indexed_session (
    agent         TEXT NOT NULL,
    native_id     TEXT NOT NULL,
    title         TEXT,
    project_root  TEXT,
    updated_ms    INTEGER,
    -- 'idle' | 'live' | 'archived'; archived sessions are searchable but
    -- are not in `asm list`, so results can say why.
    status        TEXT,
    -- Opaque per-session content fingerprint; compared verbatim to decide
    -- whether the session needs re-extracting.
    fingerprint   TEXT,
    message_count INTEGER NOT NULL DEFAULT 0,
    indexed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (agent, native_id)
);

CREATE INDEX indexed_session_project ON indexed_session (project_root);

-- Standalone FTS5: it stores its own copy of the text, which is what lets
-- snippet() work and lets rows be deleted per session.
CREATE VIRTUAL TABLE message_fts USING fts5(
    text,
    agent      UNINDEXED,
    native_id  UNINDEXED,
    role       UNINDEXED,
    seq        UNINDEXED,
    ts         UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
";
