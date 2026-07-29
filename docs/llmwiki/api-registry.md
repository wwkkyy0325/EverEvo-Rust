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
| `LlmMessage` | Struct | **Stable** | 2026-07-18 (added `thinking`) | — |
| `EverEvoError` | Enum | **Stable** | 2026-07-26 (`Tool` variant structured) | — |
| `StreamEvent` | Enum | **Stable** | 2026-07-18 | — |

## everevo-agent (Agent Loop & Events)

| Interface | Kind | Stability | Last Changed |
|-----------|------|-----------|-------------|
| `AgentEvent` | Enum | **Stable** | 2026-07-25 (added SubAgent variants) |
| `AgentLoop` | Struct | **Stable** | 2026-07-26 (added `cancel_token`) |
| `AgentLoop::run()` | Method | **Stable** | 2026-07-26 (added `with_cancel_token`) |
| `HttpClient::stream_chat()` | Method | **Stable** | 2026-07-26 (added `_cancel` param) |

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

## Frontend (Store & Components)

| Interface | Kind | Stability | Last Changed |
|-----------|------|-----------|-------------|
| `MessageItem` | Type | **Stable** | 2026-07-26 (added `blocks`, `activeBlockIdx`) |
| `ContentBlock` | Type | **Stable** | 2026-07-26 |
| `ChatState` (Zustand) | Store | **Stable** | 2026-07-26 (added `todos`, `subagentTasks`) |
| `ChatBubble` | Component | **Stable** | 2026-07-26 (added `blocks` rendering) |

---

## Stability Levels

| Level | Meaning |
|-------|---------|
| **Stable** | 不会破坏性变更。新增字段/参数可以，删减需要 ADR + deprecated 过渡 |
| **Evolving** | 可能变更，但会尽量向后兼容 |
| **Unstable** | 随时可能变更，不保证兼容 |
| **Deprecated** | 将在下个版本移除，已有替代方案 |
