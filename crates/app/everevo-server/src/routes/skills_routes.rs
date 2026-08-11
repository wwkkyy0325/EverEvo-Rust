//! Skills management API — list, get, create, delete skills.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app_state::AppState;
use everevo_agent::skill::SkillSource;
use everevo_core::ApiError;

/// Skill metadata returned by list endpoint.
#[derive(Serialize)]
struct SkillMeta {
    name: String,
    description: String,
    source: String,
    tools: Vec<String>,
    when_to_use: Vec<String>,
    disable_model_invocation: bool,
    user_invocable: bool,
}

/// Full skill returned by get endpoint.
#[derive(Serialize)]
struct SkillDetail {
    name: String,
    description: String,
    body: String,
    source: String,
    tools: Vec<String>,
    when_to_use: Vec<String>,
    disable_model_invocation: bool,
    model_override: Option<String>,
    user_invocable: bool,
}

/// Request body for creating a skill.
#[derive(Deserialize)]
struct CreateSkillRequest {
    name: String,
    description: Option<String>,
    body: String,
    #[serde(default)]
    #[allow(dead_code)]
    tools: Vec<String>,
    #[serde(default)]
    when_to_use: Vec<String>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/skills", get(list_skills).post(create_skill))
        .route("/api/skills/{name}", get(get_skill).delete(delete_skill))
}

async fn list_skills(State(state): State<Arc<AppState>>) -> Json<Vec<SkillMeta>> {
    let skills = state.skill_registry.list_metadata();
    let result: Vec<SkillMeta> = skills
        .into_iter()
        .map(|(name, description)| {
            // Get full skill for source + tools info
            let skill = state.skill_registry.get(&name);
            SkillMeta {
                name,
                description,
                source: format!(
                    "{:?}",
                    skill
                        .as_ref()
                        .map(|s| &s.source)
                        .unwrap_or(&SkillSource::User)
                )
                .to_lowercase(),
                tools: skill.as_ref().map(|s| s.tools.clone()).unwrap_or_default(),
                when_to_use: skill
                    .as_ref()
                    .map(|s| s.when_to_use.clone())
                    .unwrap_or_default(),
                disable_model_invocation: skill
                    .as_ref()
                    .map(|s| s.disable_model_invocation)
                    .unwrap_or(false),
                user_invocable: skill.as_ref().map(|s| s.user_invocable).unwrap_or(true),
            }
        })
        .collect();
    Json(result)
}

async fn get_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SkillDetail>, ApiError> {
    let skill = state
        .skill_registry
        .get(&name)
        .ok_or_else(|| ApiError::not_found("skill not found"))?;
    Ok(Json(SkillDetail {
        name: skill.name,
        description: skill.description,
        body: skill.body,
        source: format!("{:?}", skill.source).to_lowercase(),
        tools: skill.tools,
        when_to_use: skill.when_to_use,
        disable_model_invocation: skill.disable_model_invocation,
        model_override: skill.model_override,
        user_invocable: skill.user_invocable,
    }))
}

async fn create_skill(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSkillRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if req.name.is_empty() || req.body.is_empty() {
        return Err(ApiError::bad_request("name and body are required"));
    }
    // Reject if name conflicts with a builtin
    if let Some(existing) = state.skill_registry.get(&req.name) {
        if existing.source == SkillSource::Builtin {
            return Err(ApiError::conflict(format!(
                "'{}' is a built-in skill and cannot be overwritten",
                req.name
            )));
        }
    }
    let skills_dir = state.config.data_dir.join("skills");
    let description = req.description.unwrap_or_default();
    match everevo_agent::skill::promote_to_skill(
        &skills_dir,
        &req.name,
        &description,
        &req.when_to_use,
        &req.body,
    ) {
        Ok(path) => {
            let _ = state.skill_registry.rescan();
            Ok((
                StatusCode::CREATED,
                Json(serde_json::json!({"name": req.name, "path": path.display().to_string()})),
            ))
        }
        Err(e) => Err(ApiError::bad_request(e.to_string())),
    }
}

async fn delete_skill(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Refuse to delete builtin skills
    if let Some(skill) = state.skill_registry.get(&name) {
        if skill.source == SkillSource::Builtin {
            return Err(ApiError::forbidden("built-in skill cannot be deleted"));
        }
    } else {
        return Err(ApiError::not_found("not found"));
    }
    let skill_dir = state.config.data_dir.join("skills").join(&name);
    if skill_dir.exists() {
        std::fs::remove_dir_all(&skill_dir)
            .map_err(|e| ApiError::internal(format!("IO error: {e}")))?;
        let _ = state.skill_registry.rescan();
    }
    Ok((StatusCode::NO_CONTENT, Json(serde_json::json!({}))))
}
