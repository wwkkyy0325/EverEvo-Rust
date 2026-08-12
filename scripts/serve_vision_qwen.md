# 启动本地视觉模型(qwen3-vl-2b / llama.cpp)

EverEvo 的 `describe_image` 工具通过 **OpenAI 兼容端点**调用一个**专用视觉模型**。
这里用 llama.cpp 的 `llama-server` 跑 qwen3-vl-2b(双文件:LLM GGUF + 视觉编码器 `--mmproj`)。

> **显存红线(6GB GPU):上下文必须 `-c 32768`(32K)或更小**,否则显存会爆。
> 对应 EverEvo 里该 provider 的 `context_window` 也应填 `32768`。

## 1. 需要的两个文件(从 HuggingFace 下载)

| 文件 | 用途 | 典型大小 |
|------|------|---------|
| `Qwen3VL-2B-Instruct-Q4_K_M.gguf` | LLM 权重 | ~1.2 GB |
| `mmproj-Qwen3VL-2B-Instruct-F16.gguf` | 视觉编码器 | ~0.5 GB |

来源(任选一个 repo):
- https://huggingface.co/kurtinau/Qwen3-VL-2B-Instruct-GGUF
- https://huggingface.co/mradermacher/Qwen3-VL-2B-Instruct-GGUF

把两个文件放进一个目录,比如 `D:\models\qwen3-vl-2b\`。

## 2. 启动命令(可复制)

```powershell
# 把下面的路径替换成你的实际路径
$LLM      = "D:\models\qwen3-vl-2b\Qwen3VL-2B-Instruct-Q4_K_M.gguf"
$MMPROJ   = "D:\models\qwen3-vl-2b\mmproj-Qwen3VL-2B-Instruct-F16.gguf"
$LLAMA    = "D:\llama.cpp\build\bin\Release\llama-server.exe"  # 或你编译/下载的 llama-server

& $LLAMA -m $LLM --mmproj $MMPROJ `
    -c 32768 -ngl 99 --port 8080 --host 127.0.0.1 `
    --alias qwen3-vl-2b
```

Linux/macOS:

```bash
llama-server -m /path/to/Qwen3VL-2B-Instruct-Q4_K_M.gguf \
    --mmproj /path/to/mmproj-Qwen3VL-2B-Instruct-F16.gguf \
    -c 32768 -ngl 99 --port 8080 --host 127.0.0.1 \
    --alias qwen3-vl-2b
```

### 参数说明

| 参数 | 含义 | 说明 |
|------|------|------|
| `-m <llm.gguf>` | LLM 权重 | 主模型文件 |
| `--mmproj <mmproj.gguf>` | 视觉编码器 | **必须**,否则不支持图片 |
| `-c 32768` | 上下文长度 | **红线:≤ 32768**,防显存溢出 |
| `-ngl 99` | GPU 层数 | 全部卸载到 GPU;显存不足可调小 |
| `--port 8080 --host 127.0.0.1` | 监听地址 | 只监听本机 |
| `--alias qwen3-vl-2b` | 服务名 | 便于日志识别 |

## 3. 在 EverEvo 里配置

在「设置 → 大语言模型」新增一个 provider:

- **api_format**:`openai`
- **api_key**:留空(本地无鉴权)
- **base_url**:`http://127.0.0.1:8080/v1`
- **model**:`qwen3-vl-2b-instruct`
- **context_window**:`32768`

再在「设置 → 路由」把 **视觉模型** 选成这个 provider。
启动检查会在每次启动时校验:若视觉已配置但 `context_window` 超过 32K,会给出警告。

## 4. 冒烟验证

```powershell
data\bench\venv\Scripts\python.exe scripts\vision_smoke.py
```

- llama-server 未启动 → 打印原因并以 exit 2 退出。
- 已启动 → 用 GAIA q17(棋图)与 q22(分数图)各发一次描述请求并打印结果。

## 5. 常见问题

- **`--mmproj` 报错/无法识别图片**:版本过旧的 llama.cpp 不支持,升级到最新 release。
- **显存不够**:把 `-ngl` 调小(如 `-ngl 20`),让部分层跑 CPU。
- **启动后 describe_image 仍回退离线脚本**:确认路由里「视觉模型」已选,且
  `context_window` ≤ 32768。
