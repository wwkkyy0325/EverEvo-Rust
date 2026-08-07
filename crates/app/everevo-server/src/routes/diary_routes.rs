//! Diary API — read and manage diary entries.
//!
//! GET  /api/diary          → list recent diary files
//! GET  /api/diary?date=X   → read a specific date's diary
//! GET  /api/diary/today     → read today's diary

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use crate::app_state::AppState;
use everevo_core::ApiError;

#[derive(Deserialize, Default)]
struct DiaryQuery {
    date: Option<String>,
}

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/diary", axum::routing::get(list_or_read))
        .route("/api/diary/today", axum::routing::get(today))
}

async fn list_or_read(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DiaryQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(date) = q.date {
        match state.diary_manager.read_date(&date) {
            Ok(content) => Ok(Json(serde_json::json!({
                "date": date,
                "content": content,
                "has_content": !content.is_empty(),
            }))),
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    } else {
        // List recent diary files
        match state.diary_manager.read_recent(14) {
            Ok(files) => {
                let entries: Vec<_> = files
                    .into_iter()
                    .map(|(date, content)| {
                        let excerpt: String = content
                            .lines()
                            .filter(|l| !l.starts_with('#') && !l.is_empty())
                            .take(3)
                            .collect::<Vec<_>>()
                            .join(" ");
                        serde_json::json!({
                            "date": date,
                            "size": content.len(),
                            "excerpt": excerpt.chars().take(200).collect::<String>(),
                        })
                    })
                    .collect();
                Ok(Json(serde_json::json!({"files": entries})))
            }
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    }
}

async fn today(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    match state.diary_manager.read_today() {
        Ok(content) => Ok(Json(serde_json::json!({
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "content": content,
            "has_content": !content.is_empty(),
        }))),
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}
