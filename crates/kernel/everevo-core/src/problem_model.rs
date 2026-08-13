//! Session-scoped problem model — the structural "causal draft" for hard
//! questions (Phase A of the problem-modeling layer).
//!
//! A `ProblemModel` is a small causal graph: nodes are sub-questions / facts /
//! claims / candidates / constraints, each tagged with an EPISTEMIC status
//! (`Verified | Unverified | Unknown`, per ADR 0009's epistemic-boundary rule);
//! edges carry the relation (`Causal | Dependency | Evidence | Contradicts`).
//! It lives per session (working memory — volatile, no cross-session pollution)
//! and is read/written by the `problem_model` tool. Solution approaches are
//! distilled separately into reusable workflows (post-turn), not here.

use serde::{Deserialize, Serialize};

/// A node in the problem model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemNode {
    pub id: String,
    pub kind: NodeKind,
    pub content: String,
    pub status: EpiStatus,
    pub source: Option<String>,
}

/// Directed relation between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemEdge {
    pub from: String,
    pub to: String,
    pub relation: EdgeKind,
}

/// The full session-scoped problem model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProblemModel {
    pub nodes: Vec<ProblemNode>,
    pub edges: Vec<ProblemEdge>,
    pub finalized: bool,
}

impl ProblemModel {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Render a compact, human/LLM-readable snapshot of the model.
    pub fn render(&self) -> String {
        let mut out = String::from("## Problem Model (causal draft)\n");
        for n in &self.nodes {
            let src = n.source.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "- `{}` [{}] [{}] {} (source: {})\n",
                n.id,
                n.kind.as_str(),
                n.status.as_str(),
                n.content,
                src
            ));
        }
        if !self.edges.is_empty() {
            out.push_str("### Relations\n");
            for e in &self.edges {
                out.push_str(&format!(
                    "- `{}` --{}--> `{}`\n",
                    e.from,
                    e.relation.as_str(),
                    e.to
                ));
            }
        }
        out.push_str(&format!(
            "### Status: {}\n",
            if self.finalized { "FINALIZED" } else { "draft" }
        ));
        out
    }
}

/// Kinds of model nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    SubQuestion,
    Fact,
    #[default]
    Claim,
    Candidate,
    Constraint,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::SubQuestion => "sub-question",
            NodeKind::Fact => "fact",
            NodeKind::Claim => "claim",
            NodeKind::Candidate => "candidate",
            NodeKind::Constraint => "constraint",
        }
    }
}

/// Epistemic status — the boundary between known / unknown / assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpiStatus {
    /// Appeared in a retrieved tool result.
    Verified,
    /// Derived or recalled, no retrieved source states it.
    Unverified,
    /// No source could be retrieved.
    #[default]
    Unknown,
}

impl EpiStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            EpiStatus::Verified => "VERIFIED",
            EpiStatus::Unverified => "UNVERIFIED",
            EpiStatus::Unknown => "UNKNOWN",
        }
    }
}

/// Edge relations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Causal,
    #[default]
    Dependency,
    Evidence,
    Contradicts,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Causal => "causes",
            EdgeKind::Dependency => "depends-on",
            EdgeKind::Evidence => "evidence-for",
            EdgeKind::Contradicts => "contradicts",
        }
    }
}

/// Validate a new node id — must be unique within the model.
pub fn node_exists(model: &ProblemModel, id: &str) -> bool {
    model.nodes.iter().any(|n| n.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProblemModel {
        let mut m = ProblemModel::default();
        m.nodes.push(ProblemNode {
            id: "q1".into(),
            kind: NodeKind::SubQuestion,
            content: "Which volcano had the most eruptions?".into(),
            status: EpiStatus::Unknown,
            source: None,
        });
        m.edges.push(ProblemEdge {
            from: "f1".into(),
            to: "q1".into(),
            relation: EdgeKind::Evidence,
        });
        m
    }

    #[test]
    fn test_model_default_empty() {
        let m = ProblemModel::default();
        assert!(m.is_empty());
        assert!(!m.finalized);
    }

    #[test]
    fn test_node_exists() {
        let m = sample();
        assert!(node_exists(&m, "q1"));
        assert!(!node_exists(&m, "missing"));
    }

    #[test]
    fn test_render_mentions_nodes_and_status() {
        let m = sample();
        let r = m.render();
        assert!(r.contains("q1"));
        assert!(r.contains("sub-question"));
        assert!(r.contains("UNKNOWN"));
        assert!(r.contains("draft"));
    }

    #[test]
    fn test_epi_status_strings() {
        assert_eq!(EpiStatus::Verified.as_str(), "VERIFIED");
        assert_eq!(EpiStatus::Unverified.as_str(), "UNVERIFIED");
        assert_eq!(EpiStatus::Unknown.as_str(), "UNKNOWN");
    }

    #[test]
    fn test_serde_roundtrip() {
        let m = sample();
        let json = serde_json::to_string(&m).unwrap();
        let back: ProblemModel = serde_json::from_str(&json).unwrap();
        assert_eq!(back.nodes.len(), 1);
        assert_eq!(back.nodes[0].status, EpiStatus::Unknown);
    }
}
