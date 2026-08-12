# Testing Strategy
> **状态**:⛔ 已过时(归档)— 测试金字塔已并入 [CLAUDE.md](../../../CLAUDE.md);MockLlmProvider 设计已实现
> **来源**:2026-07-17 | **归档**:2026-08-12。

---


## Four-Layer Pyramid

```
╔══════════════════════════════════════════════════════════╗
║  L4: E2E (manual / nightly CI)                          ║  ~30s each
║  Real LLM + real tools + full loop                       ║  $0.01-0.10 each
║  Trigger: cargo test -- --ignored                        ║
╠══════════════════════════════════════════════════════════╣
║  L3: Integration (CI, daily)                             ║  ~1-5s each
║  Mock LLM + real DB + real sandbox                      ║  $0
║  Trigger: cargo test --test integration                  ║
╠══════════════════════════════════════════════════════════╣
║  L2: Agent Logic (CI, every commit)                      ║  ~50ms each
║  MockLlmProvider → canned responses → assert behavior    ║  $0
║  Trigger: cargo test -p everevo-agent                    ║
╠══════════════════════════════════════════════════════════╣
║  L1: Pure Functions (CI, every save)                     ║  <10ms each
║  Types, config, errors, mirror transforms                ║  $0
║  Trigger: cargo test --workspace                         ║
╚══════════════════════════════════════════════════════════╝
```

## L1: Pure Function Unit Tests

**What to test:** Every `impl`, every `fn` that doesn't need I/O.

**Pattern:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults_sane() { ... }
    #[test]
    fn test_error_display() { ... }
    #[test]
    fn test_mirror_transform() { ... }
}
```

**Examples in project:**
- `crates/everevo-core/src/config.rs` — data dir resolution, provider env parsing
- `crates/everevo-core/src/error.rs` — display formatting, `From` conversions
- `crates/everevo-downloader/src/mirror.rs` — URL extraction, GitHub transform

**Run:** `cargo test --workspace` (fast, no setup needed)

---

## L2: Agent Logic with MockLlmProvider

**What to test:** Agent loop branches, tool dispatch, session management — without a real LLM.

**Key tool:** `MockLlmProvider` in `everevo_agent::llm`

**Pattern:**
```rust
let mock = MockLlmProvider::new()
    .with_tool_call("web_search", json!({"query": "rust"}))  // turn 1
    .with_text("Found results about Rust");                   // turn 2

// Run agent loop with mock
// Assert: tool was called with correct args
// Assert: final response contains expected text
// Assert: call log shows correct message history
```

**Capabilities:**
- `.with_text("...")` — queue a text response
- `.with_tool_call("name", args)` — queue a tool call response
- `.with_response(LlmResponse { ... })` — full control
- `.with_stream(vec![...])` — for streaming tests
- `.call_log()` — inspect what messages were sent (assert prompt construction)
- `.call_count()` — verify number of LLM invocations

**Examples in project:**
- `crates/everevo-agent/tests/mock_agent_loop.rs`

**Run:** `cargo test -p everevo-agent`

---

## L3: Integration Tests

**What to test:** Cross-crate behavior — DB operations, downloader mirror resolution, tool registry wiring.

**Pattern:**
```rust
// In crates/*/tests/integration.rs
#[tokio::test]
async fn test_create_and_list_sessions() {
    let db = Database::connect("sqlite://:memory:").await.unwrap();
    // ... test real SQL operations
}
```

**Examples in project:**
- `crates/everevo-db/tests/integration.rs` — SQLite in-memory: CRUD, search, cascade delete
- `crates/everevo-downloader/tests/integration.rs` — mirror resolution, config, task building

**Run:** `cargo test --test integration` (per crate)

---

## L4: E2E with Real LLM (gated behind `#[ignore]`)

**What to test:** Full agent loop with a real LLM, real API calls, real tool execution.

**Pattern:**
```rust
#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY and costs ~$0.02"]
async fn test_agent_web_search_real() {
    if std::env::var("CI").is_ok() && std::env::var("RUN_EXPENSIVE_TESTS").is_err() {
        return; // Skip in CI by default
    }
    // Real LLM + real tools
}
```

**Usage:**
```bash
# Run all E2E tests (expensive!)
RUN_EXPENSIVE_TESTS=1 cargo test -- --ignored

# Run specific E2E test
cargo test -p everevo-agent test_agent_web_search_real -- --ignored --nocapture
```

**Cost control:**
- Always use cheapest model (Haiku / GPT-4o-mini) for E2E
- Batch together in a nightly CI workflow, not per-commit
- Record golden responses and compare for regression detection

---

## Quick Verification Workflow

When adding a new feature, run these in order:

```bash
# 1. Fast: all unit + L2 tests (seconds)
cargo test --workspace --lib

# 2. Slower: integration tests (~30s)
cargo test --workspace --test integration

# 3. Optional: E2E (manual, costs money)
cargo test --workspace -- --ignored
```

**Build-check everything:**
```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo fmt --check
```

---

## MockLlmProvider Design Principles

1. **FIFO queue** — responses are consumed in order, matching the ReAct loop's turn-by-turn pattern
2. **Call log** — records all messages sent to the mock, enabling assertions on prompt construction
3. **Exhausted error** — panics with a clear message if you forget to queue enough responses
4. **Zero deps** — no extra crates, just `std::sync::Mutex` + `Vec`
5. **Trait impl** — same `LlmProvider` trait as the real client, so the agent code doesn't know the difference

---

## What NOT to Test

- ❌ **Don't test framework code** — Axum/Tokio/SQLx behavior is their responsibility
- ❌ **Don't test Rust stdlib** — `serde_json::from_str` is already tested
- ❌ **Don't test trivial getters** — `fn name() -> &str { "tool" }` doesn't need a test
- ❌ **Don't mock external services for L1/L2** — use the real thing (or skip with `#[ignore]`)
- ❌ **Don't test implementation details** — test behavior, not internal state