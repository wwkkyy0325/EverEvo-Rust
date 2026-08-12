use std::sync::Arc;

use everevo_agent::MemoryStage;
use everevo_core::context::ContextSnapshot;

use super::AppState;

impl AppState {
    /// Create a sandbox for a session. Default level is SemiAuto.
    ///
    /// Uses the cached runtime_env (computed once at startup) to inject
    /// portable runtime paths. This avoids repeated filesystem scans on
    /// every session creation.
    /// Build a memory stage wired with all available backends (KG, RAG, workflows, telemetry).
    /// Session-scoped (分层记忆): recall is filtered to the session's own working
    /// memory + the global tier, so cross-session facts stay strictly isolated.
    pub fn build_memory_stage(
        &self,
        session_id: uuid::Uuid,
        trace_id: Option<uuid::Uuid>,
    ) -> MemoryStage {
        let mut stage = MemoryStage::new(self.fact_manager.clone())
            .with_knowledge_graph(self.knowledge_graph.clone())
            .with_workflows_dir(self.config.data_dir.join("workflows"))
            .with_session_id(Some(session_id));
        if let Some(ref rag) = self.rag_pipeline {
            stage = stage.with_rag(Arc::clone(rag));
        }
        if let Some(tid) = trace_id {
            stage = stage.with_telemetry(self.telemetry_pipeline.clone(), tid);
        }
        stage
    }

    /// Store a context snapshot for a session, evicting the oldest entry
    /// if the ring buffer is full (max 5 entries).
    pub async fn record_context_snapshot(&self, snapshot: ContextSnapshot) {
        const MAX_SNAPSHOTS: usize = 5;
        let mut map = self.context_snapshots.write().await;
        let entries = map.entry(snapshot.session_id).or_default();
        if entries.len() >= MAX_SNAPSHOTS {
            entries.remove(0); // evict oldest
        }
        entries.push(snapshot);
    }
}
