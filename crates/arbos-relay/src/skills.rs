//! Skills library (F5): reusable Markdown how-tos stored on the relay, shared across Projects.
//! Devices get full CRUD; a box reads the shared skills plus its own Project's and may save
//! skills it learned (`skills_save`), which land as that Project's own.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};
use arbos_core::skill::{slugify, Skill};

use crate::auth::Principal;
use crate::state::AppState;

type ApiResult = Result<Response, (StatusCode, String)>;

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// The project a box call is for. One box serves every project of its user (D70), so the
/// per-workspace client names the project in `X-Arbos-Project`; it must belong to the box's user.
fn box_project(state: &AppState, principal: &Principal, headers: &HeaderMap) -> Option<String> {
    let Principal::Box { user_id } = principal else {
        return None;
    };
    let id = headers
        .get("x-arbos-project")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())?;
    let p = state.db.project(id).ok().flatten()?;
    (p.user_id == *user_id).then_some(p.id)
}

/// What a principal may see: everything for devices, shared + own for a box (scoped to the
/// workspace named in the request; a box without a project header sees shared skills only).
fn visible_to(state: &AppState, principal: &Principal, headers: &HeaderMap) -> Option<String> {
    match principal {
        Principal::Device { .. } | Principal::Admin => None,
        Principal::Box { .. } => {
            Some(box_project(state, principal, headers).unwrap_or_else(|| "-".to_owned()))
        }
    }
}

fn can_see(scope: Option<&str>, principal: &Principal, s: &Skill) -> bool {
    match principal {
        Principal::Device { .. } | Principal::Admin => true,
        Principal::Box { .. } => {
            s.shared || (scope.is_some() && s.origin_project_id.as_deref() == scope)
        }
    }
}

fn can_edit(scope: Option<&str>, principal: &Principal, s: &Skill) -> bool {
    match principal {
        Principal::Device { .. } | Principal::Admin => true,
        Principal::Box { .. } => scope.is_some() && s.origin_project_id.as_deref() == scope,
    }
}

/// `POST`/`PUT` body: structured fields, or `{ "markdown": "---\n…" }` with the frontmatter.
#[derive(Deserialize, Default)]
pub struct SkillBody {
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub shared: Option<bool>,
    /// Devices may file a skill under a Project; boxes always get their own.
    #[serde(default)]
    pub origin_project_id: Option<String>,
}

impl SkillBody {
    fn apply(self, base: Skill) -> Skill {
        let mut s = match &self.markdown {
            Some(md) => {
                let parsed = Skill::from_markdown(md);
                Skill {
                    id: base.id.clone(),
                    slug: if parsed.slug.is_empty() {
                        base.slug.clone()
                    } else {
                        parsed.slug
                    },
                    name: parsed.name,
                    description: parsed.description,
                    tags: parsed.tags,
                    when_to_use: parsed.when_to_use,
                    body: parsed.body,
                    origin_project_id: base.origin_project_id.clone(),
                    shared: base.shared,
                    version: base.version,
                    created_at: base.created_at,
                    updated_at: base.updated_at,
                }
            }
            None => base,
        };
        if let Some(v) = self.name {
            s.name = v.trim().to_owned();
        }
        if let Some(v) = self.description {
            s.description = v.trim().to_owned();
        }
        if let Some(v) = self.tags {
            s.tags = v
                .into_iter()
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty())
                .collect();
        }
        if let Some(v) = self.when_to_use {
            s.when_to_use = v.trim().to_owned();
        }
        if let Some(v) = self.body {
            s.body = v.trim().to_owned();
        }
        if let Some(v) = self.shared {
            s.shared = v;
        }
        if let Some(v) = self.slug {
            s.slug = slugify(&v);
        }
        if s.slug.is_empty() {
            s.slug = slugify(&s.name);
        }
        s
    }
}

fn skill_json(s: &Skill) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    v["markdown"] = json!(s.to_markdown());
    v
}

fn index_json(s: &Skill) -> Value {
    json!({
        "id": s.id, "slug": s.slug, "name": s.name, "description": s.description, "tags": s.tags,
        "when_to_use": s.when_to_use, "shared": s.shared, "origin_project_id": s.origin_project_id,
        "version": s.version, "updated_at": s.updated_at,
    })
}

/// Queue `skills_changed` for every box so the index cache refreshes before its 60 s timer.
fn notify_boxes(state: &AppState, slug: &str) {
    if let Ok(ids) = state.db.sandbox_project_ids() {
        for id in ids {
            let _ = state
                .db
                .enqueue_push(&id, "skills_changed", &json!({ "slug": slug }));
        }
    }
}

/// `GET /skills` (device: all; box: shared + own) and `GET /skills/search?q=`.
pub async fn list(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let vis = visible_to(&state, &principal, &headers);
    let rows = match q.get("q").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(query) => state
            .db
            .search_skills(query, vis.as_deref(), 50)
            .map_err(internal)?,
        None => state.db.list_skills(vis.as_deref()).map_err(internal)?,
    };
    Ok(Json(rows.iter().map(skill_json).collect::<Vec<_>>()).into_response())
}

/// `GET /skills/index` (box): the lean view the relevance selector caches, plus a change marker.
pub async fn index(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
) -> ApiResult {
    let vis = visible_to(&state, &principal, &headers);
    let rows = state.db.list_skills(vis.as_deref()).map_err(internal)?;
    let rev = state.db.skills_rev(vis.as_deref()).map_err(internal)?;
    Ok(Json(json!({
        "rev": rev,
        "skills": rows.iter().map(index_json).collect::<Vec<_>>(),
    }))
    .into_response())
}

pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let scope = box_project(&state, &principal, &headers);
    let s = state
        .db
        .skill(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such skill".to_owned()))?;
    if !can_see(scope.as_deref(), &principal, &s) {
        return Err((StatusCode::FORBIDDEN, "not visible to this box".into()));
    }
    Ok(Json(skill_json(&s)).into_response())
}

/// `POST /skills`: create. A box posting an existing slug it owns updates that skill instead
/// (the agent's `skills_save` is an upsert).
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Json(b): Json<SkillBody>,
) -> ApiResult {
    let now = now_ms();
    let scope = box_project(&state, &principal, &headers);
    let origin = match &principal {
        Principal::Box { .. } => scope.clone(),
        Principal::Device { .. } | Principal::Admin => b.origin_project_id.clone(),
    };
    let base = Skill {
        id: ulid(),
        origin_project_id: origin,
        version: 1,
        created_at: now,
        updated_at: now,
        ..Skill::default()
    };
    let s = b.apply(base);
    if s.name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name required".into()));
    }
    if let Some(existing) = state.db.skill(&s.slug).map_err(internal)? {
        if principal.is_box() && can_edit(scope.as_deref(), &principal, &existing) {
            let merged = Skill {
                id: existing.id.clone(),
                origin_project_id: existing.origin_project_id.clone(),
                created_at: existing.created_at,
                ..s
            };
            let row = state
                .db
                .update_skill(&merged)
                .map_err(internal)?
                .ok_or((StatusCode::NOT_FOUND, "no such skill".to_owned()))?;
            notify_boxes(&state, &row.slug);
            return Ok(Json(skill_json(&row)).into_response());
        }
        return Err((
            StatusCode::CONFLICT,
            format!("a skill with slug {} exists", s.slug),
        ));
    }
    state.db.insert_skill(&s).map_err(internal)?;
    notify_boxes(&state, &s.slug);
    tracing::info!(slug = %s.slug, by = %principal.device_name(), "skill created");
    Ok((StatusCode::CREATED, Json(skill_json(&s))).into_response())
}

/// `PUT /skills/{id}`: edit fields (partial body is fine); bumps the version.
pub async fn update(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<SkillBody>,
) -> ApiResult {
    let scope = box_project(&state, &principal, &headers);
    let existing = state
        .db
        .skill(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such skill".to_owned()))?;
    if !can_edit(scope.as_deref(), &principal, &existing) {
        return Err((StatusCode::FORBIDDEN, "not this box's skill".into()));
    }
    let next = b.apply(existing.clone());
    if next.name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name required".into()));
    }
    if next.slug != existing.slug {
        if let Some(other) = state.db.skill(&next.slug).map_err(internal)? {
            if other.id != existing.id {
                return Err((
                    StatusCode::CONFLICT,
                    format!("a skill with slug {} exists", next.slug),
                ));
            }
        }
    }
    let row = state
        .db
        .update_skill(&next)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such skill".to_owned()))?;
    notify_boxes(&state, &row.slug);
    tracing::info!(slug = %row.slug, version = row.version, by = %principal.device_name(), "skill updated");
    Ok(Json(skill_json(&row)).into_response())
}

/// `DELETE /skills/{id}` (devices only).
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    if principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "boxes cannot delete skills".into()));
    }
    let existing = state
        .db
        .skill(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such skill".to_owned()))?;
    state.db.delete_skill(&existing.id).map_err(internal)?;
    notify_boxes(&state, &existing.slug);
    tracing::info!(slug = %existing.slug, "skill deleted");
    Ok(StatusCode::NO_CONTENT.into_response())
}
