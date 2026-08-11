# API Registry — Living Interface Inventory

> 所有公开接口及其稳定性状态。**修改任何接口后必须更新此文档**。

---

## everevo-core (Traits & Types)

| Interface | Kind | Stability | Last Changed | ADR |
|-----------|------|-----------|-------------|-----|
| `Tool` trait | Trait | **Stable** | 2026-07-26 (added `cancel` param) | — |
| `Tool::execute()` | Method | **Stable** | 2026-07-26 (3 params) | — |
| `ContextStage` trait | Trait | **Stable** | 2026-07-17 | — |
| `ContextPipeline` | Struct | **Stable** | 2026-07-18 | — |
| `SandboxProvider` trait | Trait | **Stable** | 2026-07-18 | — |
| `LlmMessage` | Struct | **Stable** | 2026-07-30 (added `images: Vec<ImageData>`) | — |
| `ImageData` | Struct | **Stable** | 2026-07-30 (new — multimodal base64 image) | — |
| `ToolOutput` | Struct | **Stable** | 2026-07-30 (added `images`, `Default`, `text()` ctor) | — |
| `EverEvoError` | Enum | **Stable** | 2026-07-30 (added `Network` variant) | — |
| `ApiError` | Struct | **Stable** | 2026-08-06 (new — unified HTTP error) | — |
| `ErrorCode` | Enum | **Stable** | 2026-08-06 (new — 18 machine-readable codes) | — |
| `ToolSchema` | Struct | **Stable** | 2026-08-11 (added `native_type: Option<String>` — server-side tool type, emitted WITHOUT `input_schema`, non-breaking) | — |
| `LlmProvider::native_web_search_tool()` | Method | **Stable** | 2026-08-11 (new — default `None`; providers declare a server-executed search tool) | — |
| `StreamEvent` | Enum | **Stable** | 2026-08-11 (added `ServerToolUse { name }` + `Done.stop_reason: Option<String>` — server-side tool signal + provider stop_reason, non-breaking) | — |
| `ContextBuildContext.summary` | Field | **Stable** | 2026-08-11 (new — `Option<String>` durable rolling-summary slot consumed by `RollingSummaryStage`) | — |
| `RollingSummaryStage` | Struct | **Stable** | 2026-08-11 (new — priority 75; injects `<conversation_summary>` user message before history; empty summary → no-op) | — |
| `TelemetryStage` trait | Trait | **Stable** | 2026-08-10 (new — registered emission pipeline, mirrors `ContextStage`) | 0004 |
| `TelemetryPipeline` | Struct | **Stable** | 2026-08-10 (new — priority-ordered stages + sink) | 0004 |
| `TelemetryEmitContext` | Struct | **Stable** | 2026-08-10 (new — per-emit slice inputs, all-Option) | 0004 |
| `TelemetryRecord` | Enum | **Stable** | 2026-08-10 (new — `AgentTurn` / `Retrieval`) | 0004 |
| `default_telemetry_pipeline()` | Fn | **Stable** | 2026-08-10 (new — registers retrieval + turn stages) | 0004 |

## everevo-mcp (MCP Client)

| Interface | Kind | Stability | Last Changed |
|-----------|------|-----------|-------------|
| `McpClient::call_tool()` | Method | **Stable** | 2026-07-30 (returns `(String, Vec<ImageData>)` — was `String`) |
| `McpTool::execute()` | Method | **Stable** | 2026-07-30 (populates `ToolOutput.images`) |

## everevo-agent (Agent Loop & Events)

| Interface | Kind | Stability | Last Changed |
|-----------|------|-----------|-------------|
| `AgentEvent` | Enum | **Stable** | 2026-08-10 (added `Retrospective { summary }`, non-breaking — emitted before `Done`) |
| `AgentLoop` | Struct | **Stable** | 2026-07-26 (added `cancel_token`) |
| `AgentLoop::run()` | Method | **Stable** | 2026-07-26 (added `with_cancel_token`) |
| `AgentLoop::with_telemetry()` | Method | **Stable** | 2026-08-10 (BREAKING — `Arc<Telemetry>` → `Arc<TelemetryPipeline>`) | 0004 |
| `MemoryStage::with_telemetry()` | Method | **Stable** | 2026-08-10 (BREAKING — `Arc<Telemetry>` → `Arc<TelemetryPipeline>`) | 0004 |
| `MemoryFact.session` | Field | **Stable** | 2026-08-10 (new — `Option<String>`, `#[serde(default)]`, non-breaking; 分层记忆 session scoping) |
| `MemoryStage::with_session_id()` | Method | **Stable** | 2026-08-10 (new — recall filtered to global tier + own session) |
| `MemoryTool::with_session_id()` | Method | **Stable** | 2026-08-10 (new — `memory add` tags facts with the session) |
| `memory` tool `scope` param | Tool | **Stable** | 2026-08-10 (new — `"session"` default / `"global"` promotion) |
| `FactManager::read_index_lean()` | Method | **Removed** | 2026-08-10 (orphaned — only caller replaced by session-filtered index) |
| `HttpClient::stream_chat()` | Method | **Stable** | 2026-07-26 (added `_cancel` param) |
| `AgentCharacterStage` / `AgentCharacter` | Struct | **Stable** | 2026-08-05 (new — agent's own voice; priority 0) |
| `AnswerDisciplineStage` | Struct | **Stable** | 2026-08-11 (new — output-fidelity: final-answer marker, verbatim list, constraint/candidate checks; priority 2) |
| `EvidenceChecklistStage` | Struct | **Stable** | 2026-08-11 (new — ECLoop-style pre-commit evidence checklist + deterministic verifier gate; priority 2) |
| `build_character_block(profile_path)` | Fn | **Stable** | 2026-08-05 (new — renders character + sources) |
| `synthesize_character(path, llm)` / `SynthesisReport` | Fn | **Stable** | 2026-08-05 (new — LLM distills fragments → traits) |
| `load_character(path)` | Fn | **Stable** | 2026-08-05 (new — load/auto-create profile) |
| `AgentLoop::with_compact_llm()` | Method | **Stable** | 2026-08-11 (new — compaction/rolling-summary model; `None` → falls back to the main model, behavior unchanged) |
| `AgentLoop::with_background_maintenance()` | Method | **Stable** | 2026-08-11 (new — Layer-1 per-turn background rolling-summary maintenance at soft threshold) |
| `AgentLoop::with_tool_cache_dir()` | Method | **Stable** | 2026-08-11 (new — paged tool outputs written to `data/sessions/<id>/tool_cache/`) |
| `BackgroundMaintenance` | Struct | **Stable** | 2026-08-11 (new — DB-persisted incremental summary + watermark; `in_flight` AtomicBool guards) |
| `maintain_rolling_summary()` / `RollingSummaryResult` | Fn | **Stable** | 2026-08-11 (new — budget-aware chunked summarization, rule-1 no re-summarize, extractive fallback) |
| `SUMMARY_CAP_TOKENS` / `TOOL_PAGE_THRESHOLD_CHARS` / `TOOL_PAGE_PREVIEW_CHARS` | Const | **Stable** | 2026-08-11 (new — 2048 / 30_000 / 2000) |
| `autocompact()` | Fn | **Stable** | 2026-08-11 (Layer-2 — now folds an existing `<conversation_summary>` verbatim as prefix, summarizes only post-watermark messages) |

## everevo-agent (LLM-facing Tools)

| Tool name | Stability | Last Changed | Notes |
|-----------|-----------|-------------|-------|
| `task` | **Stable** | 2026-07-30 (returns `task_id`) | dispatch sub-agents |
| `cancel_task` | **Stable** | 2026-07-30 (new) | LLM cancels a sub-agent by task_id |
| `TodoWrite` | **Stable** | 2026-08-10 (status enum +3: `failed`/`skipped`/`deferred`, non-breaking) | |
| `list_workflows` | **Stable** | 2026-07-30 (new) | discover saved workflows |
| `workflow_run` | **Stable** | 2026-07-30 (added `name` param — run by name) | |
| `parallel_agents` | **Stable** | 2026-07-30 (renamed from `Workflow`) | avoids clash with `workflow_run` |
| `describe_image` | **Stable** | 2026-08-11 (new — dedicated vision model; fallback to `chess_fen.py`/`fractions_ocr.py`) | params: `path`, `question?` |
| `tool_cache_read` | **Stable** | 2026-08-11 (new — re-read a paged tool output by absolute path, ~4MB guard) | params: `path` |

## everevo-server (HTTP API)

| Endpoint | Method | Stability | Last Changed |
|----------|--------|-----------|-------------|
| `POST /api/chat` | SSE | **Stable** | 2026-07-26 (content-block events) |
| `GET /api/sessions` | JSON | **Stable** | 2026-07-18 |
| `GET /api/sessions/{id}/messages` | JSON | **Stable** | 2026-07-26 (added `blocks_json`) |
| `GET /api/health` | JSON | **Stable** | 2026-07-26 |
| `GET /api/health/stats` | JSON | **Stable** | 2026-07-26 |
| `GET /api/session/{id}/todos` | JSON | **Stable** | 2026-07-26 |
| `POST /api/chat/{id}/interrupt` | JSON | **Stable** | 2026-07-26 |
| `GET`/`PUT /api/character` | JSON | **Stable** | 2026-08-05 (new — read/write agent voice profile) |
| `LlmProviderConfig.context_window` | Field | **Stable** | 2026-08-11 (new — optional token window; drives budget-aware compaction chunking) |
| `RoutingSettings.vision_model_id` / `compact_model_id` | Field | **Stable** | 2026-08-11 (new — route vision/compaction to separate `[[llm]]` entries; `compact_model_id` unset → main model) |
| `AppState.vision_llm` / `compact_llm` | Field | **Stable** | 2026-08-11 (new — `Option<ResolvedProvider>` resolved from routing on reload) |

## everevo-db (Persistence)

| Interface | Kind | Stability | Last Changed |
|-----------|------|-----------|-------------|
| `SessionRow.context_summary` / `summary_watermark` | Field | **Stable** | 2026-08-11 (new — durable rolling summary + watermark; migration 007; `NULL` → behavior unchanged) |
| `Database::get_session_context()` | Method | **Stable** | 2026-08-11 (new — read `(summary, watermark)`) |
| `Database::update_session_context()` | Method | **Stable** | 2026-08-11 (new — persist summary + advance watermark) |
| `Database::get_messages_after()` | Method | **Stable** | 2026-08-11 (new — `created_at > watermark`, oldest→newest) |
| `Database::get_message_created_at()` | Method | **Stable** | 2026-08-11 (new — resolves a watermark UUID → timestamp; id column is BLOB) |

## Frontend (Store & Components)

| Interface | Kind | Stability | Last Changed |
|-----------|------|-----------|-------------|
| `MessageItem` | Type | **Stable** | 2026-07-26 (added `blocks`, `activeBlockIdx`) |
| `ContentBlock` | Type | **Stable** | 2026-07-26 |
| `ChatState` (Zustand) | Store | **Stable** | 2026-07-26 (added `todos`, `subagentTasks`) |
| `ChatBubble` | Component | **Stable** | 2026-07-26 (added `blocks` rendering) |
| `useRoutingConfig` | Hook | **Stable** | 2026-08-11 (added `visionModelId`/`compactModelId`) |
| `SettingsView` | Component | **Stable** | 2026-08-11 (added vision/compact model dropdowns + `context_window` input; vision selector notes "context ≤ 32K") |

---

## Stability Levels

| Level | Meaning |
|-------|---------|
| **Stable** | 不会破坏性变更。新增字段/参数可以，删减需要 ADR + deprecated 过渡 |
| **Evolving** | 可能变更，但会尽量向后兼容 |
| **Unstable** | 随时可能变更，不保证兼容 |
| **Deprecated** | 将在下个版本移除，已有替代方案 |

---

## Unified Error Format (2026-08-06)

所有 REST 端点统一使用 `ApiError`，产生一致的 JSON 响应：

```json
{
  "error": {
    "code": "NOT_FOUND",
    "message": "human-readable description",
    "details": null
  }
}
```

### Error Codes / HTTP Mapping

| ErrorCode | HTTP Status |
|-----------|------------|
| `NOT_FOUND` | 404 |
| `INVALID_INPUT` | 400 |
| `CONFLICT` | 409 |
| `FORBIDDEN` | 403 |
| `UNAUTHORIZED` | 401 |
| `TOO_MANY_REQUESTS` | 429 |
| `INTERNAL` | 500 |
| `DATABASE_ERROR` | 500 |
| `LLM_PROVIDER_ERROR` | 503 |
| `SANDBOX_ERROR` | 500 |
| `NETWORK_ERROR` | 502 |
| `IO_ERROR` | 500 |
| `CONFIG_ERROR` | 500 |
| `AGENT_ERROR` | 500 |
| `TOOL_ERROR` | 500 |
| `BOOTSTRAP_ERROR` | 500 |
| `TIMEOUT` | 504 |
| `SERVICE_UNAVAILABLE` | 503 |

### 用法

```rust
// Route handler
use everevo_core::ApiError;

async fn my_handler(State(state): State<Arc<AppState>>) -> Result<Json<T>, ApiError> {
    let item = state.db.get(id).await
        .map_err(ApiError::from)?   // EverEvoError → ApiError auto-mapping
        .ok_or_else(|| ApiError::not_found("item not found"))?;
    Ok(Json(item))
}
```
