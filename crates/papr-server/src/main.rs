//! papr-server — Web-first HTTP API + static frontend + background jobs.

mod auth;
mod balance;
mod error;
mod jobs;
mod routes;
mod state;

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use papr_core::auth as core_auth;
use papr_core::ingestion::feed_source;
use serde_json::json;
use state::AppState;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let db_path = std::env::var("PAPR_DB").unwrap_or_else(|_| "papr.db".into());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let static_dir = resolve_static_dir();

    let state = AppState::new(Path::new(&db_path))?;
    seed_admin_and_sources(&state).await?;

    jobs::spawn_background_jobs(state.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut app = Router::new()
        .merge(routes::api_router())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state.clone());

    if let Some(dir) = &static_dir {
        if dir.is_dir() {
            tracing::info!(path = %dir.display(), "serving frontend static files");
            let index = dir.join("index.html");
            let serve = ServeDir::new(dir)
                .not_found_service(ServeFile::new(index));
            app = app.fallback_service(serve);
        } else {
            tracing::warn!(path = %dir.display(), "PAPR_STATIC_DIR is not a directory");
            app = app.fallback(get(no_frontend));
        }
    } else {
        app = app.fallback(get(no_frontend));
    }

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, db = %db_path, "papr-server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn no_frontend(uri: Uri) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "code": "noFrontend",
            "detail": format!(
                "no static frontend at {} — build with `pnpm build` and set PAPR_STATIC_DIR, or call /api/*",
                uri.path()
            ),
        })
        .to_string(),
    )
        .into_response()
}

fn resolve_static_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PAPR_STATIC_DIR") {
        return Some(PathBuf::from(p));
    }
    // Common locations relative to the binary cwd.
    for candidate in ["dist", "../dist", "../../dist"] {
        let p = PathBuf::from(candidate);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

async fn seed_admin_and_sources(state: &AppState) -> anyhow::Result<()> {
    let admin_user = std::env::var("PAPR_ADMIN_USER").unwrap_or_else(|_| "admin".into());
    let admin_pass = std::env::var("PAPR_ADMIN_PASSWORD").ok();
    let reset = std::env::var("PAPR_ADMIN_RESET")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let conn = state.db.lock().await;
    let user_count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;

    if let Some(pass) = admin_pass {
        let id = core_auth::ensure_admin(&conn, &admin_user, &pass, reset || user_count == 0)?;
        if user_count == 0 {
            let n = core_auth::migrate_article_states_to_user(&conn, id)?;
            tracing::info!(user = %admin_user, migrated_states = n, "seeded admin user");
        } else {
            tracing::info!(user = %admin_user, "ensured admin user");
        }
    } else if user_count == 0 {
        // Dev-friendly default when no password is configured.
        let id = core_auth::ensure_admin(&conn, &admin_user, "admin123", true)?;
        let n = core_auth::migrate_article_states_to_user(&conn, id)?;
        tracing::warn!(
            user = %admin_user,
            password = "admin123",
            migrated_states = n,
            "no PAPR_ADMIN_PASSWORD set — seeded default admin (change this)"
        );
    }

    if let Ok(url) = std::env::var("PAPR_FEED_SOURCE_URL") {
        if let Some(id) = feed_source::seed_feed_source(&conn, &url)? {
            tracing::info!(id, url = %url, "seeded feed source");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tower::ServiceExt;

    // macOS `SystemTime` has microsecond resolution, so two parallel tests
    // calling `test_app()` within the same microsecond would collide on the
    // same temp path — one would open the other's half-migrated DB. The atomic
    // sequence makes every path unique within the process.
    static TEST_DB_SEQ: AtomicU64 = AtomicU64::new(0);

    fn test_app() -> Router {
        let seq = TEST_DB_SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "papr-server-test-{}-{nanos}-{seq}.db",
            std::process::id(),
        ));
        let state = AppState::new(&path).expect("test state");
        routes::api_router().with_state(state)
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn article_routes_reject_anonymous_requests() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/articles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
