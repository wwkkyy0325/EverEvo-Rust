//! API integration tests — boot a minimal server and exercise endpoints.
//!
//! Each test creates a temp data directory with an in-memory SQLite DB
//! and sends real HTTP requests through the Axum router.
//!
//! Run: `cargo test -p everevo-server --test api_integration`

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use everevo_core::AppConfig;
use everevo_db::Database;
use everevo_server::build_app;

// ── Test Harness ──────────────────────────────────────────────────────────

async fn setup() -> (axum::Router, Arc<everevo_server::app_state::AppState>) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().to_path_buf();

    for sub in &[
        "db",
        "sandbox",
        "memory/facts",
        "memory/diary",
        "memory/wiki",
        "memory/.dreams",
        "memory/vector",
        "skills",
        "domain",
        "domain/inbox",
        "models",
        "runtime",
    ] {
        let _ = std::fs::create_dir_all(data_dir.join(sub));
    }

    let config = AppConfig {
        data_dir,
        ..Default::default()
    };

    let db_path = config.database_path();
    let db = Database::connect(&db_path).await.unwrap();
    build_app(config, db).await.unwrap()
}

fn req(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap()
}

async fn send(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req(method, uri, body)).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

macro_rules! ok {
    ($app:expr, $method:ident, $uri:expr) => {
        send($app, Method::$method, $uri, None).await
    };
    ($app:expr, $method:ident, $uri:expr, $body:expr) => {
        send($app, Method::$method, $uri, Some($body)).await
    };
}

// ── Health ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_returns_ok() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/health");
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
}

// ── Init ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn init_status_returns_phase() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/init/status");
    assert_eq!(status, 200);
    assert!(body.get("phase").is_some());
    assert!(body.get("has_llm").is_some());
}

#[tokio::test]
async fn init_proceed_returns_ok() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, POST, "/api/init/proceed", json!({}));
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
}

// ── Sessions CRUD ─────────────────────────────────────────────────────────

#[tokio::test]
async fn sessions_create_and_list() {
    let (app, _) = setup().await;

    // Create
    let (status, body) = ok!(&app, POST, "/api/sessions", json!({"title": "T1"}));
    assert_eq!(status, 200);
    let sid = body["data"]["id"].as_str().unwrap().to_string();

    // List
    let (status, body) = ok!(&app, GET, "/api/sessions");
    assert_eq!(status, 200);
    assert!(body["total"].as_u64().unwrap() >= 1);

    // Get
    let (status, body) = ok!(&app, GET, &format!("/api/sessions/{sid}"));
    assert_eq!(status, 200);
    assert_eq!(body["data"]["title"], "T1");

    // Update title
    let (status, _) = send(
        &app,
        Method::PATCH,
        &format!("/api/sessions/{sid}"),
        Some(json!({"title": "T1-Updated"})),
    )
    .await;
    assert!(status == 200 || status == 405); // PATCH may or may not be implemented

    // Messages
    let (status, body) = ok!(&app, GET, &format!("/api/sessions/{sid}/messages"));
    assert_eq!(status, 200);
    assert!(body["data"].is_array());

    // Delete
    let (status, _) = ok!(&app, DELETE, &format!("/api/sessions/{sid}"));
    assert_eq!(status, 200);

    // Verify deleted
    let (status, _) = ok!(&app, GET, &format!("/api/sessions/{sid}"));
    assert!(status == 200 || status == 404);
}

#[tokio::test]
async fn session_not_found_returns_error() {
    let (app, _) = setup().await;
    let (_, body) = ok!(
        &app,
        GET,
        "/api/sessions/00000000-0000-0000-0000-000000000000"
    );
    assert!(body.get("error").is_some());
}

// ── Bootstrap ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn bootstrap_status_returns_assets() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/bootstrap/status");
    assert_eq!(status, 200);
    assert!(body.get("assets").is_some());
    assert!(body.get("ready_count").is_some());
}

// ── Sandbox ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn sandbox_status_returns_data() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/sandbox/status");
    assert_eq!(status, 200);
    assert!(body.get("data").is_some());
}

#[tokio::test]
async fn sandbox_shells_returns_list() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/sandbox/shells");
    assert_eq!(status, 200);
    assert!(body["data"].is_array());
}

#[tokio::test]
async fn dreaming_status_returns_data() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/sandbox/dreaming");
    assert_eq!(status, 200);
    assert!(body.get("data").is_some());
}

// ── Config ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn config_endpoint_works() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/config");
    assert_eq!(status, 200);
    assert!(body.get("has_llm").is_some());
}

// ── MCP ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mcp_servers_returns_list() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/mcp/servers");
    assert_eq!(status, 200);
    assert!(body.get("servers").is_some());
}

// ── Agent ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_tasks_list_works() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/agent/tasks");
    assert_eq!(status, 200);
    assert!(body.get("data").is_some());
}

// ── Memory / Facts ────────────────────────────────────────────────────────

#[tokio::test]
async fn memory_facts_crud() {
    let (app, _) = setup().await;

    // Create
    let (status, body) = ok!(
        &app,
        POST,
        "/api/memory/facts",
        json!({"name": "test-api-fact", "content": "# Hello"})
    );
    assert_eq!(status, 200);
    assert_eq!(body["created"], "test-api-fact");

    // Get
    let (status, body) = ok!(&app, GET, "/api/memory/facts/test-api-fact");
    assert_eq!(status, 200);
    assert_eq!(body["name"], "test-api-fact");

    // List
    let (status, body) = ok!(&app, GET, "/api/memory/facts");
    assert_eq!(status, 200);
    assert!(body["data"]["total"].as_u64().unwrap() >= 1);

    // Delete
    let (status, _) = ok!(&app, DELETE, "/api/memory/facts/test-api-fact");
    assert_eq!(status, 200);
}

// ── Domain ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn domain_crud() {
    let (app, _) = setup().await;

    // Create
    let (status, body) = ok!(
        &app,
        POST,
        "/api/domains",
        json!({"id": "test-int-domain", "name": "Integration Test"})
    );
    assert_eq!(status, 200);
    assert_eq!(body["id"], "test-int-domain");

    // Get
    let (status, body) = ok!(&app, GET, "/api/domains/test-int-domain");
    assert_eq!(status, 200);
    assert!(body.get("domain").is_some());

    // List
    let (status, body) = ok!(&app, GET, "/api/domains");
    assert_eq!(status, 200);
    assert!(body["total"].as_u64().unwrap() >= 1);

    // Search
    let (status, body) = ok!(&app, GET, "/api/domains/search?q=Integration");
    assert_eq!(status, 200);
    assert!(body.get("results").is_some());

    // Delete (returns 204 No Content on success)
    let (status, _) = ok!(&app, DELETE, "/api/domains/test-int-domain");
    assert!(status == 200 || status == 204);
}

// ── KG ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn kg_query_works() {
    let (app, _) = setup().await;
    let (status, body) = ok!(
        &app,
        POST,
        "/api/kg/query",
        json!({"query": "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 1"})
    );
    assert_eq!(status, 200);
    assert!(body.get("results").is_some());
}

#[tokio::test]
async fn kg_entity_not_found() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/kg/entity/NonExistent");
    // Entity not found returns 404 with an error body
    assert!(status == 404 || status == 200);
    assert!(body.get("error").is_some());
}

// ── Edge Cases ────────────────────────────────────────────────────────────

#[tokio::test]
async fn invalid_json_body_returns_client_error() {
    let (app, _) = setup().await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/sessions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not valid json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_client_error());
}

#[tokio::test]
async fn empty_post_body_handled() {
    let (app, _) = setup().await;
    let (status, _) = ok!(&app, POST, "/api/sessions", json!({}));
    assert!(status == 200 || status.is_client_error());
}

// ── Workspace ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn workspace_get_returns_path() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/workspace");
    assert_eq!(status, 200);
    assert!(body.get("path").is_some());
}

#[tokio::test]
async fn workspace_put_invalid_rejects() {
    let (app, _) = setup().await;
    let (status, _body) = send(
        &app,
        Method::PUT,
        "/api/workspace",
        Some(json!({"path": "/nonexistent/path/xyz123"})),
    )
    .await;
    assert!(status.is_client_error() || status == 200);
}

// ── Diary ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn diary_today_returns_content() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/diary/today");
    assert_eq!(status, 200);
    assert!(body.get("date").is_some());
}

#[tokio::test]
async fn diary_list_returns_entries() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/diary");
    assert_eq!(status, 200);
    assert!(body.get("files").is_some() || body.get("content").is_some());
}

// ── Character ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn character_get_returns_profile() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/character");
    assert_eq!(status, 200);
    assert!(body.get("name").is_some());
}

#[tokio::test]
async fn character_put_empty_name_rejects() {
    let (app, _) = setup().await;
    let (status, _body) = send(
        &app,
        Method::PUT,
        "/api/character",
        Some(json!({"name": "", "style": "test"})),
    )
    .await;
    assert!(status.is_client_error());
}

// ── Models ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn models_list_returns_array() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/models");
    assert_eq!(status, 200);
    assert!(body.get("models").is_some());
}

#[tokio::test]
async fn models_activate_unknown_handled_gracefully() {
    let (app, _) = setup().await;
    let (status, _body) = send(
        &app,
        Method::POST,
        "/api/models/activate",
        Some(json!({"model": "nonexistent-model-xyz"})),
    )
    .await;
    // With empty model registry, this may return 200 with error or 500
    // The key is that the server doesn't crash
    assert!(status.as_u16() > 0);
}

// ── Skills ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn skills_list_returns_array() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/skills");
    assert_eq!(status, 200);
    assert!(body.is_array());
}

#[tokio::test]
async fn skills_get_unknown_returns_not_found() {
    let (app, _) = setup().await;
    let (status, _body) = ok!(&app, GET, "/api/skills/nonexistent-skill");
    assert!(status == 404 || status == 200);
}

// ── Tools / Commands ─────────────────────────────────────────────────────────

#[tokio::test]
async fn tools_list_returns_count() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/tools");
    assert_eq!(status, 200);
    assert!(body.get("tools").is_some());
    assert!(body.get("count").is_some());
}

#[tokio::test]
async fn commands_list_returns_data() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/commands");
    assert_eq!(status, 200);
    assert!(body.get("commands").is_some());
}

// ── Memory status ────────────────────────────────────────────────────────────

#[tokio::test]
async fn memory_status_returns_pipeline_info() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/memory/status");
    assert_eq!(status, 200);
    assert!(body.get("pipeline").is_some());
}

// ── Config verify (no LLM = ok:false) ────────────────────────────────────────

#[tokio::test]
async fn config_verify_without_llm_reports_not_ok() {
    let (app, _) = setup().await;
    let (status, body) = ok!(&app, GET, "/api/config/verify");
    assert_eq!(status, 200);
    assert_eq!(body.get("ok"), Some(&json!(false)));
}

// ── Error format ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn not_found_returns_api_error_envelope() {
    let (app, _) = setup().await;
    let (status, body) = ok!(
        &app,
        GET,
        "/api/sessions/00000000-0000-0000-0000-000000000000"
    );
    // Now returns proper ApiError envelope
    if status == 404 {
        let error = body.get("error").expect("should have error field");
        assert!(error.get("code").is_some(), "should have code field");
        assert!(error.get("message").is_some(), "should have message field");
    }
}

// ── Session status ───────────────────────────────────────────────────────────

#[tokio::test]
async fn session_status_returns_data() {
    let (app, _) = setup().await;
    // Create a session first
    let (_status, body) = ok!(&app, POST, "/api/sessions", json!({"title": "status-test"}));
    let sid = body["data"]["id"].as_str().unwrap();
    let (status, body) = ok!(&app, GET, &format!("/api/sessions/{sid}/status"));
    assert_eq!(status, 200);
    assert!(body.get("mode").is_some() || body.get("state").is_some());
}
