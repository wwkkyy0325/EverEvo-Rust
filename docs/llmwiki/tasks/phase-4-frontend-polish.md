# Phase 4: Frontend + Polish

**Goal:** Full-featured chat UI, tool visualization, KG visualization, llmwiki management, production-ready.

---

## Tasks

### 4.1 — Chat UI
- [ ] Streaming message display (tokens appear in real-time)
- [ ] Markdown rendering with code syntax highlighting
- [ ] User/bot message bubbles with avatars
- [ ] Message actions: copy, retry, edit (for user messages)
- [ ] Auto-scroll to bottom, smart scroll-away detection
- [ ] Input area: text input, send button, stop generation button
- **Verify:** Chat feels smooth — streaming, markdown, code blocks all render correctly

### 4.2 — Tool Call Visualization
- [ ] Expandable tool call cards in message stream
- [ ] Show: tool name, parameters, execution status (pending/running/done/error), result
- [ ] Color-coded by tool category (search=blue, file=green, shell=yellow, code=purple)
- [ ] Collapse/expand animation
- **Verify:** Agent uses web_search → card appears with query → expands to show results

### 4.3 — Session Management UI
- [ ] Sidebar: session list with titles, timestamps
- [ ] Create new session, delete session, rename session
- [ ] Search across sessions (full-text search over messages)
- [ ] Session switching with history loading
- **Verify:** Create 3 sessions with different topics, switch between them, search finds messages

### 4.4 — Knowledge Graph Visualizer
- [ ] Force-directed graph visualization (d3-force or similar)
- [ ] Nodes: entities (colored by type), Edges: relations (labeled)
- [ ] Click node → show entity details + related entities
- [ ] Search: find entity by name, highlight its subgraph
- [ ] Graph updates in real-time as agent extracts new entities
- **Verify:** Agent extracts entities from conversation → graph updates → user can explore

### 4.5 — RAG & llmwiki Management UI
- [ ] Document upload area (drag & drop files)
- [ ] Document list with ingestion status
- [ ] llmwiki doc editor (markdown with preview)
- [ ] Re-index button for llmwiki
- [ ] Search interface: query → show results with source chunks
- **Verify:** Upload a PDF → indexed → search returns relevant chunks

### 4.6 — Production Polish
- [ ] Error handling: graceful LLM errors, retry logic, user-friendly messages
- [ ] Loading states for all async operations
- [ ] Responsive layout (works on various screen sizes)
- [ ] Dark/light theme toggle
- [ ] Keyboard shortcuts (Ctrl+Enter send, Ctrl+K command palette)
- [ ] Startup script / Docker Compose for one-command launch
- [ ] `README.md` with setup instructions
- **Verify:** Fresh clone → follow README → app running in under 5 minutes

### 4.7 — File-Based Config (from Audit)
- [ ] Add `config-rs` or `figment` dependency for layered config
- [ ] `AppConfig::from_file(path)` — deserialize from TOML/YAML/JSON
- [ ] Merge priority: env vars > config file > defaults
- **Verify:** `everevo serve --config everevo.toml` works

### 4.8 — OpenAPI/Swagger Docs (from Audit)
- [ ] Integrate `utoipa` crate with Axum
- [ ] Auto-generate OpenAPI spec from Rust types and route handlers
- [ ] Serve Swagger UI at `/docs`
- **Verify:** Browser → `/docs` → interactive API explorer

### 4.9 — Rate Limiting (from Audit)
- [ ] Add rate limiting middleware (tower-governor or custom)
- [ ] Per-session and global rate limits on `/api/chat`
- **Verify:** Rapid-fire messages get 429 responses after limit exceeded

### 4.10 — Dependency Audit CI (from Audit)
- [ ] Add `cargo-deny` config — whitelist allowed licenses, ban specific crates
- [ ] Add `cargo-audit` to CI — detect known vulnerabilities
- [ ] GitHub Actions: `cargo check + clippy + test + fmt + audit + deny`
- **Verify:** CI gate blocks PRs with security issues or forbidden deps
