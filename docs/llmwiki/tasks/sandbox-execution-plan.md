# Sandbox Execution Plan — Choke Point Architecture

## Principle: EverEvo Sandbox is the ONLY process execution gateway

```
All tools (ShellTool, DownloadTool, BootstrapTool, future tools)
  → must call sandbox.execute() to spawn ANY subprocess
  → must call sandbox.open_file() to write outside sandbox dir
  → must call sandbox.connect_network() to make outbound connections
  → CANNOT import tokio::process::Command or std::process::Command directly
```

## Choke Point Verification

Audit result (2026-07-18): all `Command::new` calls live in `everevo-sandbox/`. No leakage. ✅

| File | Process Spawn? | Status |
|------|---------------|--------|
| `everevo-sandbox/src/provider.rs` | `tokio::process::Command` | ✅ Authorized (sandbox itself) |
| `everevo-sandbox/src/resolved.rs` | `std::process::Command` | ✅ Authorized (one-time WSL detection) |
| `everevo-agent/src/tools/builtins/shell.rs` | None | ✅ Uses `Arc<dyn SandboxProvider>` |
| All other crates | None | ✅ |

---

## Implementation Plan

### Task 1: SandboxProvider — Add filesystem and network methods

Currently only has `execute()`. Add the full triad:

```rust
pub trait SandboxProvider: Send + Sync {
    async fn execute(&self, config: &ExecutionConfig) -> Result<ExecutionResult>;
    async fn open_file(&self, path: &Path, mode: FileMode) -> Result<File>;
    async fn connect_network(&self, host: &str, port: u16, policy: NetworkPolicy) -> Result<Stream>;
}
```

### Task 2: Permission enforcement gate (single chokepoint)

```rust
impl TieredSandbox {
    fn check_permission(
        command: &str,
        level: PermissionLevel,
        rules: &PermissionRules,
    ) -> Result<(), SandboxError> {
        // 1. Deny patterns (highest priority)
        if command_is_denied(command, &rules.shell_deny_commands) {
            return Err(SandboxError::Denied(command.into()));
        }
        // 2. Permission level checks
        if level <= ReadOnly && is_write_op(command) {
            return Err(SandboxError::Denied("Write not allowed at ReadOnly".into()));
        }
        Ok(())
    }
}
```

### Task 3: CI lint — deny direct Command usage

Add to `Cargo.toml`:
```toml
[workspace.lints.clippy]
disallowed-methods = { level = "deny", priority = 100 }
```

Add `clippy.toml`:
```toml
disallowed-methods = [
    "std::process::Command::new",
    "tokio::process::Command::new",
    "std::process::Command::output",
    "tokio::process::Command::output",
]
```

With exceptions for `everevo-sandbox/src/resolved.rs` (WSL detection only).

### Task 4: Sandbox configuration (TOML file)

```toml
# everevo.toml
[sandbox]
prefer_wsl = true
default_timeout_secs = 30
max_timeout_secs = 300
default_memory_mb = 512

[sandbox.permissions]
level = "sandboxed"  # readonly | sandboxed | confirmed | audited | trusted

[sandbox.permissions.allow]
shell = ["git *", "npm test", "cargo check"]
file_write = ["data/sandbox/**"]

[sandbox.permissions.deny]
shell = ["rm -rf *", "curl * | sh"]
network = ["*:22", "*:25"]
```

### Task 5: Wire sandbox into AppState

```rust
// everevo-server/src/app_state.rs
pub struct AppState {
    pub sandbox: Arc<TieredSandbox>,  // always present
    // ... other fields
}
```

All tools receive `Arc<dyn SandboxProvider>` via constructor injection.
No tool can spawn a process without going through sandbox.

### Task 6: Audit persistence

```rust
// On each sandbox execution:
// 1. Write AuditRecord to in-memory buffer
// 2. Batch flush to SQLite every 10 records or 30 seconds
// 3. Expose via GET /api/sandbox/audit?session_id=X
```

---

## Execution Order

```
1. ✅ SandboxProvider trait in core (DONE)
2. ✅ TieredSandbox with timeout + PATH + Job Objects (DONE)
3. ✅ Permission levels + deny patterns (DONE)
4. ✅ AuditRecord + audit_log (DONE)
5. [NOW] Filesystem/network methods on SandboxProvider trait
6. [NOW] Permission enforcement gate (single entry point)
7. [NOW] CI lint: deny direct Command usage outside sandbox
8. Phase 2: AppContainer integration (rappct)
9. Phase 2: Audit SQLite persistence
10. Phase 3: Path allowlist enforcement
11. Phase 3: Network proxy enforcement (WFP/iptables)
```

---

## Cannot-Bypass Guarantee

| Layer | Mechanism |
|-------|-----------|
| **Architecture** | `SandboxProvider` trait is the ONLY way to spawn subprocesses |
| **Code review** | CI lint denies `Command::new` in non-sandbox crates |
| **Runtime** | Permission gate checks EVERY execution before it spawns |
| **Audit** | Every execution recorded in AuditRecord — nothing escapes logging |
| **Injection** | All tools get `Arc<dyn SandboxProvider>` via constructor — no static/global access |
