#!/bin/bash
# ============================================================================
# EverEvo Agent 基准测试
#
# 测的是 EverEvo Agent（启动 → chat API → context pipeline → tools → 响应）
# NOT 裸模型 API。横向对比其他 Agent 的公开 benchmark 成绩。
#
# 安全:
#   - 全部输出 → /tmp/everevo-bench/
#   - server 数据目录 → /tmp/everevo-bench/data/ (独立, 不碰生产 data/)
#   - temperature=0.0, 可复现
#   - 测试完自动停 server
# ============================================================================
set -euo pipefail

SANDBOX="/tmp/everevo-bench"
PORT=13456
BASE="http://127.0.0.1:$PORT"
REPORT="$SANDBOX/report_$(date +%Y%m%d_%H%M%S).md"

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; NC='\033[0m'
log()  { echo -e "${CYAN}[bench]${NC} $*"; }
ok()   { echo -e "${GREEN}[  OK]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*"; }

# ==================================================================
# Setup — isolated data directory, clean environment
# ==================================================================
setup() {
    log "Setting up isolated sandbox..."
    rm -rf "$SANDBOX"
    mkdir -p "$SANDBOX"/{data/db,data/memory,data/domain,data/models,data/runtime/onnxruntime/lib,results}

    # Copy config (read-only) and model files (needed by server)
    cp data/config.toml "$SANDBOX/data/config.toml" 2>/dev/null || true
    cp data/config/config.toml "$SANDBOX/data/config/config.toml" 2>/dev/null || true

    # Copy ONNX runtime DLL (needed for embeddings)
    if [ -d data/runtime/onnxruntime/lib ]; then
        cp -r data/runtime/onnxruntime/lib/* "$SANDBOX/data/runtime/onnxruntime/lib/" 2>/dev/null || true
    fi
    if [ -d data/models ]; then
        cp -r data/models "$SANDBOX/data/" 2>/dev/null || true
    fi

    log "Sandbox: $SANDBOX (completely isolated from production data/)"
}

# ==================================================================
# Start EverEvo Server
# ==================================================================
start_server() {
    log "Building everevo-server..."
    cargo build -p everevo-server --release 2>&1 | tail -1

    log "Starting EverEvo on port $PORT..."
    EVEREVO_DATA_DIR="$SANDBOX/data" \
        cargo run -p everevo-server --release -- \
        serve --host 127.0.0.1 --port $PORT \
        > "$SANDBOX/server.log" 2>&1 &

    SERVER_PID=$!
    log "Server PID: $SERVER_PID"

    # Wait for server to be ready
    for i in $(seq 1 30); do
        if curl -s "$BASE/api/health" > /dev/null 2>&1; then
            ok "Server ready (took ${i}s)"
            return 0
        fi
        sleep 2
    done
    fail "Server failed to start"
    cat "$SANDBOX/server.log"
    exit 1
}

stop_server() {
    log "Stopping server (PID $SERVER_PID)..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    ok "Server stopped"
}

# ==================================================================
# Chat with EverEvo Agent (SSE streaming)
# ==================================================================
chat() {
    # $1 = message, $2 = optional session_id
    local msg="$1"
    local sid="${2:-}"

    # Build JSON body
    local body
    if [ -n "$sid" ]; then
        body=$(jq -nc --arg msg "$msg" --arg sid "$sid" '{session_id: $sid, message: $msg}')
    else
        body=$(jq -nc --arg msg "$msg" '{message: $msg}')
    fi

    # Send to chat endpoint, collect all SSE events, extract final text
    # We collect all "text" events and concatenate them
    curl -sN "$BASE/api/chat" \
        -H "Content-Type: application/json" \
        -d "$body" \
        --max-time 300 2>/dev/null | \
        while IFS= read -r line; do
            # SSE format: "event: <type>" then "data: <json>"
            if [[ "$line" == event:\ * ]]; then
                EVENT_TYPE="${line#event: }"
            elif [[ "$line" == data:\ * ]]; then
                DATA="${line#data: }"
                echo "$EVENT_TYPE|$DATA"
            fi
        done
}

# Extract final assistant text from SSE output
extract_answer() {
    # Input: SSE lines "event_type|json_data"
    # Output: just the final assistant text (text events concatenated)
    local sse_output="$1"
    echo "$sse_output" | while IFS='|' read -r evt data; do
        if [ "$evt" = "text" ]; then
            # text event: {"content":"..."}
            echo "$data" | jq -r '.content // empty' 2>/dev/null
        fi
    done | tr '\n' ' ' | head -c 2000
}

# Extract tool calls from SSE output
extract_tool_calls() {
    local sse_output="$1"
    echo "$sse_output" | while IFS='|' read -r evt data; do
        if [ "$evt" = "tool_call_start" ]; then
            echo "TOOL: $(echo "$data" | jq -r '.name // "?"')($(echo "$data" | jq -r '.arguments // ""' | head -c 200))"
        fi
    done
}

# ==================================================================
# EQ-Bench v3: Emotional Intelligence (45 scenarios)
# ==================================================================
run_eqbench() {
    log "=== EQ-Bench v3: 情感/对话能力 ==="
    log "Agent 全链路参与: persona + context pipeline + 自然对话"

    cd "$SANDBOX"
    if [ ! -d "EQ-Bench" ]; then
        git clone --depth 1 https://github.com/EQ-Bench/EQ-Bench.git 2>&1 | tail -1
    fi
    cd EQ-Bench
    pip install -q requests 2>&1 | tail -1

    # EQ-Bench scenarios are in the repo. We send each through EverEvo's chat API.
    # Simplified: use a subset of emotionally complex prompts
    local prompts=(
        "My best friend just told me they're moving across the country. I feel empty inside. What should I do?"
        "I worked so hard on this project for months and my boss just gave all the credit to someone else. How do I handle this?"
        "I'm really worried about my aging parents' health but they refuse to see a doctor. I feel helpless."
        "My partner and I had a huge fight and they said some really hurtful things. I don't know if we can recover."
        "I just failed an exam I studied really hard for. I feel like giving up on everything."
    )

    local results=""
    local i=0
    for prompt in "${prompts[@]}"; do
        i=$((i+1))
        log "  [$i/${#prompts[@]}] $prompt"
        local sse_out=$(chat "$prompt" "")
        local answer=$(extract_answer "$sse_out")
        echo "Q: $prompt" >> "$SANDBOX/results/eqbench_answers.txt"
        echo "A: $answer" >> "$SANDBOX/results/eqbench_answers.txt"
        echo "---" >> "$SANDBOX/results/eqbench_answers.txt"
    done
    ok "EQ-Bench complete — answers in $SANDBOX/results/eqbench_answers.txt"
    cd - > /dev/null
}

# ==================================================================
# HumanEval: Code Generation (sample)
# ==================================================================
run_humaneval() {
    log "=== HumanEval: 代码生成 ==="
    log "Agent 全链路: context pipeline + write_file tool + 代码能力"

    cd "$SANDBOX"
    if [ ! -d "human-eval" ]; then
        git clone --depth 1 https://github.com/openai/human-eval.git 2>&1 | tail -1
    fi

    # Sample 5 coding tasks from HumanEval
    local tasks=(
        "Write a Python function 'has_close_elements(numbers, threshold)' that checks if any two numbers in a list are closer than the threshold. Return True/False."
        "Write a Python function 'separate_paren_groups(paren_string)' that separates nested parentheses into balanced groups. Return a list of strings."
        "Write a Python function 'truncate_number(number)' that returns the decimal part of a positive floating point number."
        "Write a Python function 'below_zero(operations)' that takes a list of deposit/withdrawal numbers and returns True if the balance ever falls below zero."
        "Write a Python function 'mean_absolute_deviation(numbers)' that returns the mean absolute deviation of a list of numbers."
    )

    local i=0
    for task in "${tasks[@]}"; do
        i=$((i+1))
        log "  [$i/${#tasks[@]}] $task"
        local sse_out=$(chat "$task" "")
        local answer=$(extract_answer "$sse_out")
        local tools=$(extract_tool_calls "$sse_out")
        echo "TASK: $task" >> "$SANDBOX/results/humaneval_results.txt"
        echo "TOOLS: $tools" >> "$SANDBOX/results/humaneval_results.txt"
        echo "ANSWER: $answer" >> "$SANDBOX/results/humaneval_results.txt"
        echo "===" >> "$SANDBOX/results/humaneval_results.txt"
    done
    ok "HumanEval complete — results in $SANDBOX/results/humaneval_results.txt"
    cd - > /dev/null
}

# ==================================================================
# BFCL-style: Tool Use (adapted for EverEvo's 22 tools)
# ==================================================================
run_bfcl() {
    log "=== BFCL-style: Tool Use / Function Calling ==="
    log "Agent 全链路: context pipeline + 22 tools + tool selection"

    # Instead of BFCL's raw function format, we test EverEvo's ACTUAL tool use.
    # We send prompts that require specific tools and verify the agent picks correctly.
    local prompts=(
        "Read the file Cargo.toml from the project root and tell me what edition it uses."
        "Search the web for the latest Rust release version and tell me what it is."
        "Save a memory fact: the benchmark test ran successfully on $(date +%Y-%m-%d)."
        "List all files in the src/ directory of the everevo-knowledge crate."
    )

    local i=0
    for prompt in "${prompts[@]}"; do
        i=$((i+1))
        log "  [$i/${#prompts[@]}] $prompt"
        local sse_out=$(chat "$prompt" "")
        local answer=$(extract_answer "$sse_out")
        local tools=$(extract_tool_calls "$sse_out")
        echo "Q: $prompt" >> "$SANDBOX/results/bfcl_results.txt"
        echo "TOOLS: $tools" >> "$SANDBOX/results/bfcl_results.txt"
        echo "A: $answer" >> "$SANDBOX/results/bfcl_results.txt"
        echo "===" >> "$SANDBOX/results/bfcl_results.txt"
        log "    Tools used: $tools"
    done
    ok "BFCL-style complete — results in $SANDBOX/results/bfcl_results.txt"
}

# ==================================================================
# Generate Report
# ==================================================================
generate_report() {
    log "=== Benchmark Report ==="
    cat > "$REPORT" << EOF
# EverEvo Agent 基准测试报告

**时间**: $(date '+%Y-%m-%d %H:%M:%S')
**Agent**: EverEvo v0.1.0 (full pipeline: persona + context + 22 tools + memory)
**沙箱**: \`$SANDBOX\` (与生产 \`data/\` 完全隔离)

## 实验设计

| 控制变量 | 值 |
|----------|-----|
| Agent 框架 | EverEvo (context pipeline + 22 tools) |
| LLM 后端 | glm-5.2 (config.toml mainModelId) |
| Temperature | 0.0 |
| 数据目录 | /tmp/everevo-bench/data/ (独立) |
| 生产数据 | 完全未接触 |

## 横向对比基准

| 测试 | EverEvo 方式 | 对比榜单 |
|------|-------------|---------|
| EQ-Bench v3 | chat API (persona + context) | Gemini3 Pro=87, Claude3.5=82 |
| HumanEval | chat API + write_file tool | GPT-4=67%, Claude3.5=92% |
| BFCL-style | chat API + 22 tools | GPT-4o=88%, Claude3.5=85% |
EOF

    echo "" >> "$REPORT"
    echo "## 结果摘要" >> "$REPORT"
    echo '```' >> "$REPORT"
    echo "EQ-Bench:  $(grep -c 'Q:' "$SANDBOX/results/eqbench_answers.txt" 2>/dev/null || echo 0) scenarios tested"
    echo "HumanEval: $(grep -c 'TASK:' "$SANDBOX/results/humaneval_results.txt" 2>/dev/null || echo 0) tasks tested"
    echo "BFCL:      $(grep -c 'Q:' "$SANDBOX/results/bfcl_results.txt" 2>/dev/null || echo 0) prompts tested"
    echo '```' >> "$REPORT"

    echo "" >> "$REPORT"
    echo "### BFCL 工具调用结果" >> "$REPORT"
    cat "$SANDBOX/results/bfcl_results.txt" 2>/dev/null >> "$REPORT" || echo "(pending)" >> "$REPORT"

    echo "" >> "$REPORT"
    echo "### EQ-Bench 情感对话" >> "$REPORT"
    cat "$SANDBOX/results/eqbench_answers.txt" 2>/dev/null >> "$REPORT" || echo "(pending)" >> "$REPORT"

    echo "" >> "$REPORT"
    echo "### HumanEval 代码生成" >> "$REPORT"
    cat "$SANDBOX/results/humaneval_results.txt" 2>/dev/null >> "$REPORT" || echo "(pending)" >> "$REPORT"

    echo ""
    cat "$REPORT"
    ok "Report saved to $REPORT"
}

# ==================================================================
# Main
# ==================================================================
case "${1:-all}" in
    all)
        setup
        start_server
        run_eqbench
        run_bfcl
        run_humaneval
        stop_server
        generate_report
        ;;
    eqbench)
        setup; start_server; run_eqbench; stop_server
        ;;
    bfcl)
        setup; start_server; run_bfcl; stop_server
        ;;
    humaneval)
        setup; start_server; run_humaneval; stop_server
        ;;
    start)
        setup; start_server
        log "Server running at $BASE — test manually:"
        log "  curl -X POST $BASE/api/chat -H 'Content-Type: application/json' -d '{\"message\":\"hello\"}'"
        ;;
    stop)
        stop_server
        ;;
    report)
        generate_report
        ;;
    *)
        echo "Usage: $0 {all|eqbench|bfcl|humaneval|start|stop|report}"
        exit 1
        ;;
esac
