//! Tool registry assembly — extracted from chat.rs §6.
//! Builds the per-session registry (12 base tools + MCP) with dependency injection.

use crate::app_state::{AppState, ConfirmationNotification};
use crate::sandbox_tool::SandboxedShellTool;
use everevo_agent::subagent_context::SubAgentContext;
use everevo_agent::tools::builtins::{SubAgentHandle, SubAgentStatus};
use everevo_core::tool::ToolRegistry;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

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

    // ── Per-session tools ──
    let session_work_dir = {
        let sandboxes = state.sandboxes.read().await;
        if let Some(sb) = sandboxes.get(&session_id) {
            let work_dir = sb.work_dir().clone();
            let shell = Arc::new(SandboxedShellTool {
                inner: sb.provider(),
                work_dir: work_dir.clone(),
                session_id,
                confirmations: state.confirmations.clone(),
                notif_tx: notif_tx.clone(),
                auto_confirm: false,
            });
            registry.register(shell);

            // Download tool scoped to sandbox work_dir
            let mut dl =
                everevo_agent::tools::builtins::DownloadTool::new(state.downloader.clone());
            dl = dl.with_work_dir(work_dir.clone());
            registry.register(Arc::new(dl));

            Some(work_dir)
        } else {
            None
        }
    };

    // ── Global tools ──
    registry.register(Arc::new(
        everevo_agent::tools::builtins::BootstrapTool::new(state.bootstrap.clone()),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::MemoryTool::new(state.fact_manager.clone())
            .with_db(state.db.clone())
            .with_kg(state.knowledge_graph.clone()),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::TodoWriteTool::new(state.todo_store.clone())
            .with_persistence(state.config.data_dir.join("tasks"))
            .with_session_id(session_id),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::EnterPlanModeTool::new(
            Arc::clone(&plan_state),
            state.config.data_dir.clone(),
        ),
    ));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::ExitPlanModeTool::new(
            Arc::clone(&plan_state),
            state.config.data_dir.clone(),
        ),
    ));
    registry.register(Arc::new(everevo_agent::tools::builtins::SkillTool::new(
        state.skill_registry.clone(),
    )));
    registry.register(Arc::new(everevo_agent::tools::builtins::VerifyTool));
    registry.register(Arc::new(everevo_agent::tools::builtins::WebFetchTool));
    registry.register(Arc::new(everevo_agent::tools::builtins::WebSearchTool));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::CompactTool::new()
            .with_compact_focus(Arc::clone(&compact_focus))
            .with_dreaming_engine(state.dreaming_engine.clone()),
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
            let mut wf_tools = ToolRegistry::new();
            if let Some(shell) = registry.get("shell") {
                wf_tools.register(Arc::clone(shell));
            }
            if let Some(memory) = registry.get("memory") {
                wf_tools.register(Arc::clone(memory));
            }
            let wf_tools = Arc::new(wf_tools);
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
                    let result = everevo_agent::AgentLoop::new()
                        .with_max_turns(mt)
                        .run_subagent(
                            Arc::clone(&self.llm),
                            Arc::clone(&self.tools),
                            messages,
                            self.cancel.clone(),
                        )
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
    let workspace = session_work_dir
        .clone()
        .unwrap_or_else(|| state.config.data_dir.clone());
    let code_search = everevo_agent::tools::builtins::CodeSearchTool::new(workspace.clone());
    // Start background indexing — first search will use pre-built index if ready
    code_search.start_background_index();
    registry.register(Arc::new(code_search));
    registry.register(Arc::new(everevo_agent::tools::builtins::CodeMapTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(everevo_agent::tools::builtins::ListDirTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(everevo_agent::tools::builtins::ReadFileTool::new(
        workspace.clone(),
    )));
    registry.register(Arc::new(
        everevo_agent::tools::builtins::WriteFileTool::new(workspace),
    ));

    // ── Cluster tool ──
    // Build a SubAgentPool for cluster patterns (fan_out, map_reduce, verify)
    let cluster_pool = {
        let mut cluster_base = ToolRegistry::new();
        if let Some(shell) = registry.get("shell") {
            cluster_base.register(Arc::clone(shell));
        }
        if let Some(memory) = registry.get("memory") {
            cluster_base.register(Arc::clone(memory));
        }
        if let Some(read_file) = registry.get("read_file") {
            cluster_base.register(Arc::clone(read_file));
        }
        if let Some(list_dir) = registry.get("list_dir") {
            cluster_base.register(Arc::clone(list_dir));
        }
        if let Some(code_search) = registry.get("code_search") {
            cluster_base.register(Arc::clone(code_search));
        }
        let pool = everevo_agent::subagent_pool::SubAgentPool::new(
            everevo_agent::subagent_pool::SubAgentPoolConfig {
                max_concurrent: 8,
                timeout_secs: 300,
            },
            Arc::clone(client),
            Arc::new(cluster_base),
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
    let mut base_for_task = ToolRegistry::new();
    if let Some(shell) = registry.get("shell") {
        if is_fully_auto {
            if let Some(sandboxes) = state.sandboxes.read().await.get(&session_id) {
                let auto_shell = Arc::new(SandboxedShellTool {
                    inner: sandboxes.provider(),
                    work_dir: sandboxes.work_dir().clone(),
                    session_id,
                    confirmations: state.confirmations.clone(),
                    notif_tx: notif_tx.clone(),
                    auto_confirm: true,
                });
                base_for_task.register(auto_shell);
            } else {
                base_for_task.register(Arc::clone(shell));
            }
        } else {
            base_for_task.register(Arc::clone(shell));
        }
    }
    if let Some(memory) = registry.get("memory") {
        base_for_task.register(Arc::clone(memory));
    }
    if let Some(code_map) = registry.get("code_map") {
        base_for_task.register(Arc::clone(code_map));
    }
    // Sub-agents need read access to workspace files for productive work
    if let Some(list_dir) = registry.get("list_dir") {
        base_for_task.register(Arc::clone(list_dir));
    }
    if let Some(read_file) = registry.get("read_file") {
        base_for_task.register(Arc::clone(read_file));
    }
    if let Some(code_search) = registry.get("code_search") {
        base_for_task.register(Arc::clone(code_search));
    }

    let mut base_for_workflow = ToolRegistry::new();
    if let Some(shell) = registry.get("shell") {
        base_for_workflow.register(Arc::clone(shell));
    }
    if let Some(memory) = registry.get("memory") {
        base_for_workflow.register(Arc::clone(memory));
    }
    if let Some(code_map) = registry.get("code_map") {
        base_for_workflow.register(Arc::clone(code_map));
    }
    if let Some(list_dir) = registry.get("list_dir") {
        base_for_workflow.register(Arc::clone(list_dir));
    }
    if let Some(read_file) = registry.get("read_file") {
        base_for_workflow.register(Arc::clone(read_file));
    }
    if let Some(code_search) = registry.get("code_search") {
        base_for_workflow.register(Arc::clone(code_search));
    }

    // ── Task tool (sub-agent delegation) ──
    // Enable recursive delegation for sub-agents within depth limit.
    // base_for_task gets a task tool clone; TaskTool itself gets a copy without it.
    let max_depth = state.config.subagent_max_depth;
    let task_tool = if sub_ctx.depth < max_depth {
        let mut task_registry = ToolRegistry::new();
        for name in &[
            "shell",
            "memory",
            "code_map",
            "list_dir",
            "read_file",
            "code_search",
        ] {
            if let Some(tool) = base_for_task.get(name) {
                task_registry.register(Arc::clone(tool));
            }
        }
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
    let mut team_base = ToolRegistry::new();
    if let Some(shell) = registry.get("shell") {
        team_base.register(Arc::clone(shell));
    }
    if let Some(memory) = registry.get("memory") {
        team_base.register(Arc::clone(memory));
    }
    let sandbox_root = state.config.data_dir.join("sandbox");
    registry.register(Arc::new(
        everevo_agent::tools::builtins::TeamTool::new()
            .with_llm(Arc::clone(client))
            .with_base_tools(Arc::new(team_base))
            .with_sandbox_root(Arc::new(sandbox_root))
            .with_shared_counters(pending.clone(), task_backlog.clone()),
    ));

    // ── Review gate (PRE-ACT) — blocks unsafe/redundant tool calls ──
    registry.add_hook(Arc::new(
        everevo_agent::tools::review_gate::ReviewGateHook::new(everevo_core::types::RiskLevel::High),
    ));

    // ── Audit hook — logs every tool call ──
    registry.add_hook(Arc::new(everevo_agent::tools::audit_hook::AuditHook::new()));

    // ── Reflect gate (POST-ACT) — quick error check + trajectory recording ──
    let reflect_gate = Arc::new(everevo_agent::tools::reflect_gate::ReflectGateHook::new());
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
    let sr = everevo_knowledge::graph::SymbolRegistry::new(Some(Arc::clone(
        &state.knowledge_graph,
    )));
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
    }
}
