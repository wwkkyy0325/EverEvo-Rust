//! Domain knowledge base API routes — CRUD, search, ingest, inbox processing.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::AppState;
use everevo_core::ApiError;
use everevo_knowledge::domain::{DocumentParser, Domain, DomainManager};
use everevo_core::llm::LlmProvider;
use everevo_core::retrieval::Retriever;
use everevo_core::EverEvoError;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/domains", get(list_domains).post(create_domain))
        .route("/api/domains/search", get(search_domains))
        .route("/api/domains/inbox", post(process_inbox))
        .route("/api/domains/{id}", get(get_domain).delete(delete_domain))
        .route("/api/domains/{id}/ingest", post(ingest_document))
        .route("/api/domains/{id}/merge", post(merge_domain))
        // RAG endpoints
        .route("/api/rag/search", get(rag_search))
        .route("/api/rag/ingest", post(rag_ingest))
        // Memory fact management (full CRUD — list/create/get/update/delete)
        .route("/api/memory/facts", get(list_facts).post(create_fact))
        .route("/api/memory/facts/{name}", get(get_fact).put(update_fact).delete(delete_fact))
}

// ── Request / Response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateDomainRequest {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
struct MergeRequest {
    source_id: String,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// GET /api/domains — list all domains with coverage stats.
async fn list_domains(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let mgr = DomainManager::load(&domain_root)?;
    let coverage = mgr.coverage();

    let domains: Vec<serde_json::Value> = mgr
        .registry
        .domains
        .values()
        .filter(|d| d.merged_into.is_none())
        .map(|d| {
            let stats = coverage.iter().find(|c| c.domain_id == d.id);
            serde_json::json!({
                "id": d.id,
                "name": d.name,
                "description": d.description,
                "document_count": d.document_count,
                "related_ids": d.related_ids,
                "has_relations": stats.map(|c| c.has_relations).unwrap_or(false),
                "is_new": stats.map(|c| c.is_new).unwrap_or(false),
                "created_at": d.created_at.to_rfc3339(),
                "updated_at": d.updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(
        serde_json::json!({ "domains": domains, "total": domains.len() }),
    ))
}

/// POST /api/domains — create a new domain.
async fn create_domain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateDomainRequest>,
) -> Result<Json<Domain>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let mut mgr = DomainManager::load(&domain_root)?;

    let id = if req.id.is_empty() {
        req.name.to_lowercase().replace(' ', "-")
    } else {
        req.id
    };

    if mgr.registry.domains.contains_key(&id) {
        return Err(ApiError::conflict(format!("Domain '{id}' already exists")));
    }

    let description = if req.description.is_empty() {
        format!("Knowledge domain for {}", req.name)
    } else {
        req.description
    };

    mgr.registry.create(id.clone(), req.name, description);
    if mgr.registry.domains.contains_key(&id) {
        // Create domain directory structure
        let doc_dir = domain_root.join(&id).join("documents");
        let _ = std::fs::create_dir_all(&doc_dir);
        let inbox_dir = domain_root.join(&id).join("inbox");
        let _ = std::fs::create_dir_all(&inbox_dir);
    }
    mgr.save()?;

    let domain = mgr
        .registry
        .domains
        .get(&id)
        .cloned()
        .ok_or(EverEvoError::NotFound(id))?;

    Ok(Json(domain))
}

/// GET /api/domains/{id} — get domain details + document list.
async fn get_domain(
    State(state): State<Arc<AppState>>,
    Path(domain_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let mgr = DomainManager::load(&domain_root)?;

    let domain = mgr
        .registry
        .domains
        .get(&domain_id)
        .cloned()
        .ok_or_else(|| EverEvoError::NotFound(format!("Domain '{domain_id}' not found")))?;

    let documents = mgr.list_documents(&domain_id)?;
    let suggestions = mgr.registry.suggest_relations(&domain_id, 0.6);

    Ok(Json(serde_json::json!({
        "domain": {
            "id": domain.id,
            "name": domain.name,
            "description": domain.description,
            "document_count": domain.document_count,
            "parent_id": domain.parent_id,
            "related_ids": domain.related_ids,
            "merged_into": domain.merged_into,
            "created_at": domain.created_at.to_rfc3339(),
            "updated_at": domain.updated_at.to_rfc3339(),
        },
        "documents": documents.iter().map(|d| serde_json::json!({
            "filename": d.filename,
            "size_bytes": d.size_bytes,
            "modified": d.modified.to_rfc3339(),
        })).collect::<Vec<_>>(),
        "suggested_relations": suggestions.iter().map(|(id, score)| serde_json::json!({
            "domain_id": id,
            "similarity": score,
        })).collect::<Vec<_>>(),
    })))
}

/// DELETE /api/domains/{id} — mark a domain as merged (soft delete).
async fn delete_domain(
    State(state): State<Arc<AppState>>,
    Path(domain_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let mut mgr = DomainManager::load(&domain_root)?;

    if !mgr.registry.domains.contains_key(&domain_id) {
        return Err(ApiError::not_found(format!(
            "Domain '{domain_id}' not found"
        )));
    }

    // Mark as merged into "archived" (soft delete)
    mgr.registry.create(
        "archived".into(),
        "Archived".into(),
        "Soft-deleted domains".into(),
    );
    mgr.registry.merge_domains("archived", &domain_id)?;
    mgr.save()?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/domains/{id}/ingest — upload a document into a domain.
async fn ingest_document(
    State(state): State<Arc<AppState>>,
    Path(domain_id): Path<String>,
    body: String,
) -> Result<Json<serde_json::Value>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let models_dir = state.config.data_dir.join("models");
    let mut mgr = DomainManager::load_with_onnx(&domain_root, &models_dir)?;

    if !mgr.registry.domains.contains_key(&domain_id) {
        return Err(ApiError::not_found(format!(
            "Domain '{domain_id}' not found"
        )));
    }

    let doc_id = Uuid::new_v4();
    let filename = format!("{doc_id}.md");
    let text = DocumentParser::parse(&filename, body.as_bytes()).unwrap_or(body.clone());
    let _hash = everevo_knowledge::domain::content_hash(&text);
    let chunker = everevo_knowledge::domain::SemanticChunker::default();
    let chunks = chunker.chunk(&text);

    // Write document to domain's documents directory
    let doc_dir = domain_root.join(&domain_id).join("documents");
    std::fs::create_dir_all(&doc_dir).ok();
    std::fs::write(doc_dir.join(&filename), &body)
        .map_err(|e| EverEvoError::Internal(format!("Write doc: {e}")))?;

    // Update centroid with real embedding (falls back to dummy if ONNX unavailable)
    let doc_vec: Vec<f32> = if let Some(emb) = mgr.embedder() {
        emb.encode(&text)
            .unwrap_or_else(|_| vec![0.1_f32; mgr.registry.embedding_dim])
    } else {
        vec![0.1_f32; mgr.registry.embedding_dim]
    };
    let _ = mgr.registry.add_document(&domain_id, &doc_vec);
    mgr.save()?;

    Ok(Json(serde_json::json!({
        "document_id": doc_id.to_string(),
        "domain_id": domain_id,
        "chunks": chunks.len(),
        "chunk_count": chunks.len(),
    })))
}

/// POST /api/domains/inbox — process the global inbox.
async fn process_inbox(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let models_dir = state.config.data_dir.join("models");
    // Auto-detect ONNX embedder from bootstrap models
    let mut mgr = DomainManager::load_with_onnx(&domain_root, &models_dir)?;
    let result = mgr.process_global_inbox().await?;

    // Generate LLM descriptions for newly created domains
    let guard = state.llm.read().await;
    let client = guard.get("primary").and_then(|v| v.as_ref());
    if let Some(client) = client {
        for new_id in &result.new_domains {
            if let Some(domain) = mgr.registry.domains.get(new_id) {
                let prompt = build_domain_description_prompt(&domain.name, domain.document_count);
                if let Ok(resp) = client
                    .chat(&[everevo_core::llm::LlmMessage::user(&prompt)], &[])
                    .await
                {
                    if let Some(desc) = resp.content {
                        if let Some(d) = mgr.registry.domains.get_mut(new_id) {
                            d.description = desc.lines().take(3).collect::<Vec<_>>().join(" ");
                            let _ = mgr.save();
                        }
                    }
                }
            }
        }
    }

    Ok(Json(serde_json::json!({
        "processed": result.processed,
        "new_domains": result.new_domains,
    })))
}

/// POST /api/domains/{id}/merge — merge source domain into target.
async fn merge_domain(
    State(state): State<Arc<AppState>>,
    Path(target_id): Path<String>,
    Json(req): Json<MergeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let mut mgr = DomainManager::load(&domain_root)?;

    mgr.registry.merge_domains(&target_id, &req.source_id)?;
    mgr.save()?;

    let target = mgr
        .registry
        .domains
        .get(&target_id)
        .ok_or_else(|| EverEvoError::NotFound(target_id.clone()))?;

    Ok(Json(serde_json::json!({
        "merged": true,
        "target": {
            "id": target.id,
            "name": target.name,
            "document_count": target.document_count,
        }
    })))
}

/// GET /api/domains/search?q= — search across all domain documents.
async fn search_domains(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let domain_root = state.config.data_dir.join("domain");
    let retriever = everevo_knowledge::domain::DomainRetriever::new(&domain_root);
    let results = retriever.search(&query.q, query.limit);

    Ok(Json(serde_json::json!({
        "query": query.q,
        "results": results.iter().map(|r| serde_json::json!({
            "id": r.id,
            "label": r.label,
            "snippet": r.snippet,
            "score": r.score,
            "source": r.source,
            "metadata": r.metadata,
        })).collect::<Vec<_>>(),
        "total": results.len(),
    })))
}

// ── RAG Handlers ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RagSearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    top_k: usize,
}

async fn rag_search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RagSearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rag = state
        .rag_pipeline
        .as_ref()
        .ok_or_else(|| EverEvoError::Internal("RAG pipeline not initialized".into()))?;
    let results = rag.search_in("memory", &query.q, query.top_k)?;
    Ok(Json(serde_json::json!({
        "real_embeddings": rag.real_embeddings,
        "results": results.iter().map(|r| serde_json::json!({
            "id": r.chunk.id.to_string(),
            "content": r.chunk.content,
            "score": r.score,
            "chunk_type": r.chunk.chunk_type.as_str(),
        })).collect::<Vec<_>>(),
        "total": results.len(),
    })))
}

async fn rag_ingest(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<Json<serde_json::Value>, ApiError> {
    use everevo_agent::rag::make_chunk;
    use everevo_vector::ChunkType;
    let rag = state
        .rag_pipeline
        .as_ref()
        .ok_or_else(|| EverEvoError::Internal("RAG pipeline not initialized".into()))?;
    let chunk = make_chunk(body, ChunkType::Fact);
    rag.ingest_into("memory", vec![chunk])?;
    Ok(Json(serde_json::json!({ "ingested": 1 })))
}

// ── Memory Fact Handlers ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateFactRequest {
    name: String,
    content: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    fact_type: String,
}

async fn create_fact(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateFactRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ft = everevo_core::memory::FactType::from_str(&req.fact_type)
        .unwrap_or(everevo_core::memory::FactType::User);
    let now = chrono::Utc::now();
    let fact = everevo_core::memory::MemoryFact {
        name: req.name.clone(),
        description: if req.description.is_empty() {
            req.name.clone()
        } else {
            req.description
        },
        content: req.content,
        fact_type: ft,
        created_at: now,
        updated_at: now,
        projection: everevo_core::memory::ProjectionMetadata::new("1.0", "api", vec![], 1.0),
        links: vec![],
    };
    state
        .fact_manager
        .save(&fact)
        .map_err(|e| ApiError::from(EverEvoError::Internal(e.to_string())))?;
    Ok(Json(serde_json::json!({ "created": req.name })))
}

async fn get_fact(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let facts = everevo_agent::memory::FactStore::load_all(&state.fact_manager)
        .map_err(|e| ApiError::from(EverEvoError::Internal(e.to_string())))?;
    match facts.into_iter().find(|f| f.name == name) {
        Some(f) => Ok(Json(serde_json::json!({
            "name": f.name,
            "description": f.description,
            "content": f.content,
            "fact_type": f.fact_type.as_str(),
            "created_at": f.created_at.to_rfc3339(),
        }))),
        None => Err(ApiError::not_found(format!("Fact '{name}' not found"))),
    }
}

async fn update_fact(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(req): Json<CreateFactRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let facts = everevo_agent::memory::FactStore::load_all(&state.fact_manager)
        .map_err(|e| ApiError::from(EverEvoError::Internal(e.to_string())))?;
    let existing = facts
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| ApiError::not_found(format!("Fact '{name}' not found")))?;

    let ft = everevo_core::memory::FactType::from_str(&req.fact_type).unwrap_or(existing.fact_type);
    let updated = everevo_core::memory::MemoryFact {
        name: existing.name,
        description: if req.description.is_empty() {
            existing.description
        } else {
            req.description
        },
        content: if req.content.is_empty() {
            existing.content
        } else {
            req.content
        },
        fact_type: ft,
        created_at: existing.created_at,
        updated_at: chrono::Utc::now(),
        projection: existing.projection,
        links: existing.links,
    };
    state
        .fact_manager
        .save(&updated)
        .map_err(|e| ApiError::from(EverEvoError::Internal(e.to_string())))?;
    Ok(Json(serde_json::json!({ "updated": name })))
}

async fn list_facts(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let facts = state
        .fact_manager
        .load_all()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let items: Vec<_> = facts
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name, "description": f.description,
                "fact_type": f.fact_type.as_str(),
                "created_at": f.created_at.to_rfc3339(),
                "updated_at": f.updated_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": { "facts": items, "total": items.len() } })))
}

async fn delete_fact(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .fact_manager
        .delete(&name)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "data": { "deleted": true, "name": name } })))
}

/// Build an LLM prompt for generating a domain description.
fn build_domain_description_prompt(name: &str, doc_count: usize) -> String {
    format!(
        "You are organizing a knowledge base. A new domain named \"{name}\" was created \
         with {doc_count} documents. Write a concise one-line description (max 100 chars) \
         of what this domain covers based on its name. Reply ONLY with the description, \
         no prefixes, no quotes."
    )
}

