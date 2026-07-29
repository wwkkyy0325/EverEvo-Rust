//! EverEvo workflow engine — execute multi-step automation workflows.
//!
//! ## Design
//!
//! Workflows are JSON-defined step sequences executed by the engine.
//! Callbacks provide I/O (shell, fetch, memory, agent) so the engine
//! stays pure logic and testable.
//!
//! ## Step types
//!
//! - `shell` — execute a command
//! - `fetch` — fetch a URL
//! - `memory_save` / `memory_search` — persistent memory ops
//! - `agent` — run a sub-agent
//! - `delay` — wait N seconds
//! - `log` — emit a log message
//! - `set_variable` — set a variable for later steps
//! - `condition` — if/else branching
//!
//! ## Variable references
//!
//! Steps can reference previous step outputs using `${{step_id.key}}`.
//! The engine resolves these at execution time.
//!
//! ```json
//! {
//!   "name": "Deploy Check",
//!   "steps": [
//!     {"id": "build", "type": "shell", "params": {"command": "cargo build"}},
//!     {"id": "test",  "type": "shell", "params": {"command": "cargo test"}},
//!     {"id": "notify","type": "log",   "params": {"message": "Build: ${{build.exit_code}}, Tests: ${{test.exit_code}}"}}
//!   ]
//! }
//! ```

pub mod engine;
pub mod types;

pub use engine::{NoopCallbacks, WorkflowCallbacks, WorkflowEngine};
pub use types::*;
