//! Built-in tool implementations — all use **direct API calls** to internal crates.
//!
//! | Tool | Backend | Call Pattern |
//! |------|---------|-------------|
//! | `shell` | ShellResolver + TieredSandbox | Sandbox CLI execution |
//! | `download` | everevo_downloader::Downloader | Direct API → Downloader::submit() |
//! | `bootstrap` | everevo_bootstrap::Bootstrap | Direct API → Bootstrap::check() |

mod bootstrap;
mod browser_bridge;
mod cluster;
mod code_search;
mod compact;
mod delegate;
mod describe_image;
mod download;
mod http_util;
mod memory_tool;
mod plan_mode;
mod shell;
mod skill;
mod team;
mod todo_write;
mod tool_cache_read;
pub mod workflow;
mod workflow_runner;

pub use bootstrap::BootstrapTool;
pub use cluster::ClusterTool;
pub use code_search::{CodeMapTool, CodeSearchTool};
pub use compact::CompactTool;
pub use delegate::{CancelTaskTool, SubAgentHandle, SubAgentStatus, TaskTool};
pub use describe_image::DescribeImageTool;
pub use download::DownloadTool;
pub use memory_tool::MemoryTool;
pub use plan_mode::{
    is_tool_allowed_in_plan_mode, EnterPlanModeTool, ExitPlanModeTool, PlanModeState,
};
pub use shell::ShellTool;
pub use skill::SkillTool;
pub use team::{TeamRole, TeamTool};
pub use todo_write::{
    load_persisted_tasks, new_todo_store, TodoItem, TodoStore, TodoWriteTool, GLOBAL_TASK_KEY,
};
pub use tool_cache_read::ToolCacheReadTool;
pub use workflow::{WorkflowResults, WorkflowTask, WorkflowTool};
pub use workflow_runner::{ListWorkflowsTool, SaveWorkflowTool, WorkflowRunnerTool};
