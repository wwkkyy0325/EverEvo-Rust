# Permission Model & Agent Hierarchy Architecture

**Status:** Design finalized. Implementation pending.

## Design References

- Claude Code 7-mode permission system (plan → bypassPermissions)
- Claude Code subagent isolation model (Issue #4740 — permission boundary bug learned from)
- IETF SOOS MAD Protocol (draft-sato-soos-mad-02): Narrowing Property, cascade revocation
- AWS Defense-in-Depth RAI Multi-Agent: 7-layer governance, DynamoDB audit
- TDCommons Orchestrator Framework: PON → DAPE → SEAs → SCM → SRM → Ledger
- SecureYeoman ADR 004: RBAC inheritance, swarms, human-in-the-loop

---

## 1. Permission Levels (Redesigned)

| Level | Name | Shell | Write | Network | Confirm | Audit | Use Case |
|-------|------|-------|-------|---------|---------|-------|----------|
| 0 | `ReadOnly` | ❌ | ❌ | ❌ | — | ✅ | 代码审查、架构分析、搜索 |
| 1 | `FullyManual` | ✅ | ✅ | ✅ | **每条命令** | ✅ | 敏感项目、生产环境 |
| 2 | `SemiAuto` | ✅ | ✅ | ✅ | **危险命令+计划** | ✅ | 日常开发（默认） |
| 3 | `FullyAuto` | ✅ | ✅ | ✅ | ❌ | ✅ | CI/CD、信任环境 |

### What triggers confirmation at SemiAuto

```
Dangerous command patterns (regex deny list):
  rm -rf, del /f /s, format, dd, >/dev/sda, curl * | sh, chmod 777, sudo, ...

Plan-level triggers:
  - Multi-step plans touching >5 files
  - Plans involving network download + execution
  - Plans modifying system configuration
  - Plans with estimated cost > threshold

Safe (auto-approved) at SemiAuto:
  - git status, git diff, git log
  - cargo build, cargo test, npm test
  - cat, ls, dir, echo
  - Single-file edits with visible diff
```

### Permission Attenuation (The Narrowing Property)

A sub-agent can never acquire permissions its delegator doesn't hold:

```
sub_agent.effective_level = min(sub_agent.own_level, delegator.level)
```

Delegation can only attenuate, never amplify.

---

## 2. Agent Hierarchy

```
┌─────────────────────────────────────────────────────────┐
│                   Main Agent (Orchestrator)               │
│                                                          │
│  Role: Planner, Scheduler, Auditor                       │
│  Permissions: ReadOnly (reads code, plans, audits)       │
│  NEVER executes shell commands or writes files directly   │
│                                                          │
│  Responsibilities:                                       │
│    - Decompose user request into subtask DAG             │
│    - Select sub-agent types + permission levels           │
│    - Spawn sub-agents with scoped execution tokens        │
│    - Monitor sub-agent progress via audit trail           │
│    - Synthesize results, resolve contradictions           │
│    - Escalate ambiguity to user                          │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │ Research │  │   Code   │  │  Shell   │  ...          │
│  │ SubAgent │  │ SubAgent │  │ SubAgent │               │
│  │ (SemiAuto)│  │(SemiAuto)│  │(FullyManual)│            │
│  └──────────┘  └──────────┘  └──────────┘               │
└─────────────────────────────────────────────────────────┘
```

### Agent Types (Planned)

| Type | Role | Default Level | Tools |
|------|------|---------------|-------|
| `MainAgent` | Orchestrator, planner, auditor | `ReadOnly` | Glob, Grep, Read, Agent(spawn), Audit(read) |
| `ResearchAgent` | Web search, documentation lookup | `SemiAuto` | WebSearch, WebFetch, Read |
| `CodeAgent` | Code generation, editing, refactoring | `SemiAuto` | Read, Write, Edit, Glob, Grep |
| `ShellAgent` | Command execution, build, test | `FullyManual` | Bash(sandboxed), Read |
| `ReviewAgent` | Code review, security audit | `ReadOnly` | Read, Glob, Grep |

### Delegation Rules

```
1. MainAgent spawns sub-agents via Agent tool (never executes directly)
2. Sub-agent effective permission = min(its_config_level, main_agent_delegation_level)
3. Max delegation depth = 3 (MainAgent → SubAgent → SubSubAgent)
4. Sub-agent execution tokens are session-scoped and time-bounded (default: 5 min)
5. Cancelling MainAgent cascades to all descendant sub-agents
6. Sub-agent returns summary to MainAgent (not raw output)
```

---

## 3. Audit Trail Architecture

### Record Schema (per action)

```rust
struct AuditRecord {
    // Identity
    timestamp:       DateTime<Utc>,
    session_id:      Uuid,
    agent_id:        String,        // "main", "sub:code:abc123"
    agent_type:      AgentType,     // MainAgent, CodeAgent, ShellAgent, ...

    // Action
    action_type:     ActionType,    // Read | Write | Execute | Search | Delegate
    command:         String,        // The executed command or action description
    working_dir:     String,

    // Permission context
    permission_level: PermissionLevel,
    was_confirmed:    bool,         // Did user explicitly approve this?
    delegator_id:     Option<String>, // Which agent delegated this action?

    // Execution result
    exit_code:        i32,
    duration_ms:      u64,
    killed_by_timeout: bool,
    stdout_len:       usize,
    stderr_len:       usize,

    // Security
    network_allowed:  bool,
    memory_limit_mb:  Option<u64>,
    job_object_applied: bool,
    risk_score:       u8,           // 0-100, computed pre-execution
}
```

### Audit Storage

```
data/sandbox/{session_id}/
├── audit.jsonl              ← All actions, append-only JSONL
├── decisions.jsonl          ← Delegation decisions (who delegated to whom)
└── work/                    ← Execution workspace
```

Every line in `audit.jsonl` is one action. Every line in `decisions.jsonl` is one delegation event (agent A spawned agent B with level X for task Y).

### Cross-Session Audit

A separate `data/audit.db` (SQLite) indexes all session audits for querying:

```sql
CREATE TABLE audit_index (
    session_id   TEXT,
    agent_id     TEXT,
    action_type  TEXT,
    command      TEXT,
    exit_code    INTEGER,
    duration_ms  INTEGER,
    risk_score   INTEGER,
    timestamp    TEXT
);
```

This enables: "Show me all failed shell commands across all sessions" or "List all SemiAuto actions that were confirmed by the user."

### The Causal Chain

```
audit.jsonl record → trace_id links back to:
  decisions.jsonl   → which delegation spawned this agent
  parent audit.jsonl → what the parent agent was doing when it delegated
```

Every action is traceable: **who asked for it → who approved it → who executed it → what happened.**

---

## 4. Three-Stage Execution Flow

### Stage 1: Plan (MainAgent, ReadOnly)

```
User: "Add a dark mode toggle to the settings"

MainAgent (ReadOnly):
  1. Read App.tsx, SettingsView.tsx, index.css
  2. Analyze: needs toggle state, CSS variables, 3 file edits
  3. Build plan DAG:
     │
     ├── Task A: Add CSS variables (index.css)
     ├── Task B: Add toggle state (App.tsx)
     └── Task C: Wire toggle to SettingsView (SettingsView.tsx)
  4. Estimate: 3 files, low risk
  5. Present plan to user → user approves
```

### Stage 2: Dispatch (MainAgent → SubAgents)

```
MainAgent:
  1. Spawn CodeAgent (SemiAuto) → Task A + Task B (parallel, independent)
  2. Wait for completion
  3. Spawn CodeAgent (SemiAuto) → Task C (depends on A, B)

Each CodeAgent:
  - Reads target file
  - Edits file
  - Audit record written: { action: Write, file: index.css, agent: sub:code:xyz }
  - Returns summary: "Added 12 CSS custom properties for dark theme"
```

### Stage 3: Verify (MainAgent → ReviewAgent)

```
MainAgent:
  1. Spawn ReviewAgent (ReadOnly) → Review all 3 changes
  2. ReviewAgent finds: "Task B uses incorrect type for theme state"
  3. MainAgent spawns CodeAgent to fix → ReviewAgent re-checks → OK
  4. Present final summary to user
```

---

## 5. Implementation Phases

### Phase A: Permission Model Redesign (next)
- Replace current 5-level `PermissionLevel` with new 4-level model
- Implement confirmation gating in `SessionSandbox::execute()`
- Add `was_confirmed` field to `AuditRecord`
- SemiAuto: pattern-match dangerous commands → flag for confirmation

### Phase B: MainAgent + SubAgent Framework
- Define `AgentType` enum and `AgentRole` trait
- Implement `MainAgent` — planner + delegator (ReadOnly)
- Implement agent spawning with permission attenuation
- Sub-agent execution tokens (session-scoped, time-bounded)

### Phase C: Audit Query + Dashboard
- `audit.db` SQLite index for cross-session queries
- API: `GET /api/sessions/{id}/audit` — query audit trail
- Frontend: expandable audit view per session

### Phase D: Advanced Orchestration
- Quorum consensus for high-risk operations
- Performance-driven optimization (learn from audit history)
- Confidence-weighted result reconciliation

---

## 6. Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| MainAgent is ReadOnly | Prevents orchestrator from accidentally executing dangerous commands. All execution goes through audited sub-agents. |
| Permission narrowing | Industry standard (IETF MAD, Claude Code). Sub-agent can never have MORE power than delegator. |
| JSONL audit per session | Append-only, crash-safe, human-readable with `tail -f`. SQLite index for cross-session queries. |
| SemiAuto as default | Balances safety and usability. Most common development tasks are safe; only flagged patterns trigger confirmations. |
| Depth limit = 3 | Prevents runaway delegation chains. Matches Claude Code and AWS RAI best practices. |
| Agent tool, not function call | Sub-agents return summaries (not raw output) to keep parent context clean. Matches Claude Code subagent pattern. |

## References

- [Claude Code Permission Model](https://skywork.ai/blog/permission-model-claude-code-vs-code-jetbrains-cli/)
- [Claude Code Subagent Bug #4740](https://github.com/anthropics/claude-code/issues/4740)
- [IETF MAD Protocol draft-sato-soos-mad-02](https://datatracker.ietf.org/doc/html/draft-sato-soos-mad-02)
- [AWS Defense-in-Depth RAI Multi-Agent](https://github.com/aws-samples/sample-defense-in-depth-rai-multi-agent)
- [Stacklok Token Delegation for MCP](https://stacklok.com/blog/token-delegation-and-mcp-server-orchestration-for-multi-user-ai-systems/)
- [SecureYeoman ADR 004 — Agents & Orchestration](https://github.com/MacCracken/secureyeoman/blob/main/docs/adr/004-agents-and-orchestration.md)
