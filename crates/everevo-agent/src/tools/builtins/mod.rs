//! Built-in tool implementations — all use **direct API calls** to internal crates.
//!
//! | Tool | Backend | Call Pattern |
//! |------|---------|-------------|
//! | `shell` | ShellResolver + TieredSandbox | Sandbox CLI execution |
//! | `download` | everevo_downloader::Downloader | Direct API → Downloader::submit() |
//! | `bootstrap` | everevo_bootstrap::Bootstrap | Direct API → Bootstrap::check() |

mod bootstrap;
mod cluster;
mod code_search;
mod compact;
mod delegate;
mod download;
mod list_dir;
mod memory_tool;
mod plan_mode;
mod read_file;
mod shell;
mod skill;
mod team;
mod todo_write;
mod verify;
mod web_fetch;
mod web_search;
mod write_file;
pub mod workflow;
mod workflow_runner;

pub use bootstrap::BootstrapTool;
pub use cluster::ClusterTool;
pub use code_search::{CodeMapTool, CodeSearchTool};
pub use compact::CompactTool;
pub use delegate::{SubAgentHandle, SubAgentStatus, TaskTool};
pub use download::DownloadTool;
pub use list_dir::ListDirTool;
pub use memory_tool::MemoryTool;
pub use plan_mode::{EnterPlanModeTool, ExitPlanModeTool, PlanModeState, is_tool_allowed_in_plan_mode};
pub use read_file::ReadFileTool;
pub use shell::ShellTool;
pub use skill::SkillTool;
pub use team::{TeamRole, TeamTool};
pub use todo_write::{load_persisted_tasks, new_todo_store, TodoItem, TodoStore, TodoWriteTool};
pub use verify::VerifyTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
pub use write_file::WriteFileTool;
pub use workflow::{WorkflowResults, WorkflowTask, WorkflowTool};
pub use workflow_runner::WorkflowRunnerTool;
