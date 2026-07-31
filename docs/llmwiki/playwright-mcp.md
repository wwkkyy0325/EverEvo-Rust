# Playwright MCP — Browser Automation + Vision

EverEvo 通过 MCP 基础设施接入微软官方 [Playwright MCP](https://playwright.dev/mcp/introduction)，
让 agent 获得真实浏览器能力：导航、点击、填表、执行 JS、accessibility snapshot、**截图识图**。
这是绕过反爬（如 DuckDuckGo 封数据中心 IP）最稳的方式——真实浏览器指纹 + 用户系统代理/cookie。

## 能力（配置后自动注入 agent 工具列表）

| 工具 | 作用 |
|------|------|
| `browser_navigate` | 打开 URL |
| `browser_click` / `browser_type` | 点击元素、填表 |
| `browser_evaluate` | 在页面执行任意 JS |
| `browser_snapshot` | 返回 accessibility 树（结构化文本，token 省） |
| `browser_screenshot` | 截图 → 作为 image block 喂给 vision LLM |
| network 拦截 / cookie 持久 / pdf | 共 40+ 工具 |

## 启用步骤

### 1. 开启 MCP server 配置
编辑 `data/config/config.toml`，取消注释（或新增）playwright 段：

```toml
[[mcp_servers]]
name = "playwright"
transport = "stdio"
command = "npx"
args = ["-y", "@playwright/mcp@latest"]
```

EverEvo 启动时自动加载 `[[mcp_servers]]`（`AppConfig::load` → `load_mcp_servers`），
并把 bootstrapped 的 Node runtime（`data/runtime/node`）prepend 到子进程 PATH，
所以 `npx` 即使在干净机器上也能找到。

### 2. 安装浏览器（一次性）
首次使用前，在 EverEvo 的 shell 工具里跑（sandbox PATH 已有 Node）：

```
npx playwright install chromium
```

可选：把浏览器缓存指向固定位置，经 `env` 注入：
```toml
[[mcp_servers]]
name = "playwright"
command = "npx"
args = ["-y", "@playwright/mcp@latest"]
env = { "PLAYWRIGHT_BROWSERS_PATH" = "F:/workspace-new/wwkkyy0325/EverEvo-Rust/data/playwright-browsers" }
```

### 3. 验证
- `GET /api/health` → `mcp_servers` 含 `playwright`
- 让 agent `browser_navigate https://example.com` + `browser_snapshot`

## 截图识图（vision）
用支持视觉的模型（如 Claude）时，`browser_screenshot` 返回的截图会作为 image content block
直接喂给 LLM：
- **Anthropic**：`tool_result.content` 为 `[text, image base64 block]` 数组
- **OpenAI 兼容**：tool 消息后追加一条 `image_url` user 消息

截图**只在当前对话轮喂给 LLM，不持久化到 DB**（避免撑爆 content_hash），刷新会话后历史
截图不回放——这是设计取舍，截图时效性强。

## 故障排查
- **MCP server connection failed**：`npx` 找不到 → 确认 `data/runtime/node` 已 bootstrap
  （启动日志 `All 8 assets ready`）；或 host 装了 Node。
- **browser 工具调用报错 "Executable doesn't exist"**：没跑 `npx playwright install chromium`。
- **`@playwright/mcp` 首次下载慢**：`npx -y` 会从 npm 拉，属正常。
