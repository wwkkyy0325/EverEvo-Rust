-- Durable rolling conversation summary (context management, spec D3).
--
-- context_summary:   rolling summary of the conversation, maintained
--                    incrementally in the background (Layer-1) and folded by
--                    the one-shot autocompact (Layer-2). Persists across
--                    requests/restarts — messages are rebuilt from the DB per
--                    request, so an in-memory summary would not survive.
-- summary_watermark: the message id (rows.messages.id) of the newest message
--                    already covered by context_summary. Only messages newer
--                    than this watermark are re-summarized; the old summary is
--                    kept verbatim as a prefix (never re-summarized).
--                    NULL = no summary yet.

ALTER TABLE sessions ADD COLUMN context_summary TEXT;
ALTER TABLE sessions ADD COLUMN summary_watermark TEXT;
