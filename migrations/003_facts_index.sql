-- Facts table — SQLite index for fast retrieval of long-term memories.
-- MD files remain the human-readable source of truth;
-- this table provides sub-millisecond FTS5 search.
-- Design reference: OpenDB (93.6% LongMemEval, 0.5ms median retrieval).

CREATE TABLE IF NOT EXISTS facts (
    id          TEXT PRIMARY KEY,       -- kebab-case slug (matches MD filename)
    description TEXT NOT NULL,          -- one-line summary
    content     TEXT NOT NULL,          -- full markdown body
    fact_type   TEXT NOT NULL DEFAULT 'project',  -- user|feedback|project|reference
    retrieval_count INTEGER NOT NULL DEFAULT 0,   -- times retrieved (for scoring)
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- FTS5 index for sub-millisecond full-text search
-- Handles keyword, temporal, and multi-word queries
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    description,
    content,
    content_rowid='rowid',
    tokenize='porter unicode61'
);

-- Triggers to keep FTS5 in sync
CREATE TRIGGER IF NOT EXISTS facts_fts_insert AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, description, content)
        VALUES (new.rowid, new.description, new.content);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_delete AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, description, content)
        VALUES ('delete', old.rowid, old.description, old.content);
END;

CREATE TRIGGER IF NOT EXISTS facts_fts_update AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, description, content)
        VALUES ('delete', old.rowid, old.description, old.content);
    INSERT INTO facts_fts(rowid, description, content)
        VALUES (new.rowid, new.description, new.content);
END;

-- Index for type-based filtering + recency sorting
CREATE INDEX IF NOT EXISTS idx_facts_type ON facts(fact_type);
CREATE INDEX IF NOT EXISTS idx_facts_updated ON facts(updated_at DESC);
