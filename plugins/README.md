# EverEvo Plugin Fleet

Independent MCP servers that extend the kernel's capabilities.
Each plugin is a standalone binary compiled from its own crate.

## Architecture

```
plugins/
├── tools/          ← Tool plugins (MCP tools/call)
│   ├── web_search/ ← Complete example
│   ├── memory/     ← Memory operations
│   └── ...         ← 20+ more tools
├── stages/         ← ContextStage plugins (MCP prompts/get)
│   └── ...
├── hooks/          ← ToolHook plugins (MCP tools/call with hook context)
│   └── ...
└── registry.toml   ← Per-plugin version + canary config
```

## Creating a New Plugin

### 1. Copy the template

```bash
cp -r tools/web_search tools/my_tool
```

### 2. Edit Cargo.toml

```toml
[package]
name = "plugin-my-tool"
version = "1.0.0"
edition = "2021"

[[bin]]
name = "plugin-my-tool"

[dependencies]
serde = "1"
serde_json = "1"
```

### 3. Edit src/main.rs

Replace the tool logic in `execute_search()` with your tool's logic.
The MCP server framework (JSON-RPC over stdin/stdout) stays the same.

### 4. Build

```bash
cd plugins && cargo build -p plugin-my-tool --release
```

Output: `target/release/plugin-my-tool.exe`

### 5. Deploy

```bash
cp target/release/plugin-my-tool.exe \
   data/plugins/my_tool/versions/v1.0.0/plugin.exe
```

Then update `data/plugins/my_tool/registry.toml`.

## Protocol

Each plugin speaks MCP (Model Context Protocol) over stdin/stdout:
- JSON-RPC 2.0 framing: one JSON object per line (NDJSON)
- Supported methods: `initialize`, `tools/list`, `tools/call`, `ping`
- stderr is reserved for diagnostics only

## Self-Repair

If a plugin is broken, the kernel's bootstrap tools are always available:
- `shell` → git checkout + cargo build
- `read_file` / `write_file` → inspect and fix source
- `plugin_status` → diagnose which plugin is broken
- `plugin_rollback` → emergency rollback to stable

## Version Management

```
data/plugins/my_tool/
├── versions/
│   ├── v1.0.0/plugin.exe + checksum.sha256
│   └── v1.0.1/plugin.exe + checksum.sha256
├── registry.toml          ← active version + metrics
└── stable → versions/v1.0.0 (symlink)
```

## Agent Self-Modification

The agent can:
1. Read plugin source: `read_file("plugins/tools/my_tool/src/main.rs")`
2. Modify the code: `write_file(...)`
3. Build: `shell("cd plugins && cargo build -p plugin-my-tool --release")`
4. Stage: `cp target/release/plugin-my-tool.exe data/plugins/my_tool/versions/v1.0.1/`
5. Canary: `plugin_status` → set canary at 10% traffic
6. Observe: kernel auto-promotes or auto-rolls back based on metrics

On build failure, the plugin source is auto-reverted via `git checkout`.
