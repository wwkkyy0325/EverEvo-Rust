//! Built-in tool implementations — all use **direct API calls** to internal crates.
//!
//! | Tool | Backend | Call Pattern |
//! |------|---------|-------------|
//! | `shell` | ShellResolver + TieredSandbox | Sandbox CLI execution |
//! | `download` | everevo_downloader::Downloader | Direct API → Downloader::submit() |
//! | `bootstrap` | everevo_bootstrap::Bootstrap | Direct API → Bootstrap::check() |

mod ask_user_tool;
mod bootstrap;
mod cluster;
mod code_search;
mod compact;
mod delegate;
mod describe_image;
mod download;
mod memory_tool;
mod pipeline;
mod plan_mode;
mod problem_model_tool;
mod sandbox_tool;
mod shell;
mod skill;
mod team;
mod todo_write;
mod tool_cache_read;
pub mod wayback;
pub mod web_search_delegate;
pub mod workflow;
mod workflow_runner;

pub use ask_user_tool::AskUserTool;
pub use bootstrap::BootstrapTool;
pub use cluster::ClusterTool;
pub use code_search::{CodeMapTool, CodeSearchTool};
pub use compact::CompactTool;
pub use delegate::{CancelTaskTool, SubAgentHandle, SubAgentStatus, TaskTool};
pub use describe_image::DescribeImageTool;
pub use download::DownloadTool;
pub use memory_tool::MemoryTool;
pub use pipeline::PipelineTool;
pub use plan_mode::{
    is_tool_allowed_in_plan_mode, EnterPlanModeTool, ExitPlanModeTool, PlanModeState,
};
pub use problem_model_tool::ProblemModelTool;
pub use sandbox_tool::SandboxedShellTool;
pub use shell::ShellTool;
pub use skill::SkillTool;
pub use team::{TeamRole, TeamTool};
pub use todo_write::{
    load_persisted_tasks, new_todo_store, TodoItem, TodoStore, TodoWriteTool, GLOBAL_TASK_KEY,
};
pub use tool_cache_read::ToolCacheReadTool;
pub use wayback::WaybackLookupTool;
pub use web_search_delegate::WebSearchDelegateTool;
pub use workflow::{WorkflowResults, WorkflowTask, WorkflowTool};
pub use workflow_runner::{ListWorkflowsTool, SaveWorkflowTool, WorkflowRunnerTool};
