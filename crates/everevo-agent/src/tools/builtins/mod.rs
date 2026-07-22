//! Built-in tool implementations — all use **direct API calls** to internal crates.
//!
//! | Tool | Backend | Call Pattern |
//! |------|---------|-------------|
//! | `shell` | ShellResolver + TieredSandbox | Sandbox CLI execution |
//! | `download` | everevo_downloader::Downloader | Direct API → Downloader::submit() |
//! | `bootstrap` | everevo_bootstrap::Bootstrap | Direct API → Bootstrap::check() |

mod bootstrap;
mod delegate;
mod download;
mod memory_tool;
mod shell;

pub use bootstrap::BootstrapTool;
pub use delegate::{SubAgentHandle, SubAgentStatus, TaskTool};
pub use download::DownloadTool;
pub use memory_tool::MemoryTool;
pub use shell::ShellTool;
