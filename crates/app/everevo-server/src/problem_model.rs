//! ProblemModel data types — moved to the kernel during the P1.1
//! tool-ownership refactor. Re-exported here so existing `crate::problem_model::*`
//! imports keep working.
pub use everevo_core::problem_model::{
    node_exists, EdgeKind, EpiStatus, NodeKind, ProblemEdge, ProblemModel, ProblemNode,
};
