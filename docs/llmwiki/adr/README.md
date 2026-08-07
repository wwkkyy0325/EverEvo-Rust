# Architecture Decision Records

| # | Title | Status | Date |
|---|-------|--------|------|
| [0001](0001-unified-error-handling.md) | Adopt ApiError for consistent HTTP error responses | Accepted | 2026-08-06 |
| [0002](0002-session-coordinator.md) | Centralize per-session data-flow via SessionCoordinator | Accepted | 2026-08-05 |
| [0003](0003-catch-unwind-boundaries.md) | Add catch_unwind at agent-loop and chat-handler spawn sites | Accepted | 2026-08-06 |

## Format

Each ADR follows Michael Nygard's format:
- **Status** — Proposed / Accepted / Deprecated / Superseded
- **Context** — the forces at play
- **Decision** — what was chosen and why
- **Consequences** — what becomes easier and harder
- **Alternatives Considered** — options evaluated and rejected

## Process

1. Copy `TEMPLATE.md` to `NNNN-kebab-case.md`
2. Fill in all sections, status = Proposed
3. Open a PR for discussion
4. After approval, change status to Accepted and merge

ADRs are immutable once accepted. If a decision changes, create a new ADR that supersedes the old one.
