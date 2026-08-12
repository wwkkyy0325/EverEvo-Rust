# Sandbox Complete Plan
> **状态**:⛔ 已过时(归档)— 沙箱已实现 4 层,见 [06-tool-system.md](../architecture/06-tool-system.md) §sandbox;本方案是 5 层设计愿景
> **来源**:2026-07-18 | **归档**:2026-08-12。以代码现状文档为准。

---


Based on Claude Code permission model (6 modes), Firecracker/gVisor isolation levels,
and Windows AppContainer + Job Objects defense-in-depth.

---

## 1. Permission Levels (5 tiers, inspired by Claude Code)

```
L0: ReadOnly    — 只读文件系统, 禁止写, 禁止网络, 禁止 Shell
L1: Sandboxed   — Shell 仅限 sandbox 目录, 网络允许, 文件系统只读 (Phase 3)
L2: Confirmed   — 用户 UI 确认后才执行 (高风险操作)
L3: Audited     — 无限制但全量审计日志
L4: Trusted     — 完全访问 (bootstrap/download 等系统操作)
```

### Level Assignment Rules

| Tool | Default Level | Reasoning |
|------|--------------|-----------|
| `shell` (general) | L2 Confirmed | LLM 可能注入危险命令 |
| `shell` (npm/pip/cargo install) | L1 Sandboxed | 包管理在 sandbox 内执行 |
| `download` | L4 Trusted | URL 白名单, saved to sandbox |
| `file_read` | L0 ReadOnly | 只能读, 不能写 |
| `file_write` | L1 Sandboxed | 只能写 sandbox 目录 |
| `bootstrap` | L4 Trusted | 系统初始化操作 |

---

## 2. Execution Tiers (5-layer fallback)

```
┌─────────────────────────────────────────────────────────┐
│ Tier 0: WSL (Linux kernel isolation)                    │
│   wsl.exe -e sh -c "..."                                │
│   最强: 完整 Linux 内核隔离                              │
│   条件: WSL 2 已安装                                    │
│                                                         │
│ Tier 1: AppContainer (Windows deny-by-default)           │
│   CreateAppContainerProfile + LPAC                       │
│   文件系统: 只允许 data/sandbox/ + data/runtime/ 读      │
│   网络: deny-by-default, 仅允许白名单域名                 │
│   条件: Windows 10 1607+, 需要 rappct crate              │
│                                                         │
│ Tier 2: Job Objects (进程树管控)                          │
│   KILL_ON_JOB_CLOSE + 内存上限 + 进程数上限               │
│   条件: 任何 Windows 版本                                │
│                                                         │
│ Tier 3: Filesystem (per-session workspace)               │
│   data/sandbox/{session_id}/                             │
│   条件: 无条件 (总是生效)                                  │
│                                                         │
│ Tier 4: Audit (always-on logging)                        │
│   AuditRecord (timestamp, command, exit_code, duration)  │
│   条件: 无条件 (总是生效)                                  │
└─────────────────────────────────────────────────────────┘
```

### Tier Selection Logic

```rust
fn resolve_tier() -> SandboxTier {
    if wsl_available()  { return Tier::Wsl; }
    if win10_1607_plus(){ return Tier::AppContainer; }
    Tier::JobObject  // minimum on Windows
}
```

---

## 3. Network Control Model

### Three-Policy System

```rust
enum NetworkPolicy {
    Allowed,           // 全端口出站 (默认)
    Restricted(Vec<String>),  // 仅允许白名单域名+端口
    Denied,            // 完全断网
}
```

### Default Rules

| Dest | Policy | Reason |
|------|--------|--------|
| `*.python.org`, `*.npmjs.org`, `*.crates.io` | Allowed | 包管理 |
| `hf-mirror.com`, `*.huggingface.co` | Allowed | 模型下载 |
| `localhost:3000-9999` | Allowed | 本地服务 |
| `0.0.0.0`, `127.*`, `192.168.*` | Allowed | 本地回环 |
| `*:22` (SSH) | Ask | 可能用于数据外泄 |
| `*:25/587/465` (SMTP) | Denied | 禁止邮件外泄 |
| LAN/WAN egress to unknown IPs | Ask | 需用户确认 |

### Implementation (Phase 3)

- Windows: Windows Filtering Platform (WFP) + `NetRateControlInformation`
- Linux: iptables/nftables + network namespace
- Docker: `--network=none` + proxy container for allowed routes

---

## 4. Filesystem Control

```
Path                                             Access
────────────────────────────────────────────────────────────
data/sandbox/{session}/         Read+Write      沙箱工作区
data/runtime/                   ReadOnly        运行时文件
data/models/                    ReadOnly        模型文件
data/downloads/                 ReadOnly        下载缓存
docs/llmwiki/                   ReadOnly        项目知识库
./ (project root)               ReadOnly        项目源文件
~/.ssh/                         Denied          禁止读取
~/.aws/                         Denied          禁止读取
~/.env*, .env                   Denied          禁止读取
C:\*, /etc/, /var/              Denied          禁止 (除非 Confirmed)
```

### Implementation

- Windows: AppContainer capability allowlisting
- Linux: bubblewrap (`bwrap --ro-bind /safe/path --tmpfs /tmp`)
- FS-Only tier: path validation before file operations

---

## 5. Permission Configuration (Claude Code Compatible)

```toml
# everevo.toml — per-project or per-user

[permissions]
mode = "default"  # default | acceptEdits | plan | auto | strict

[permissions.allow]
shell = ["git *", "npm test", "npm run *", "cargo check", "cargo build"]
file_write = ["data/sandbox/**", "*.tmp", "*.log"]

[permissions.deny]
shell = ["rm -rf *", "curl * | sh", "wget * -O /dev/null"]
file_read = ["~/.ssh/*", "~/.aws/*", ".env*"]
network = ["*:22", "*:25", "*:587"]

[permissions.ask]
shell = ["git push *", "npm publish", "cargo publish", "docker *"]
network = ["*:*"]  # any non-allowlisted outbound
```

### Rule Evaluation Order

```
1. Deny (highest priority) → blocks execution
2. Ask → prompts user in UI
3. Allow → executes without prompt
```

Deny rules take precedence — a broad deny rule always overrides a narrow allow rule (matching Claude Code behavior).

---

## 6. Audit Trail

Every sandbox execution records:

```json
{
  "id": "exec_abc123",
  "session_id": "sess_xyz",
  "timestamp": "2026-07-18T16:00:00Z",
  "tool": "shell",
  "command": "pip install requests",
  "shell": "PowerShell",
  "tier": "JobObject",
  "permission_level": "Sandboxed",
  "network_policy": "Allowed",
  "exit_code": 0,
  "duration_ms": 2340,
  "killed_by_timeout": false,
  "user_confirmed": true,
  "stdout_hash": "sha256:abc...",
  "files_accessed": ["data/sandbox/sess_xyz/req.txt"],
  "network_connections": ["pypi.org:443"]
}
```

### Audit Storage

- Memory: last 1000 records in `TieredSandbox.audit_log`
- SQLite: `data/db/audit.db` (async flush, batch insert)
- Frontend: `GET /api/sandbox/audit?session_id=X` → timeline view

---

## 7. Implementation Phases

### Phase 2 (Now)
- [x] `SandboxProvider` trait in core ✅
- [x] `TieredSandbox` with timeout + PATH + Job Objects ✅
- [x] AuditRecord + audit_log ✅
- [ ] Permission levels: ReadOnly, Sandboxed, Audited, Trusted
- [ ] ShellTool applies L2 (Confirmed) for general commands
- [ ] Network policy: Allowed by default, Denied for high-risk ports
- [ ] `--permission-mode` CLI flag

### Phase 3 (Filesystem + Network)
- [ ] AppContainer on Windows (via `rappct` crate or custom FFI)
- [ ] bubblewrap on Linux (filesystem namespacing)
- [ ] Network: Windows Filtering Platform + iptables
- [ ] Path allowlist/denylist enforcement
- [ ] Permission config file (TOML)
- [ ] UI confirmation flow for L2 operations

### Phase 4 (Full Isolation)
- [ ] WASM sandbox tier (wasmtime with fuel metering)
- [ ] Docker sandbox tier (bollard, optional)
- [ ] gVisor integration (Linux, optional)
- [ ] Audit dashboard in frontend
- [ ] Anomaly detection on audit logs (ML-based?)

---

## 8. Risk Matrix

| Risk | Mitigation | Status |
|------|-----------|--------|
| LLM injects `rm -rf /` | L2 Confirmed + deny rule | Phase 2 |
| LLM exfiltrates data via curl | Network deny for `*:22`, `*:25` + audit | Phase 2 |
| LLM reads AWS keys from `~/.aws/` | Filesystem deny for `~/.aws/*` | Phase 3 |
| Child process fork bombs | Job Objects `ActiveProcessLimit=1` | Phase 2 |
| Subprocess survives parent crash | `KILL_ON_JOB_CLOSE` | Phase 2 ✅ |
| Zip bomb extraction fills disk | `max_file_size_mb` limit | Phase 3 |
| Memory exhaustion | Job Objects `JobMemoryLimit` | Phase 2 |

---

## References

- Claude Code Permissions: https://code.claude.com/docs/en/permissions
- Claude Code Sandboxing: https://anthropic.com/engineering/claude-code-sandboxing
- Arapuca (cross-platform sandbox): https://github.com/sergio-correia/arapuca
- rappct (Windows AppContainer): https://lib.rs/crates/rappct
- Firecracker vs gVisor: https://segmentfault.com/a/1190000047288554