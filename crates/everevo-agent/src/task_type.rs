//! Task type and priority enums — extracted from deprecated orchestration.rs.
//! Used by TaskTool (delegate.rs) for classifying sub-agent tasks.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskType {
    DirectAnswer,
    CodeTask,
    ResearchTask,
    ReviewTask,
    FileOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskPriority {
    Low,
    Normal,
    High,
    Critical,
}
