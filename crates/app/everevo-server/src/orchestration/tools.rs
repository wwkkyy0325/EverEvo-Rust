//! Tool registry assembly — MCP-first architecture with in-process fallback.
//!
//! ## Loading order (each phase overrides previous via HashMap::insert):
//!
//! ```text
//! [1] Bootstrap tools      ← kernel-compiled, never removable (shell, plugin_status, ...)
//! [2] MCP plugin auto-load ← 21 plugins: 13 tools + 5 stages + 3 hooks
//! [3] External MCP servers ← user-configured MCP servers via config
//! [4] In-process fallback   ← only registered when MCP didn't provide (web_search, etc.)
//! [5] Stateful in-process   ← always registered (memory, todo, plan, skill, task, workflow)
//! [6] Tool hooks            ← automatic pre/post gates (MCP hooks are callable tools)
//! [7] Plan mode filter      ← removes write tools when in plan mode
//! ```
//!
//! ## MCP vs In-Process
//!
//! | Category | MCP plugin | In-process fallback | Why |
//! |----------|-----------|-------------------|-----|
//! | web_search, web_fetch | ✅ primary | if MCP missing | Stateless HTTP |
//! | verify, compact | ✅ primary | if MCP missing | Stateless validation |
//! | code_search, code_map | ✅ primary | if MCP missing | FS scanning |
//! | list_dir, read/write_file | ✅ primary | if MCP missing | FS I/O |
//! | memory, todo, plan, skill | ❌ | **always** | Deep kernel integration (db, kg, state) |
//! | task, workflow, team | ❌ | **always** | Sub-agent orchestration |
//! | review/audit/reflect hooks | callable tools | **always** | Auto-gate vs on-demand call |

use crate::app_state::{AppState, AskNotification, ConfirmationNotification};
use crate::session_store::ServerSessionStore;
use everevo_agent::subagent_context::SubAgentContext;
use everevo_agent::tools::builtins::{
    AskUserTool, PipelineTool, ProblemModelTool, SandboxedShellTool, WebSearchDelegateTool,
};
use everevo_agent::tools::builtins::{SubAgentHandle, SubAgentStatus};
use everevo_core::tool::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Compute the workspace root from CARGO_MANIFEST_DIR at compile time.
///
/// `everevo-server` lives at `crates/app/everevo-server/`, so we go up 3
/// levels to reach the project root.  This is stable regardless of how the
/// server is launched (Tauri, cargo run, direct binary) — unlike `current_dir()`
/// which changes when Tauri sets its own cwd.
fn project_root_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/app
        .and_then(|p| p.parent()) // crates
        .and_then(|p| p.parent()) // project root
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Result from assembling the per-session tool registry.
pub struct AssembledTools {
    pub tools: Arc<ToolRegistry>,
    pub pending: Arc<std::sync::atomic::AtomicUsize>,
    pub subagent_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    pub results_backlog: Arc<std::sync::Mutex<Vec<(String, String, String)>>>, // (id, description, result)
    pub task_handles: Arc<std::sync::Mutex<Vec<SubAgentHandle>>>,
    pub task_statuses: Arc<std::sync::Mutex<Vec<SubAgentStatus>>>,
    /// Shared focus channel: CompactTool writes → AgentLoop's autocompact reads.
    pub compact_focus: Arc<std::sync::Mutex<Option<String>>>,
    /// Hook feedback slot: ReflectGateHook writes → AgentLoop reads after tool execution.
    pub hook_feedback: Arc<std::sync::Mutex<Option<String>>>,
}

/// Assemble the full per-session 11-tool registry.
///
/// Covers shell, download, bootstrap, memory, TodoWrite, EnterPlan, ExitPlan,
/// Skill, Verify, Task (with sub-agents), and Workflow.
pub async fn assemble(
    state: &Arc<AppState>,
    session_id: Uuid,
    client: &Arc<everevo_agent::llm::HttpClient>,
    notif_tx: &mpsc::UnboundedSender<ConfirmationNotification>,
    ask_user_tx: &mpsc::UnboundedSender<AskNotification>,
    permission_level: &str,
    sub_ctx: &SubAgentContext,
) -> AssembledTools {
    let mut registry = ToolRegistry::new();
    let is_fully_auto = permission_level == "全自动" || permission_level == "fully_auto";

    // Shared focus channel: CompactTool writes, AgentLoop's autocompact reads
    let compact_focus = Arc::new(std::sync::Mutex::new(None::<String>));

    // Plan mode state: shared between EnterPlanModeTool/ExitPlanModeTool and chat route
    let plan_state: everevo_agent::tools::builtins::PlanModeState =
        Arc::clone(&state.plan_mode_sessions);
    // Check if session is in plan mode — write tools will be filtered
    let is_plan_mode = {
        let ps = state.plan_mode_sessions.read().await;
        ps.contains_key(&session_id)
    };

    // ── Bootstrap tools (kernel-built, never removable) ──
    // These guarantee self-repair: shell, read_file, write_file,
    // plugin_status, plugin_rollback are always available.
    // Use CARGO_MANIFEST_DIR (compile-time constant, always accurate)
    // instead of current_dir() which varies by launcher (Tauri → src-tauri/).
    let project_root = project_root_dir();
    let plugins_source_dir = project_root.join("plugins");
    // Read the sandbox work_dir ONCE so:
    // (a) read_file/write_file resolve relative paths against it
    // (b) SandboxedShellTool uses it as cwd
    // (c) DownloadTool is scoped to it
    // Without this, relative paths resolve against the server's own cwd
    // (src-tauri/ under Tauri), which breaks file tools.
    let session_work_dir = {
        let sandboxes = state.sandboxes.read().await;
        sandboxes.get(&session_id).map(|sb| sb.work_dir().clone())
    };
    everevo_kernel::bootstrap::register_all(
        &mut registry,
        Some(Arc::clone(&state.plugin_registry)),
        Some(plugins_source_dir.clone()),
        session_work_dir.clone(),
    );

    // ── MCP Plugin auto-loading: spawn plugin binaries, register their tools ──
    // Runs BEFORE in-process tools so MCP tools are the canonical versions.
    // In-process tools below serve as fallback (only registered if MCP didn't provide them).
    {
        let target_dir = project_root.join("target").join("release");
        let search_dirs = [
            target_dir,
            project_root.join("plugins").join("target").join("release"),
        ];

        // NOTE: plan_mode is intentionally NOT auto-loaded as an MCP plugin —
        // plugin-plan-mode's `enter_plan_mode`/`exit_plan_mode` run in a separate
        // process and cannot write the shared `PlanModeState` the chat route's
        // write-tool filter reads, so they are a no-op that misleads the agent
        // into believing write tools are blocked. The in-process
        // EnterPlanMode/ExitPlanMode (registered below, step [5]) are the single
        // functional track (plan-mode merge, 2026-08-13).
        let tool_plugins = [
            ("web_search", "plugin-web-search.exe"),
            ("web_fetch", "plugin-web-fetch.exe"),
            ("memory", "plugin-memory.exe"),
            ("code_search", "plugin-code-search.exe"),
            ("list_dir", "plugin-list-dir.exe"),
            ("read_file", "plugin-read-file.exe"),
            ("write_file", "plugin-write-file.exe"),
            ("download", "plugin-download.exe"),
            ("verify", "plugin-verify.exe"),
            ("compact", "plugin-compact.exe"),
            ("todo_write", "plugin-todo-write.exe"),
            ("skill", "plugin-skill.exe"),
        ];

        let hook_plugins = [
            ("audit_hook", "plugin-hooks-audit.exe"),
            ("review_gate", "plugin-hooks-review_gate.exe"),
            ("reflect_gate", "plugin-hooks-reflect_gate.exe"),
        ];

        let stage_plugins = [
            ("best_practices", "plugin-stage-best-practices.exe"),
            ("persona", "plugin-stage-persona.exe"),
            ("skill_stage", "plugin-stages-skill_stage.exe"),
            ("memory_stage", "plugin-stages-memory_stage.exe"),
            ("domain", "plugin-stages-domain.exe"),
        ];

        for (plugin_id, exe_name) in tool_plugins
            .iter()
            .chain(hook_plugins.iter())
            .chain(stage_plugins.iter())
        {
            // Benchmark mode (EVEREVO_BENCHMARK=1): skip the MCP write_file
            // plugin — its relative paths resolve against the server CWD (repo
            // root) with no checks, which could pollute the host. The bootstrap
            // write_file (work_dir-relative + kernel-protected) is used instead.
            if std::env::var("EVEREVO_BENCHMARK").is_ok() && *plugin_id == "write_file" {
                continue;
            }
            let path = search_dirs
                .iter()
                .map(|d| d.join(exe_name))
                .find(|p| p.exists());
            if let Some(path) = path {
                match everevo_mcp::McpClient::connect_stdio(
                    &path.to_string_lossy(),
                    &[],
                    &std::collections::HashMap::new(),
                )
                .await
                {
                    Ok(client) => {
                        let client = Arc::new(tokio::sync::Mutex::new(client));
                        let defs = { client.lock().await.tools.clone() };
                        let count = defs.len();
                        if count > 0 {
                            let plugin_tools =
                                everevo_mcp::McpTool::from_defs(Arc::clone(&client), &defs);
                            for tool in plugin_tools {
                                registry.register(tool);
                            }
                            tracing::info!(%plugin_id, count, "MCP plugin loaded");
                        } else {
                            tracing::debug!(%plugin_id, "MCP plugin loaded (no tools exposed)");
                        }
                    }
                    Err(e) => {
                        tracing::debug!(%plugin_id, error=%e, "MCP plugin spawn failed");
                    }
                }
            }
        }
    }

    // ── web_search_local delegate ──
    // Replace the plugin's cn.bing/Sogou chain with a native server-side web
    // search through the first Anthropic-format provider (DeepSeek etc.) when
    // one is configured. Same tool name → ToolRegistry::register replaces the
    // plugin version; research_search from the plugin stays untouched.
    if let Some(ws) = state.web_search_llm.read().await.as_ref().cloned() {
        registry.register(Arc::new(WebSearchDelegateTool { llm: ws.client }));
        tracing::info!("web_search_local → delegated to native web-search provider");
    }

    // ── Per-session store (P1.1: session-stateful tools live in the agent
    // crate and depend on the SessionStore seam; the server implements it). ──
    let store: Arc<dyn everevo_agent::tools::session_store::SessionStore> =
        Arc::new(ServerSessionStore::new(
            Arc::clone(state),
            session_id,
            ask_user_tx.clone(),
            notif_tx.clone(),
            // Headless/fully_auto: never block a human.
            is_fully_auto || std::env::var("EVEREVO_BENCHMARK").is_ok(),
            is_fully_auto,
        ));

    // Reuse the work_dir from the sandbox (already read above).
    // Register sandbox-scoped shell and download tools.
    if let Some(ref work_dir) = session_work_dir {
        let sandboxes = state.sandboxes.read().await;
        if let Some(sb) = sandboxes.get(&session_id) {
            let shell = Arc::new(SandboxedShellTool {
                inner: sb.provider(),
                work_dir: work_dir.clone(),
                session_id,
                store: Arc::clone(&store),
            });
            registry.register(shell);

            // Download tool scoped to sandbox work_dir
            let mut dl =
                everevo_agent::tools::builtins::DownloadTool::new(state.downloader.clone());
            dl = dl.with_work_dir(work_dir.clone());
            registry.register(Arc::new(dl));
        }
    }

    // ── Global tools ──
    // ask_user: blocks the MAIN loop until the user replies (Claude Code style).
    // Never registered for sub-agents (base_for_task/base_for_workflow below) —
    // only the main loop may block on a human. Headless/fully_auto short-circuits.
    registry.register(Arc::new(AskUserTool {
        session_id,
        store: Arc::clone(&store),
    }));
    // problem_model: session-scoped structural causal draft for HARD questions.
    // Main loop only — sub-agents don't need to modify the parent's problem model.
    registry.register(Arc::new(ProblemModelTool {
        session_id,
        store: Arc::clone(&store),
    }));
    // pipeline: tool-callable context pipeline (selective reuse / self-assembly).
    // Main loop only — sub-agents keep their pre-built registries.
    registry.register(Arc::new(PipelineTool));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::BootstrapTool::new(state.bootstrap.clone()),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::MemoryTool::new(state.fact_manager.clone())
            .with_db(state.db.clone())
            .with_kg(state.knowledge_graph.clone())
            // 分层记忆: tag saved facts with this session so recall is isolated.
            .with_session_id(Some(session_id)),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::TodoWriteTool::new(state.todo_store.clone())
            .with_persistence(state.config.data_dir.join("tasks"))
            .with_session_id(session_id),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::EnterPlanModeTool::new(
            Arc::clone(&plan_state),
            session_id,
            state.config.data_dir.clone(),
        ),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::ExitPlanModeTool::new(
            Arc::clone(&plan_state),
            session_id,
            state.config.data_dir.clone(),
        ),
    ));
    registry.register(Arc::new(everevo_agent::tools::builtins::SkillTool::new(
        state.skill_registry.clone(),
    )));
    // Stateful tools: always in-process because MCP plugins lack kernel integration.
    // memory → needs FactManager + DB + KG
    // todo_write → needs persistent store + session state
    // plan_mode → needs shared PlanModeState across sessions
    // skill → needs SkillRegistry
    // compact → needs focus channel (AgentLoop autocompact)
    // code_search/code_map → needs background FS indexing
    // download → needs session-scoped work_dir
    registry.register(Arc::new(
        everevo_agent::tools::builtins::CompactTool::new()
            .with_compact_focus(Arc::clone(&compact_focus))
            .with_dreaming_engine(state.dreaming_engine.clone()),
    ));
    // ── describe_image — dedicated vision model for image processing ──
    // Primary: routing.visionModelId (a separate [[llm]] entry, e.g. qwen3-vl-2b).
    // Fallback: deterministic offline scripts (chess_fen.py / fractions_ocr.py).
    let vision_llm = state
        .vision_llm
        .read()
        .await
        .as_ref()
        .map(|v| Arc::clone(&v.client) as Arc<dyn everevo_core::llm::LlmProvider>);
    let tooltest_dir = {
        let p = state.config.data_dir.join("bench").join("tooltest");
        if p.is_dir() {
            Some(p)
        } else {
            None
        }
    };
    registry.register(Arc::new(
        everevo_agent::tools::builtins::DescribeImageTool::new(vision_llm, tooltest_dir),
    ));
    // ── tool_cache_read — re-read paged tool outputs from disk (stateless) ──
    registry.register(Arc::new(
        everevo_agent::tools::builtins::ToolCacheReadTool::new(),
    ));
    // TeamTool needs LLM + tools wired in later via with_llm()/with_base_tools()
    // ── Team tool — wired with LLM + tools for real sub-agent dispatch ──
    // ── WorkflowRunner with real callbacks ──
    let wf_tool = {
        let sandbox_provider = state
            .sandboxes
            .read()
            .await
            .get(&session_id)
            .map(|sb| sb.provider());
        if let Some(sandbox) = sandbox_provider {
            // Build base_for_workflow before RealCallbacks so agent_run can use it
            let wf_tools = Arc::new(registry.subset(&["shell", "memory"]));
            let wf_llm = Arc::clone(client);
            let wf_sub_ctx = sub_ctx.clone();
            let wf_cancel = tokio_util::sync::CancellationToken::new();

            struct RealCallbacks {
                sandbox: Arc<dyn everevo_core::sandbox::SandboxProvider>,
                facts: Arc<everevo_agent::memory::facts::FactManager>,
                http: reqwest::Client,
                llm: Arc<everevo_agent::llm::HttpClient>,
                tools: Arc<ToolRegistry>,
                sub_ctx: SubAgentContext,
                cancel: tokio_util::sync::CancellationToken,
            }
            #[async_trait::async_trait]
            impl everevo_workflow::WorkflowCallbacks for RealCallbacks {
                async fn shell_exec(
                    &self,
                    cmd: &str,
                    wd: Option<&str>,
                ) -> Result<(String, String, i32), String> {
                    let mut config = everevo_core::sandbox::ExecutionConfig::new(cmd);
                    if let Some(dir) = wd {
                        config = config.with_working_dir(std::path::PathBuf::from(dir));
                    }
                    let result = self
                        .sandbox
                        .execute(&config)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok((result.stdout, result.stderr, result.exit_code))
                }
                async fn fetch_url(&self, url: &str) -> Result<String, String> {
                    self.http
                        .get(url)
                        .send()
                        .await
                        .map_err(|e| format!("fetch: {e}"))?
                        .text()
                        .await
                        .map_err(|e| format!("read: {e}"))
                }
                async fn memory_save(&self, key: &str, content: &str) -> Result<(), String> {
                    let fact = everevo_core::memory::MemoryFact {
                        name: key.into(),
                        description: String::new(),
                        content: content.into(),
                        fact_type: everevo_core::memory::FactType::Project,
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                        projection: everevo_core::memory::ProjectionMetadata::new(
                            "2.0.0",
                            "workflow",
                            vec![],
                            0.8,
                        ),
                        links: vec![],
                        // Workflow recipes are cross-session reusable — global tier.
                        session: Some("global".into()),
                    };
                    self.facts.save(&fact).map_err(|e| e.to_string())
                }
                async fn memory_search(&self, query: &str) -> Result<Vec<String>, String> {
                    let facts = self.facts.load_all().map_err(|e| e.to_string())?;
                    Ok(facts
                        .iter()
                        .filter(|f| f.name.contains(query) || f.content.contains(query))
                        .map(|f| format!("[{}] {}", f.name, f.description))
                        .collect())
                }
                async fn agent_run(
                    &self,
                    prompt: &str,
                    max_turns: usize,
                ) -> Result<String, String> {
                    let system_prompt = self.sub_ctx.build_system_prompt(prompt);
                    let messages = vec![
                        everevo_core::llm::LlmMessage::system(&system_prompt),
                        everevo_core::llm::LlmMessage::user(prompt),
                    ];
                    let mt = if max_turns == 0 { 3 } else { max_turns };
                    let llm: Arc<dyn everevo_core::LlmProvider> = self.llm.clone();
                    let result = everevo_agent::AgentLoop::new()
                        .with_max_turns(mt)
                        .run_subagent(llm, Arc::clone(&self.tools), messages, self.cancel.clone())
                        .await;
                    Ok(result)
                }
            }
            let cb: Arc<dyn everevo_workflow::WorkflowCallbacks> = Arc::new(RealCallbacks {
                sandbox,
                facts: state.fact_manager.clone(),
                http: reqwest::Client::new(),
                llm: wf_llm,
                tools: wf_tools,
                sub_ctx: wf_sub_ctx,
                cancel: wf_cancel,
            });
            everevo_agent::tools::builtins::WorkflowRunnerTool::new()
                .with_callbacks(cb)
                .with_workflows_dir(state.config.data_dir.join("workflows"))
        } else {
            everevo_agent::tools::builtins::WorkflowRunnerTool::new()
                .with_workflows_dir(state.config.data_dir.join("workflows"))
        }
    };
    registry.register(Arc::new(wf_tool));

    // ── list_workflows — lets the LLM discover saved workflows by name ──
    registry.register(Arc::new(
        everevo_agent::tools::builtins::ListWorkflowsTool::new(
            state.config.data_dir.join("workflows"),
        ),
    ));

    // ── save_workflow — lets the LLM sediment a repeatable procedure ──
    registry.register(Arc::new(
        everevo_agent::tools::builtins::SaveWorkflowTool::new(
            state.config.data_dir.join("workflows"),
        ),
    ));

    // ── promote_to_skill — lets the LLM promote a procedure into a skill ──
    registry.register(Arc::new(everevo_agent::skill::PromoteSkillTool::new(
        state.config.data_dir.join("skills"),
    )));
    // Code tools: MCP plugin provides simpler versions; in-process fallback for
    // background indexing and full-scope paths.
    // These tools are read-only (RiskLevel::Low) and MUST scan the whole project
    // source tree — scope them to the project root, NOT the sandbox work dir
    // (which defaults to an isolated `data/sandbox/{id}/work`). Otherwise
    // code_map/code_search only see the empty sandbox and every project-path
    // query fails with a read_dir error. (User requirement #3: 全域只读源码检索.)
    let code_workspace = project_root.clone();
    if registry.get("code_search").is_none() {
        let code_search =
            everevo_agent::tools::builtins::CodeSearchTool::new(code_workspace.clone());
        code_search.start_background_index();
        registry.register(Arc::new(code_search));
    }
    if registry.get("code_map").is_none() {
        registry.register(Arc::new(everevo_agent::tools::builtins::CodeMapTool::new(
            code_workspace.clone(),
        )));
    }
    // list_dir, read_file, write_file are provided by:
    // - MCP plugins (primary, loaded in step [2])
    // - Bootstrap tools (kernel-built: read_file, write_file)
    // No in-process fallback needed.

    // ── Cluster tool ──
    // Build a SubAgentPool for cluster patterns (fan_out, map_reduce, verify)
    let cluster_pool = {
        let cluster_base =
            Arc::new(registry.subset(&["shell", "memory", "read_file", "list_dir", "code_search"]));
        let pool = everevo_agent::subagent_pool::SubAgentPool::new(
            everevo_agent::subagent_pool::SubAgentPoolConfig {
                max_concurrent: 8,
                timeout_secs: 300,
            },
            Arc::clone(client),
            cluster_base,
            sub_ctx.clone(),
            Arc::new(
                session_work_dir
                    .clone()
                    .unwrap_or_else(|| state.config.data_dir.clone()),
            ),
        );
        Arc::new(pool)
    };
    registry.register(Arc::new(
        everevo_agent::tools::builtins::ClusterTool::new().with_pool(cluster_pool),
    ));

    // ── Base registries for sub-agents ──
    // base_for_task and base_for_workflow share the same tool set; the task
    // variant swaps shell for an auto-confirming SandboxedShellTool under
    // fully_auto (the per-session store's auto_confirm inherits it). Both
    // derive from the single main registry by name.
    let base_for_workflow = registry.subset(&[
        "shell",
        "memory",
        "code_map",
        "list_dir",
        "read_file",
        "code_search",
    ]);
    let mut base_for_task = registry.subset(&[
        "shell",
        "memory",
        "code_map",
        "list_dir",
        "read_file",
        "code_search",
    ]);
    if is_fully_auto {
        if let Some(sandboxes) = state.sandboxes.read().await.get(&session_id) {
            let auto_shell = Arc::new(SandboxedShellTool {
                inner: sandboxes.provider(),
                work_dir: sandboxes.work_dir().clone(),
                session_id,
                store: Arc::clone(&store),
            });
            base_for_task = base_for_workflow.subset(&[
                "memory",
                "code_map",
                "list_dir",
                "read_file",
                "code_search",
            ]);
            base_for_task.register(auto_shell);
        }
    }

    // ── Task tool (sub-agent delegation) ──
    // Enable recursive delegation for sub-agents within depth limit.
    // base_for_task gets a task tool clone; TaskTool itself gets a copy without it.
    let max_depth = state.config.subagent_max_depth;
    let task_tool = if sub_ctx.depth < max_depth {
        let task_registry = base_for_task.subset(&[
            "shell",
            "memory",
            "code_map",
            "list_dir",
            "read_file",
            "code_search",
        ]);
        // Give sub-agents a TaskTool with tighter limits for deeper recursion
        let sub_task = everevo_agent::tools::builtins::TaskTool::new(
            Arc::new(state.config.data_dir.join("sandbox")),
            Arc::new(ToolRegistry::new()),
            Some(Arc::clone(client)),
        )
        .with_subagent_limits(30, 300);
        base_for_task.register(Arc::new(sub_task));

        everevo_agent::tools::builtins::TaskTool::new(
            Arc::new(state.config.data_dir.join("sandbox")),
            Arc::new(task_registry),
            Some(Arc::clone(client)),
        )
        .with_subagent_limits(100, 600)
    } else {
        tracing::info!(
            depth = sub_ctx.depth,
            max = max_depth,
            "Task tool disabled for sub-agents — max depth reached"
        );
        everevo_agent::tools::builtins::TaskTool::new(
            Arc::new(state.config.data_dir.join("sandbox")),
            Arc::new(base_for_task),
            Some(Arc::clone(client)),
        )
        .with_subagent_limits(100, 600)
    };

    let pending = task_tool.pending.clone();
    let rx = task_tool.take_receiver();
    let task_backlog = Arc::clone(&task_tool.results_backlog);
    let task_handles = task_tool.handles.clone();
    let task_statuses = task_tool.statuses.clone();
    // Capture result sender BEFORE task_tool is moved into registry.
    // TeamTool shares this channel so team agent results are injected
    // into the main loop as [SubAgent Result] messages.
    let task_result_tx = task_tool.result_sender();

    // Set sub-agent context + persona
    *task_tool
        .subagent_ctx
        .write()
        .unwrap_or_else(|e| e.into_inner()) = sub_ctx.clone();
    let profile_path = state
        .config
        .data_dir
        .join("memory")
        .join("persona")
        .join("profile.json");
    if let Ok(content) = std::fs::read_to_string(&profile_path) {
        if let Ok(profile) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(injection) = profile
                .get("system_prompt_injection")
                .and_then(|v| v.as_str())
            {
                task_tool.set_persona(injection.to_string());
            }
        }
    }

    state
        .subagent_handles
        .write()
        .await
        .insert(session_id, task_handles.clone());
    state
        .subagent_statuses
        .write()
        .await
        .insert(session_id, task_statuses.clone());

    registry.register(Arc::new(task_tool));

    // ── cancel_task tool — lets the LLM proactively stop a sub-agent ──
    // Shares the same handles/pending/statuses Arcs as the TaskTool above so
    // cancellation propagates to the spawned sub-agent's CancellationToken.
    registry.register(Arc::new(
        everevo_agent::tools::builtins::CancelTaskTool::new(
            task_handles.clone(),
            task_statuses.clone(),
            pending.clone(),
        ),
    ));

    // ── Workflow tool ──
    registry.register(Arc::new(
        everevo_agent::tools::builtins::WorkflowTool::new(
            everevo_agent::tools::builtins::workflow::new_workflow_results(),
        )
        .with_subagent_engine(
            Arc::clone(client),
            Arc::new(base_for_workflow),
            Arc::new(session_work_dir.unwrap_or_else(|| state.config.data_dir.join("sandbox"))),
        )
        .with_shared_counters(pending.clone(), task_backlog.clone()),
    ));

    // ── Team tool — wired with LLM + tools for real sub-agent dispatch ──
    let team_base = registry.subset(&["shell", "memory"]);
    let sandbox_root = state.config.data_dir.join("sandbox");
    let mut team_tool = everevo_agent::tools::builtins::TeamTool::new()
        .with_llm(Arc::clone(client))
        .with_base_tools(Arc::new(team_base))
        .with_sandbox_root(Arc::new(sandbox_root))
        .with_shared_counters(pending.clone(), task_backlog.clone());
    if let Some(ref tx) = task_result_tx {
        team_tool = team_tool.with_result_tx(tx.clone());
    }
    registry.register(Arc::new(team_tool));

    // ── Tool Hooks (automatic pre/post execution gates) ──
    // NOTE: These are in-process ToolHook implementations that fire automatically
    // on every tool call. The MCP hook plugins (audit_hook, review_gate, reflect_gate)
    // expose CALLABLE tools the agent can invoke explicitly (audit_log, review_pre_execute,
    // reflect_post_execute). These two mechanisms are complementary:
    //
    //   - In-process hooks: automatic safety net (always runs)
    //   - MCP hook tools:   explicit agent introspection (on-demand)
    //
    // Migration path: when MCP plugins can expose ToolHook trait via MCP resources,
    // these can be converted to McpToolHook wrappers. For now, both coexist.
    //
    // ── Review gate (PRE-ACT) — blocks unsafe/redundant tool calls ──
    registry.add_hook(Arc::new(
        everevo_agent::tools::review_gate::ReviewGateHook::new(
            everevo_core::types::RiskLevel::High,
        ),
    ));

    // ── Audit hook — logs every tool call ──
    registry.add_hook(Arc::new(everevo_agent::tools::audit_hook::AuditHook::new()));

    // ── Reflect gate (POST-ACT) — quick error check + trajectory recording ──
    let reflect_gate = Arc::new(everevo_agent::tools::reflect_gate::ReflectGateHook::new());
    let hook_feedback = reflect_gate.feedback_slot();
    registry.add_hook(reflect_gate);

    // ── MCP tools — register tools from connected MCP servers ──
    {
        let mcp = state.mcp_clients.read().await;
        for (name, client) in mcp.iter() {
            let guard = client.lock().await;
            let mcp_tools: Vec<_> =
                everevo_mcp::McpTool::from_defs(Arc::clone(client), &guard.tools);
            for tool in mcp_tools {
                registry.register(tool);
            }
            tracing::info!(server = %name, tool_count = guard.tools.len(), "MCP tools registered");
        }
    }

    // ── Plan mode tool restriction ──
    if is_plan_mode {
        let before = registry.len();
        registry.retain(|tool| {
            everevo_agent::tools::builtins::is_tool_allowed_in_plan_mode(tool.name())
        });
        tracing::info!(
            %session_id,
            before,
            after = registry.len(),
            "Plan mode: filtered write tools"
        );
    }

    let tools = Arc::new(registry);
    tracing::info!(tool_count = tools.len(), "Agent tools ready");

    // ── Symbol registry: populate knowledge graph with tool entities ──
    // Idempotent — re-running doesn't duplicate entities.
    let sr =
        everevo_knowledge::graph::SymbolRegistry::new(Some(Arc::clone(&state.knowledge_graph)));
    if let Err(e) = sr.register_tools(&tools) {
        tracing::warn!(error = %e, "Symbol registry: tool registration failed (non-fatal)");
    }

    AssembledTools {
        tools,
        pending,
        subagent_rx: rx,
        results_backlog: task_backlog,
        task_handles,
        task_statuses,
        compact_focus,
        hook_feedback,
    }
}
