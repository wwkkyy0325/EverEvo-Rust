# Contributing to EverEvo

Thanks for your interest! This guide covers how to set up, develop, and submit changes.

## Quick Start

**Prerequisites:**
- Rust stable (via [rustup](https://rustup.rs/))
- Node.js 18+ LTS (for frontend)

```sh
git clone <repo-url>
cd EverEvo-Rust

# Backend
cargo build --workspace
cargo test --workspace --lib

# Frontend
cd frontend
npm install
npx tsc --noEmit
```

## Project Structure

| Crate | Purpose |
|-------|---------|
| `everevo-core` | Shared types, traits (`Tool`, `ContextStage`, `SandboxProvider`), `ApiError`, `AppConfig` |
| `everevo-agent` | Agent loop, 22 built-in tools, LLM client, memory pipeline, skills, code search |
| `everevo-server` | Axum HTTP server, SSE chat, routes, orchestration layer |
| `everevo-db` | SQLite via SQLx, message persistence, session CRUD |
| `everevo-sandbox` | Tiered sandbox (read_only/fully_manual/semi_auto/fully_auto) |
| `everevo-vector` | ONNX embeddings + HNSW vector store |
| `everevo-knowledge` | Oxigraph knowledge graph + domain document ingestion |
| `everevo-a2a` | A2A protocol v0.3.0 (agent cards, task execution) |
| `everevo-bootstrap` | Runtime/model provisioning for first run |
| `everevo-downloader` | Resumable concurrent download engine |
| `everevo-mcp` | MCP client (stdio + HTTP transports) |
| `everevo-workflow` | JSON-defined automation workflow engine |
| `everevo-bundler` | Standalone asset bundler CLI binary |
| `everevo-webagent` | Standalone MCP search service binary |
| `frontend/` | React + Vite + Zustand + Tailwind v4 |

### Key Extension Points

| What | Where |
|------|-------|
| Add a new Tool | `everevo-core/src/tool.rs` (trait) → `everevo-server/src/orchestration/tools.rs` (register) |
| Add a new ContextStage | `everevo-core/src/context.rs` (trait) → `everevo-agent/src/stages/pipeline.rs` (register) |
| Add a new HTTP route | `everevo-server/src/routes/` → `routes/mod.rs` (merge) |
| Add a new builtin skill | `everevo-agent/src/skill.rs` (register) + `SKILL.md` file |

## Adding a New Tool

1. **Implement `Tool` trait** in `crates/everevo-agent/src/tools/builtins/`:
   ```rust
   use everevo_core::tool::{Tool, ToolOutput};
   
   pub struct MyTool;
   
   #[async_trait]
   impl Tool for MyTool {
       fn name(&self) -> &str { "my_tool" }
       fn description(&self) -> &str { "What this tool does" }
       fn parameters_schema(&self) -> serde_json::Value { /* JSON Schema */ }
       fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }
       async fn execute(&self, params: serde_json::Value, cancel: Option<CancellationToken>)
           -> Result<ToolOutput, EverEvoError> {
           // Implementation
       }
   }
   ```

2. **Register** in `crates/everevo-server/src/orchestration/tools.rs`:
   ```rust
   registry.register(Arc::new(everevo_agent::tools::builtins::MyTool));
   ```

3. **Export** from `crates/everevo-agent/src/tools/builtins/mod.rs`:
   ```rust
   pub use my_tool::MyTool;
   ```

4. **Test**: `cargo test -p everevo-agent --lib`

5. **Update** `crates/everevo-server/src/routes/tools_routes.rs` with the new tool name.

## Adding a New ContextStage

1. **Implement `ContextStage`** trait:
   ```rust
   use everevo_core::context::{ContextStage, ContextBuildContext, ContextFragment};
   
   pub struct MyStage;
   
   impl ContextStage for MyStage {
       fn priority(&self) -> i32 { 10 }
       fn name(&self) -> &str { "my_stage" }
       fn build(&self, ctx: &ContextBuildContext) -> Option<ContextFragment> {
           // Return injected context or None
       }
   }
   ```

2. **Register** in `crates/everevo-agent/src/stages/pipeline.rs`:
   ```rust
   pipeline.with_stage(MyStage);
   ```

3. **Test**: `cargo test -p everevo-agent --lib`

## Local Validation

Run before every commit:

```sh
# Backend
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace --lib

# Frontend
cd frontend && npx tsc --noEmit && npx vite build
```

For incremental checks (faster):
```sh
cargo check --workspace && cargo test -p everevo-agent --lib && cd frontend && npx tsc --noEmit
```

## Commit Conventions

Format: `<type>(<scope>): <description>`

| Type | When |
|------|------|
| `feat` | New feature or capability |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `refactor` | Code restructuring (no behavior change) |
| `test` | Adding or updating tests |
| `chore` | Maintenance, dependency bumps, CI |
| `perf` | Performance improvement |

Examples: `feat(tools): add browser screenshot tool`, `fix(agent): sub-agent result lost on disconnect`

## File Size Guidelines

| Threshold | Action |
|-----------|--------|
| < 500 lines | No action needed |
| 500–800 lines | Consider splitting by domain boundary |
| > 800 lines | Must split — use subdirectory modules with `pub use` re-exports |

Split by **domain boundary**, not by equal line count. Keep cohesive types and their impls together. Each split file becomes a separate `#[cfg(test)]` module.

## Code Style

### Rust
- `snake_case` for functions/variables, `PascalCase` for types
- `cargo fmt` and `cargo clippy` enforced
- `unwrap()` only in tests; use `unwrap_or_else(|e| e.into_inner())` for Mutex/RwLock
- Never `expect()` in production code that could panic — use `match` + `tracing::error!`
- Public APIs must have doc comments (`cargo doc` must succeed)

### TypeScript
- `camelCase` for variables/functions, `PascalCase` for components
- Prettier formatting (project config)

## Pull Request Process

1. Create a focused branch from `main`
2. Make changes (one concern per PR)
3. Run full verification (see above)
4. Open PR with:
   - What changed and why
   - Test evidence
   - Any breaking API changes flagged
5. All CI checks must pass before merge

## Questions?

- Architecture: read `docs/llmwiki/design.md`
- API inventory: `docs/llmwiki/api-registry.md`
- Past decisions: `docs/llmwiki/adr/`
- Changelog: `docs/llmwiki/changelog.md`
