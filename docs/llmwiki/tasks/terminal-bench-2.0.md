# Terminal-Bench 2.0 — EverEvo Agent 基准测试
> **状态**:🔄 进行中 — Terminal Bench 2.0

---


## 目标

用 Terminal-Bench 2.0（89 个真实终端任务）通过 Harbor 框架测试 **EverEvo Agent（整个 Agent 框架，不是裸 LLM）**，
拿到可公开对比的 pass rate 分数。

## 前置条件

- [ ] Docker Desktop 已启动且 `docker ps` 正常
- [ ] Harbor 框架已安装：`pip install harbor`（已完成 ✅）
- [ ] EverEvo 配置文件 `data/config.toml` 包含有效的 API key
- [ ] Rust 工具链已安装（用于交叉编译或容器内构建）

## 架构设计

```
┌─────────────────────────────────────────────────────────────┐
│                    Harbor Framework                          │
│                                                             │
│  ┌───────────────────────┐    ┌───────────────────────────┐ │
│  │   Terminal-Bench 2.0  │    │   EverEvo Harbor Adapter   │ │
│  │   89 terminal tasks   │───▶│   (everevo_agent.py)      │ │
│  │   Docker containers   │    │                           │ │
│  └───────────────────────┘    │  ┌─────────────────────┐  │ │
│                               │  │ everevo-server       │  │ │
│                               │  │ (Linux binary,      │  │ │
│                               │  │  running inside      │  │ │
│                               │  │  Docker container)   │  │ │
│                               │  │                     │  │ │
│                               │  │ POST /api/chat      │  │ │
│                               │  │  → Agent Loop       │  │ │
│                               │  │  → 22 tools         │  │ │
│                               │  │  → SSE response     │  │ │
│                               │  └─────────────────────┘  │ │
│                               └───────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

### 为什么要在容器内运行 everevo-server？

1. EverEvo 的 bash/read_file/write_file 工具直接在容器文件系统上操作
2. 终端任务的所有文件都在容器里，Agent 的自然执行环境就是容器
3. 完全隔离——Agent 看不到宿主机文件，确保测试公正

### 关键设计决策

| 决策 | 选择 | 原因 |
|------|------|------|
| Harbor agent 类型 | `BaseInstalledAgent` | 需要安装二进制到容器内 |
| 构建方式 | 容器内 cargo build | 避免交叉编译问题（libsqlite3-sys 需要目标平台的 cc） |
| 通信方式 | HTTP (curl → /api/chat) | EverEvo 的 SSE 接口，Agent 在容器内自循环 |
| 评分方式 | Harbor 内置 verifier | 二进制 pass/fail，100% 客观 |

## 实施步骤

### Step 1: 构建 EverEvo Linux 二进制

```powershell
# 方案 A: 使用 Docker 构建（推荐，无需交叉编译器）
docker run --rm -v ${PWD}:/build -w /build rust:1.80-slim \
    cargo build -p everevo-server --release
# 输出: target/release/everevo-server (Linux x86_64 binary)

# 方案 B: 使用 cross 工具（需要 Docker）
cargo install cross
cross build -p everevo-server --release --target x86_64-unknown-linux-gnu

# 验证二进制类型
file target/release/everevo-server
# 应输出: ELF 64-bit LSB executable, x86-64
```

**注意**: 构建前需要禁用 `onnx` feature（Terminal-Bench 不需要向量检索）：
```toml
# 临时修改 or 使用 --no-default-features
cargo build -p everevo-server --release --no-default-features
```

### Step 2: 准备配置文件

```powershell
# 创建精简配置文件给容器内使用
mkdir -p scripts/bench/everevo-config
cp data/config.toml scripts/bench/everevo-config/config.toml
# 确保 API key 已填写
```

### Step 3: 编写 Harbor Agent Adapter

文件: `scripts/everevo_harbor_agent.py`

实现:
- `EverEvoAgent(BaseInstalledAgent)`:
  - `name()` → "everevo"
  - `install(environment)`: 拷贝二进制 + 配置到容器
  - `run(instruction, environment, context)`: 启动 server → 发送任务 → 监控 SSE → 返回结果

### Step 4: 运行基准测试

```powershell
# 1. 确保 Docker Desktop 运行中
docker ps

# 2. 下载 Terminal-Bench 2.0 数据集
harbor download terminal-bench@2.0 --cache

# 3. 快速冒烟测试（只跑 1 个任务）
harbor run \
  --dataset terminal-bench@2.0 \
  --agent scripts.everevo_harbor_agent:EverEvoAgent \
  --model glm-5.2 \
  --n-tasks 1 \
  --n-concurrent 1 \
  --timeout-multiplier 2.0 \
  --debug

# 4. 全量跑（89 任务，4 并发）
harbor run \
  --dataset terminal-bench@2.0 \
  --agent scripts.everevo_harbor_agent:EverEvoAgent \
  --model glm-5.2 \
  --n-concurrent 4 \
  --timeout-multiplier 2.0 \
  --jobs-dir data/bench/terminal-bench-results

# 5. 查看结果
harbor view --jobs-dir data/bench/terminal-bench-results
```

### Step 5: 生成报告

```powershell
python scripts/terminal_bench_report.py \
  --results data/bench/terminal-bench-results \
  --output data/bench/report_terminal_bench.md
```

## 验证检查点

每个步骤后的验证：

### Step 1 验证
```powershell
# 确认二进制是 Linux ELF
file target/release/everevo-server | grep "ELF.*x86-64"

# 确认二进制大小合理（> 10MB）
ls -lh target/release/everevo-server
```

### Step 3 验证（冒烟测试）
```powershell
# 在本地启动 server，确保 /api/health 正常
cargo run -p everevo-server --release &
curl http://127.0.0.1:13456/api/health

# 发一个简单任务，确认 agent 正常响应
curl -X POST http://127.0.0.1:13456/api/chat \
  -H "Content-Type: application/json" \
  -d '{"message":"Say hello in one word"}'
# 应返回 SSE stream，最后有 event:done
```

### Step 4 验证（单任务）
```powershell
# 确认 Harbor 能启动容器、运行 agent、收集结果
harbor run --dataset terminal-bench@2.0 \
  --agent scripts.everevo_harbor_agent:EverEvoAgent \
  --model glm-5.2 --n-tasks 1 --debug

# 查看 trial 日志
ls jobs/*/trials/*/
cat jobs/*/trials/*/logs/agent/*.log
```

## 防作弊清单

- [ ] temperature=0.0（固定非随机输出）
- [ ] 跑一轮，不刷 best-of-N
- [ ] 不改 Harbor 框架代码
- [ ] 不改 Terminal-Bench 测试数据
- [ ] 公布完整 config（API endpoint 脱敏但参数透明）
- [ ] 公布每个任务的 pass/fail（不只是总分）
- [ ] 记录 token 消耗（通过 everevo-server 的 done 事件）

## 预期结果

| 指标 | 预期 |
|------|------|
| Overall Pass Rate | 待跑（取决于 LLM backend 和 Agent 工具质量） |
| Token 消耗 | 记录每个任务的 input/output tokens |
| 对比基线 | LangChain 66.5%, Claude Code ~70-80% |

## 参考

- Harbor Framework: https://github.com/harbor-framework/harbor
- Terminal-Bench 2.0: https://github.com/laude-institute/terminal-bench-2
- 数据集: `harbor download terminal-bench@2.0`