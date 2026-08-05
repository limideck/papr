//! Shared server state: SQLite writer mutex + HTTP client.

use papr_core::auth::User;
use papr_core::db;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(db_path: &std::path::Path) -> anyhow::Result<Self> {
        let conn = db::open(db_path)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("papr-server/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            http,
        })
    }
}

/// Authenticated request context extracted from the session cookie.
#[derive(Clone, Debug)]
pub struct AuthUser(pub User);

impl AuthUser {
    pub fn id(&self) -> i64 {
        self.0.id
    }

    pub fn require_admin(&self) -> Result<(), crate::error::ApiError> {
        if self.0.is_admin {
            Ok(())
        } else {
            Err(crate::error::ApiError::forbidden())
        }
    }
}
