-- Per-session workspace isolation.
-- Each session can optionally bind to a specific workspace directory.
-- When NULL, the session uses its isolated sandbox directory (data/sandboxes/<uuid>/work/).
-- When set, shell commands and file tools operate in the specified directory.

ALTER TABLE sessions ADD COLUMN workspace_dir TEXT;
