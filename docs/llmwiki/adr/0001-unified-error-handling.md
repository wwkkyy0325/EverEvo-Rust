# ADR-0001: Adopt ApiError for Consistent HTTP Error Responses

- **Status:** Accepted
- **Date:** 2026-08-06

## Context

HTTP error handling used 9 different formats across 14 route files:
- `Json({"error": "msg"})` returning 200 OK
- `(StatusCode, String)` tuples
- `(StatusCode, Json<Value>)` tuples
- Per-file error types (`AppError`, `KgError`)
- `{"ok": false, "error": "msg"}` strings

This made error identification difficult — clients had to parse different JSON shapes depending on which endpoint failed. The A2A protocol layer already had a clean pattern (`A2aError` + `A2aErrorCode` enum), proving the approach worked.

## Decision

Introduce two types in `everevo-core/src/error.rs`:

1. **`ErrorCode` enum** — 18 machine-readable variants mapped to HTTP status codes
2. **`ApiError` struct** — carries `code`, `message`, `details`; implements `IntoResponse`

All REST endpoints return:
```json
{"error": {"code": "NOT_FOUND", "message": "human-readable", "details": null}}
```

This required adding `axum` as a dependency to `everevo-core` (for `IntoResponse` impl — orphan rule requires the impl in the type-defining crate).

## Consequences

**Easier:**
- Single JSON envelope for all REST errors — clients parse one format
- Machine-readable `code` field enables programmatic error handling
- `From<EverEvoError> for ApiError` maps 17 variant-to-code in one place
- Factory constructors (`ApiError::not_found()`, `ApiError::bad_request()`, etc.) are self-documenting
- ~70 lines of duplicate error type definitions deleted

**Harder:**
- Adding new error variants requires touching `ErrorCode` enum (centralized)
- SSE stream paths still use `Result<(), String>` — not unified

## Alternatives Considered

1. **`anyhow`/`eyre`** — Too opaque for HTTP; no machine-readable codes
2. **Per-endpoint error types** — Maximum flexibility, maximum duplication (what we had)
3. **RFC 7807 Problem Details** — More complex than needed; our 18 codes cover all cases
4. **Status quo** — 9 formats was already causing debugging difficulty
