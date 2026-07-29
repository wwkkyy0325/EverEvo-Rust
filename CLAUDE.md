# CLAUDE.md — EverEvo Project

## Verification Pipeline (Run After Every Change)

### Quick verify (30s — during development)
```bash
cargo check --workspace && cargo test -p everevo-agent --lib && cd frontend && npx tsc --noEmit
```

### Full verify (2min — before commit/PR)
```bash
cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace && cd frontend && npx tsc --noEmit && npx vite build
```

### Incremental test (run only what you changed)
```bash
# Only changed everevo-sandbox:
cargo check -p everevo-sandbox && cargo test -p everevo-sandbox

# Only changed frontend:
cd frontend && npx tsc --noEmit && npx vite build
```

### Rules
- Never claim completion without fresh verification output
- Report failures with exact `file:line` references
- If a test fails, fix the code — never weaken or delete tests
- Run `cargo check` NOT `cargo build` (faster, same type-checking)

## Architecture

```
EverEvo-Rust/
├── crates/
│   ├── everevo-core/       # Shared types, traits, context pipeline, LLM types
│   ├── everevo-agent/      # Agent loop, tools, memory, persona, skills, code search
│   ├── everevo-server/     # Axum HTTP server, SSE chat, routes, app state
│   ├── everevo-db/         # SQLite via SQLx, migrations, message persistence
│   ├── everevo-sandbox/    # Tiered sandbox execution with permission levels
│   ├── everevo-vector/     # ONNX embeddings, HNSW vector store, chunk constructors
│   ├── everevo-knowledge/  # Knowledge graph (Oxigraph) + domain document ingestion
│   ├── everevo-bootstrap/  # First-run runtime & model provisioning
│   ├── everevo-downloader/ # Multi-mirror resumable concurrent download engine
│   ├── everevo-mcp/        # MCP client (stdio + HTTP transports)
│   ├── everevo-workflow/   # JSON-defined multi-step automation workflows
│   └── everevo-bundler/    # Standalone asset bundler binary (CLI)
├── frontend/               # React + Vite + Zustand + Tailwind v4
└── migrations/             # SQL migration files (auto-applied by sqlx)
```

### Key Decisions

1. **Content-block SSE** (not raw token streaming)
   → Reason: enables interleaved thinking/tool/text rendering
2. **Draft-in-messages** (not separate streamMessage state)
   → Reason: abort preserves partial content in the message list
3. **Pluggable ContextStage pipeline** (not monolithic prompt builder)
   → Each stage is a trait impl with priority ordering, injectable independently
4. **Per-tool DB persistence** (not batched TurnComplete)
   → Reason: preserves thinking↔tool interleaving across page refreshes

### Context Pipeline (priority order)

```
[0] SystemPrompt       → static instructions + tool descriptions
[1] Persona            → user communication style + thinking paradigm
[2] BestPractices      → verification, planning, code quality rules
[3] Skill              → loaded SKILL.md instructions
[4+] Memory/Domain     → relevant facts and domain docs (RAG)
[80] History           → conversation messages (sliding window)
[99] LatestMessage     → current user input
```

Stages are trait objects (`ContextStage`). Add new stages via `.with_stage()` without touching core logic.

## Code Conventions

- **Rust**: standard idioms, `cargo fmt`, `cargo clippy`. Tests in `#[cfg(test)] mod tests {}`
- **TypeScript**: React 18, Zustand 5, Tailwind 4. Components in `frontend/src/components/`
- **Commits**: conventional commits (`feat:`, `fix:`, `chore:`)
- **Imports**: match existing ordering. Remove imports YOUR changes made unused.
- **Naming**: snake_case (Rust), camelCase (TS). Match surrounding code style.

## Interface Change Protocol

When you modify a **public API** (trait, struct field, function signature, HTTP endpoint, or store type):

1. **Update `docs/llmwiki/api-registry.md`** — change the "Last Changed" date
2. **If breaking change** — write an ADR in `docs/llmwiki/adr/NNN-title.md` using the template
3. **Update `docs/llmwiki/changelog.md`** — append a dated entry summarizing what changed and why
4. **Check all consumers** — grep for the changed name across the workspace, fix all callers
5. **Run incremental test** — `cargo test -p <affected-crate>` for each changed crate

## Routing

- **API inventory**: `docs/llmwiki/api-registry.md` (all public interfaces + stability status)
- **Architecture decisions**: `docs/llmwiki/adr/` (ADR records)
- **Current work**: check TodoWrite panel or ask user
- **Architecture details**: `docs/llmwiki/design.md`
- **DB schema**: `migrations/` directory + `crates/everevo-db/src/models.rs`
- **API routes**: `crates/everevo-server/src/routes/`
- **Frontend store**: `frontend/src/store.ts` (Zustand)
