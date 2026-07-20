-- Add content_hash column for message integrity verification.
-- Part of the immutable raw log (Tier-2) design.
-- Existing messages get a placeholder hash — they can be verified
-- against current content manually if needed.

ALTER TABLE messages ADD COLUMN content_hash TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000';

-- FTS5 full-text search index for cross-session memory search.
-- Used by the memory_search tool for keyword-based recall.
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    content_rowid='rowid',
    tokenize='porter unicode61'
);

-- Populate FTS from existing messages
INSERT INTO messages_fts(rowid, content)
    SELECT rowid, content FROM messages WHERE content != '';

-- Triggers to keep FTS in sync with messages
CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
END;
