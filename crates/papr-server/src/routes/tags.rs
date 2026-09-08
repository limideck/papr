use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::Json;
use papr_core::db;
use rusqlite::OptionalExtension;
use papr_core::error::AppError;
use papr_core::models::{TAG_KIND_AI, TAG_KIND_INTEREST};
use papr_core::user_db;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct ListQuery {
    /// Optional `interest` | `ai` filter.
    pub kind: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let kind = q.kind.as_deref();
    Ok(Json(json!(
        user_db::list_tags_for_user(&conn, user.id(), kind).map_err(ApiError::from)?
    )))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub name: String,
    /// Defaults to `interest` (admin vocabulary). AI tags are normally
    /// created by the worker; admins may still create them explicitly.
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    TAG_KIND_INTEREST.to_string()
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_tag(&conn, &body.name, &body.kind).map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    pub name: Option<String>,
    pub color: Option<String>,
}

/// `GET /api/tags/{id}` — single-tag admin info (type/parent/domain) for the
/// tag-management detail panel.
pub async fn get_one(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let row: Option<(String, Option<String>, Option<i64>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT t.name, t.tag_type, t.parent_id, t.domain,
                    (SELECT p.name FROM tags p WHERE p.id = t.parent_id)
             FROM tags t WHERE t.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(papr_core::error::AppError::from)
        .map_err(ApiError::from)?;
    let Some((name, tag_type, parent_id, domain, parent_name)) = row else {
        return Err(ApiError::from(AppError::code("tagNotFound")));
    };
    Ok(Json(json!({
        "id": id,
        "name": name,
        "tagType": tag_type,
        "parentId": parent_id,
        "parentName": parent_name,
        "domain": domain,
    })))
}

pub async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    if let Some(name) = body.name {
        db::rename_tag(&conn, id, &name).map_err(ApiError::from)?;
    }
    if let Some(color) = body.color {
        db::set_tag_color(&conn, id, &color).map_err(ApiError::from)?;
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_tag(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct CleanupEmptyBody {
    /// Must be `ai`. Interest tags are never bulk-deleted when unused.
    pub kind: String,
}

/// Delete unused AI tags (`article_count = 0`). Admin only.
/// Interest cleanup is rejected — empty interest tags stay as vocabulary.

/// `POST /api/tags/{id}/merge` — merge tag `{id}` into `targetTagId` (same
/// kind): every article of the source tag is re-attached to the target, then
/// the source tag is deleted. Admin only. Used to repair AI-taxonomy
/// fragmentation (near-synonym tags split across dozens of spellings).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeBody {
    pub target_tag_id: i64,
}

pub async fn merge(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<MergeBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let moved = db::merge_tags(&conn, id, body.target_tag_id).map_err(ApiError::from)?;
    Ok(Json(json!({ "moved": moved })))
}

pub async fn cleanup_empty(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CleanupEmptyBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let deleted = db::delete_empty_tags(&conn, &body.kind).map_err(ApiError::from)?;
    Ok(Json(json!({ "deleted": deleted })))
}

#[derive(Deserialize)]
pub struct ReorderBody {
    pub ids: Vec<i64>,
}

pub async fn reorder(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ReorderBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::reorder_tags(&conn, &body.ids).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SetTagBody {
    pub on: bool,
}

pub async fn set_article_tag(
    State(state): State<AppState>,
    user: AuthUser,
    Path((article_id, tag_id)): Path<(i64, i64)>,
    Json(body): Json<SetTagBody>,
) -> ApiResult<Json<Value>> {
    // Attach (and create-via-toggle) stays admin-only. Any authenticated
    // reader may detach a tag from an article — especially useful for
    // clearing unwanted AI tags without leaving the reader.
    if body.on {
        user.require_admin()?;
    }
    let conn = state.db.lock().await;
    db::set_article_tag(&conn, article_id, tag_id, body.on).map_err(ApiError::from)?;
    // Feedback loop, keyed on AI tags only (interest is the admin's own
    // vocabulary — removals there carry no signal):
    //  • detach records a *dismissal* → the worker skips this tag on every
    //    future article until an admin restores it;
    //  • explicit re-attach withdraws the dismissal (the reader is telling us
    //    the tag belongs after all).
    let kind = db::tag_kind(&conn, tag_id).map_err(ApiError::from)?;
    if kind.as_deref() == Some(TAG_KIND_AI) {
        if body.on {
            db::unsuppress_tag(&conn, tag_id).map_err(ApiError::from)?;
        } else {
            db::record_tag_dismissal(&conn, tag_id).map_err(ApiError::from)?;
        }
    }
    Ok(Json(json!({ "ok": true })))
}

// ─────────────────── feedback loop: review queue / suppression ───────────────────

#[derive(Deserialize)]
pub struct ReviewQueueQuery {
    /// `new` | `single` | `unparented` | `all` (default `new`).
    #[serde(default = "default_review_filter")]
    pub filter: String,
    #[serde(default = "default_zero")]
    pub offset: i64,
    #[serde(default = "default_page_size")]
    pub limit: i64,
}

fn default_review_filter() -> String {
    "new".to_string()
}
fn default_zero() -> i64 {
    0
}
fn default_page_size() -> i64 {
    50
}

/// `GET /api/tags/review-queue` — unreviewed AI tags needing an admin
/// decision (confirm / suppress / delete / nest). Reader can view; mutations
/// are admin-only.
pub async fn review_queue(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ReviewQueueQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let (total, items) = db::review_queue(&conn, &q.filter, q.offset.max(0), q.limit.clamp(1, 100))
        .map_err(ApiError::from)?;
    Ok(Json(json!({ "total": total, "items": items })))
}

/// `POST /api/tags/{id}/review` — mark one tag as triaged (leaves the queue).
pub async fn confirm_review(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::confirm_tag_reviewed(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/tags/{id}/suppress` — stop auto-attaching this tag everywhere.
pub async fn suppress(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::record_tag_dismissal(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

/// `DELETE /api/tags/{id}/suppress` — restore a suppressed tag.
pub async fn unsuppress(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::unsuppress_tag(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SuppressedQuery {
    #[serde(default = "default_zero")]
    pub offset: i64,
    #[serde(default = "default_page_size")]
    pub limit: i64,
}

/// `GET /api/tags/suppressed` — tags readers dismissed (or an admin blocked);
/// the restore surface for the feedback loop.
pub async fn suppressed_list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<SuppressedQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    let (total, items) =
        db::list_suppressed_tags(&conn, q.offset.max(0), q.limit.clamp(1, 100))
            .map_err(ApiError::from)?;
    Ok(Json(json!({ "total": total, "items": items })))
}

/// `POST /api/tags/{id}/hierarchy` — manual nesting / type edit from the tag
/// management UI. `parentName` may be a topic name (already existing in the
/// tag's own vocabulary) or JSON `null` to detach; omitting it leaves the
/// parent untouched. `tagType` is `"entity"` | `"topic"` | `null` (clear).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HierarchyBody {
    /// `None` = leave the parent as-is; `Some(None)` = clear it;
    /// `Some(Some(name))` = nest under that topic.
    #[serde(default)]
    pub parent_name: Option<Option<String>>,
    #[serde(default)]
    pub tag_type: Option<Option<String>>,
    /// L1 domain: `Some(Some(name))` sets, `Some(None)` clears.
    #[serde(default)]
    pub domain: Option<Option<String>>,
}

pub async fn set_hierarchy(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<HierarchyBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;

    let kind = db::tag_kind(&conn, id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::from(AppError::code("tagNotFound")))?;

    if let Some(ty) = &body.tag_type {
        db::set_tag_type(&conn, id, ty.as_deref()).map_err(ApiError::from)?;
    }
    if let Some(dom) = &body.domain {
        db::set_tag_domain(&conn, id, dom.as_deref()).map_err(ApiError::from)?;
    }
    if let Some(parent) = &body.parent_name {
        match parent {
            None => db::set_tag_parent(&conn, id, None).map_err(ApiError::from)?,
            Some(name) => {
                let parent_id = db::find_tag_id_by_name(&conn, &kind, name)
                    .map_err(ApiError::from)?
                    .ok_or_else(|| ApiError::from(AppError::code("tagNotFound")))?;
                db::validate_tag_parent_link(&conn, id, parent_id).map_err(ApiError::from)?;
                db::set_tag_parent(&conn, id, Some(parent_id)).map_err(ApiError::from)?;
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ListAliasesQuery {
    pub tag_id: Option<i64>,
    /// Optional `interest` | `ai` filter.
    pub kind: Option<String>,
}

pub async fn list_aliases(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListAliasesQuery>,
) -> ApiResult<Json<Value>> {
    let conn = state.db.lock().await;
    Ok(Json(json!(
        db::list_tag_aliases(&conn, q.tag_id, q.kind.as_deref()).map_err(ApiError::from)?
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAliasBody {
    pub tag_id: i64,
    pub alias: String,
}

pub async fn create_alias(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateAliasBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    let id = db::create_tag_alias(&conn, body.tag_id, &body.alias).map_err(ApiError::from)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct UpdateAliasBody {
    pub alias: String,
}

pub async fn update_alias(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdateAliasBody>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::rename_tag_alias(&conn, id, &body.alias).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_alias(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    user.require_admin()?;
    let conn = state.db.lock().await;
    db::delete_tag_alias(&conn, id).map_err(ApiError::from)?;
    Ok(Json(json!({ "ok": true })))
}
