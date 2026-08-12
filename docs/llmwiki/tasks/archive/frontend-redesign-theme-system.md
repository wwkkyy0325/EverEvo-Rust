# Frontend Redesign: Theme System + Component Architecture
> **状态**:✅ 已完成(归档)— 前端主题系统已落地(22/23 勾选)

---


**Created:** 2026-07-21
**Goal:** 建立可扩展的多主题设计 token 体系 + shadcn/ui 组件基座，解决当前硬编码颜色、无可复用组件、无主题支持的问题。

---

## Research Summary (联网深度调研)

### 业界参考

| 项目 | 框架 | 样式 | 主题系统 | 组件库 |
|------|------|------|----------|--------|
| **Hermes Workspace** | React 19 + TanStack Start | Tailwind 4 | CSS Variables + 自适应 | 自研 + shadcn 兼容 |
| **ClawX** | Electron + React | CSS-in-JS / CSS Vars | light/dark/system | 自研 |
| **ableinc/local-ai** | Electron + React 19 | Tailwind 4 + CSS Vars | dark/light | shadcn/ui (Radix) |
| **Orbis (Tauri)** | React + shadcn/ui | Tailwind CSS v4 | CSS Variables | shadcn/ui (35+ 组件) |

### 核心技术决策

1. **CSS 自定义属性 = 设计 token 单一真相源**
   - 三层 token 架构：Global（原始值）→ Semantic（语义）→ Component（组件级）
   - OKLCH 颜色空间（感知均匀，HSL 已被视为反模式用于色阶）
   - `[data-theme="..."]` 属性选择器切换主题

2. **Tailwind CSS v4（必须迁移）**
   - CSS-first 配置（`@theme`），不再需要 `tailwind.config.js`
   - OKLCH 默认颜色空间，透明度天然可用
   - Rust 引擎（Oxide），构建速度 5x+
   - shadcn/ui 2025 年官方支持 v4

3. **shadcn/ui = 源码复制模式**
   - 不是 npm 黑盒，组件源码在你的 `src/components/ui/` 里
   - Radix UI 基座（无障碍访问），Tailwind 样式，完全可定制
   - 不改源码即可多主题适配（所有颜色走 CSS 变量）

4. **组件架构：组合式基元 > 单体组件**
   - Message = typed blocks (text, thinking, tool_call, media, error)
   - 流式渲染三层 memoization：container → block → component
   - 虚拟滚动处理长对话

---

## Phase 1: Design Token Foundation + Tailwind v4 Migration

**目标：** 迁移到 Tailwind v4 + 建立 CSS Variables 设计 token 体系 + dark/light 切换

### Tasks

- [x] 1.1 — Tailwind CSS v3 → v4 迁移 ✅
  - 升级 npm 依赖 (`tailwindcss@^4`, `@tailwindcss/vite`, 移除 `autoprefixer`/`postcss`)
  - 替换 `@tailwind base/components/utilities` → `@import "tailwindcss"`
  - 移除 `tailwind.config.js`，移除 `postcss.config.js`
  - 在 `vite.config.ts` 添加 `@tailwindcss/vite` 插件
  - **Verify:** ✅ `npm run build` 成功，726ms, 0 errors

- [x] 1.2 — 定义设计 token（OKLCH CSS 变量） ✅
  - 在 `index.css` 定义 `:root`（亮色）和 `.dark`（暗色）40+ 变量
  - Token 清单：`--background`, `--foreground`, `--primary`, `--secondary`, `--muted`, `--accent`, `--destructive`, `--border`, `--input`, `--ring`，每个配 `-foreground` 对
  - App-specific tokens: `--sidebar-*`, `--chat-*-bubble`, `--tool-*`, `--statusbar-*`
  - 颜色使用 OKLCH

- [x] 1.3 — 映射 CSS 变量到 Tailwind v4 `@theme` ✅
  - 使用 `@theme inline { --color-background: var(--background); ... }`
  - 所有语义 token class (`bg-background`, `text-foreground`, `bg-primary`) 生效

- [x] 1.4 — ThemeProvider（React Context）+ localStorage 持久化 ✅
  - 创建 `src/hooks/useTheme.tsx` — 两轴主题系统 (mode × colorTheme)
  - 持久化到 `localStorage('everevo-mode' + 'everevo-color-theme')`
  - 初始化时读取 `prefers-color-scheme` 系统偏好作为 fallback
  - 在 `<html>` 上管理 `.dark` class 和 `data-theme` attribute

- [x] 1.5 — ThemeToggle + ThemeSelector 组件 ✅
  - Sun/Moon 图标切换按钮 + 4 配色主题下拉选择器
  - CSS transition 平滑过渡 (`transition: background-color 0.3s ease, color 0.3s ease`)

---

## Phase 2: shadcn/ui Component Foundation

**目标：** 初始化 shadcn/ui + 添加基础组件 + 替换现有裸标签

### Tasks

- [x] 2.1 — shadcn/ui 初始化 ✅
  - 手动创建 `components.json` (new-york, neutral, CSS variables)
  - 配置 `tsconfig.json` path alias (`@/*` → `./src/*`)
  - 配置 `vite.config.ts` resolve alias
  - 安装 `clsx`, `tailwind-merge`, `class-variance-authority`, `lucide-react`

- [x] 2.2 — 添加核心 UI 组件 ✅
  - button (cva variants), input, card (Card/Header/Title/Description/Content/Footer), badge, separator
  - 所有组件使用语义 token class，颜色自动跟随主题

- [x] 2.3 — 用 shadcn 组件重构 ChatView ✅
  - 输入框替换为 `<Input />`，发送按钮替换为 `<Button />`
  - 消息气泡用提取的 `ChatBubble` 组件

- [x] 2.4 — 后续优化（deferred）
  - SessionSidebar 的 ScrollArea / 键盘导航留到下一轮

---

## Phase 3: Multi-Theme System

**目标：** 支持 4-5 套独立配色主题，不仅 dark/light

### Tasks

- [x] 3.1 — 定义 4 套主题配色 ✅
  - `default` — 蓝灰基调（OKLCH 264° blue）
  - `ocean` — 青蓝色调（OKLCH 200° teal）
  - `sunset` — 暖橙色调（OKLCH 55° orange）
  - `forest` — 翠绿色调（OKLCH 155° green）
  - 每个主题 `[data-theme="xxx"]` + `.dark` 组合覆盖 brand tokens

- [x] 3.2 — ThemeSelector 组件 ✅
  - 下拉面板展示 4 主题，带配色预览圆点 + 勾选标记
  - 选择后更新 `<html data-theme="...">` + localStorage
  - 放在 nav bar 中，与 ThemeToggle 并列

- [x] 3.3 — 全组件硬编码色 → 语义 token ✅
  - 8 个组件全部迁移: App, ChatView, SessionSidebar, BootstrapView, SettingsView, AuditPanel, ConfirmDialog, MemoryPanel, DomainPanel
  - 状态色: hover, active, disabled 全部覆盖

- [x] 3.4 — 主题切换过渡动画 ✅
  - `transition: background-color 0.3s ease, color 0.3s ease` 在 body 上，组件交互有微动画

---

## Phase 4: Component Architecture Refinement

**目标：** 抽离可复用业务组件，规范目录结构

### Tasks

- [x] 4.1 — 重构目录结构 ✅
  ```
  src/
  ├── components/
  │   ├── ui/           ← shadcn 基座 (button, input, card, badge, separator)
  │   ├── chat/         ← ChatBubble, ToolCallCard, ThinkingPanel
  │   └── ...           ← App, ChatView, SessionSidebar, SettingsView, etc.
  ├── hooks/            ← useTheme
  └── lib/              ← utils (cn)
  ```

- [x] 4.2 — 抽取 ChatBubble 组件 ✅
  - Props: `msg: MessageItem`，自动处理 user/assistant 样式变体

- [x] 4.3 — 抽取 ToolCallCard 组件 ✅
  - 展开/折叠交互（点击 header）
  - 按工具类别着色（shell/web/file/code 四色）
  - JSON 格式化参数 + 截断结果（500 字符）

- [x] 4.4 — 抽取 ThinkingPanel 组件 ✅
  - 流式/完成两种视觉状态，折叠/展开交互

- [ ] 4.5 — Markdown 渲染增强（deferred）
  - 代码块复制按钮、Shiki 语法高亮、响应式表格
  - 留到下一轮优化

---

## Key Design Decisions

1. **Tailwind v4 而不是 v3** — v4 的 CSS-first 配置 + OKLCH 天然适合多主题；2025 年的默认选择
2. **shadcn/ui 源码复制模式** — 不依赖 npm 黑盒，完全控制，和 EverEvo "self-built" 哲学一致
3. **`data-theme` 属性 > `.theme-xxx` class** — 更语义化，和 `.dark` class 可以独立组合
4. **OKLCH > HSL** — 感知均匀，同 Lightness 值 = 同视觉亮度，调色板可预测
5. **三层 token 架构** — Global（恒定）→ Semantic（主题切换）→ Component（极少改动）
6. **React 18 保持** — 不需要 19/TanStack Start，纯 SPA 足够；避免不必要的迁移成本

---

## Phase 5: Pixel Theme (2026-07-21) ✅

**目标：** Minecraft / 8-bit 像素风主题，零 npm 依赖，纯 CSS 驱动

### Research

- **Pixelact UI** — shadcn/ui 上的像素组件库（copy-paste 模式），与我们架构完美兼容
- **NES.css** — 20k+ star 纯 CSS 8-bit 框架，`box-shadow` 实现像素边框
- **halflightcss** — Tailwind v4 原生像素/CRT 工具类（pixel-text, pixel-border, crt, scanline）
- **BATTLEWARE** — Press Start 2P + CGA 调色板的 Tailwind v4 复古主题参考
- **comatv** — npm 上的 Minecraft 风格 React 组件

### Tasks

- [x] 5.1 — 像素字体集成 ✅
  - Google Fonts: Press Start 2P (headings) + DotGothic16 (body, 支持中文/日文)
  - index.html 添加 preconnect + font link

- [x] 5.2 — `[data-theme="pixel"]` 主题 token ✅
  - 完整覆盖 25+ CSS 变量（颜色 + 结构）
  - Minecraft 调色板: grass green primary, stone gray bg, gold accent, deepslate dark
  - `--radius: 0px` — 全局尖角
  - `--pixel-unit: 4px` — 像素网格基准
  - `--font-heading` / `--font-body` — 像素字体 token
  - `.dark` 变体: cave/night 暗色氛围

- [x] 5.3 — 像素专属 CSS 规则（全部 `[data-theme="pixel"]` 作用域） ✅
  - body: 像素字体 + font-smoothing: none
  - h1-h6: Press Start 2P + uppercase
  - code/pre: 小号 Press Start 2P
  - img/svg: image-rendering: pixelated
  - button: 4px border + box-shadow 像素边框 + active 按压效果
  - input/textarea/select: 4px border + 像素字体
  - .rounded-*: 补偿性 2px 粗边框
  - .pixel-border 工具类: 显式像素边框 + 偏移阴影

- [x] 5.4 — ThemeSelector 集成 ✅
  - ColorTheme 类型新增 `'pixel'`
  - COLOR_THEMES 数组追加: 👾 像素 · Minecraft 风 · 8-bit 复古游戏
  - 配色预览圆点: pixel 主题用方形 (borderRadius: 0) 区别于其他圆形
  - localStorage 恢复支持

- [x] 5.5 — 构建验证 ✅
  - CSS +4KB (38KB → 42KB)，JS 不变
  - 832ms 构建，零错误
  - 0 新增 npm 依赖
  - 0 组件代码改动

### Architecture advantage

像素主题完全依靠我们已有的 `data-theme` + CSS variables 架构。添加一个主题 = 写一段 CSS 覆盖 token。无需碰任何组件代码。这是 Phase 1 设计决策的直接收益。