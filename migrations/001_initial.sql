-- EverEvo initial schema
-- SQLite (Phase 1), PostgreSQL-compatible (future)

-- Sessions table
CREATE TABLE IF NOT EXISTS sessions (
    id          BLOB PRIMARY KEY,        -- UUID stored as 16 bytes (SQLite) / UUID (Postgres)
    title       TEXT NOT NULL DEFAULT 'New Session',
    created_at  TEXT NOT NULL,           -- ISO 8601 timestamp
    updated_at  TEXT NOT NULL,
    metadata    TEXT NOT NULL DEFAULT '{}'  -- JSON
);

-- Messages table
CREATE TABLE IF NOT EXISTS messages (
    id           BLOB PRIMARY KEY,
    session_id   BLOB NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('system', 'user', 'assistant', 'tool')),
    content      TEXT NOT NULL DEFAULT '',
    tool_calls   TEXT,                     -- JSON array of ToolCall, nullable
    tool_call_id TEXT,                     -- For 'tool' role messages, the call they respond to
    created_at   TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_messages_created ON messages(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);
