//! Shared server state: SQLite writer mutex + HTTP client + word-cloud dict.

use papr_core::auth::User;
use papr_core::db;
use papr_core::wordcloud_dict::SharedWordCloudDict;
use rusqlite::Connection;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub http: reqwest::Client,
    pub wordcloud: Arc<SharedWordCloudDict>,
    /// Count of in-flight manual `POST /api/articles/{id}/auto-tag` calls.
    /// Background workers skip claiming while this is non-zero so a reader
    /// click does not compete with the backlog for LLM / DB lock time.
    pub auto_tag_manual_inflight: Arc<AtomicUsize>,
}

impl AppState {
    pub fn new(db_path: &std::path::Path) -> anyhow::Result<Self> {
        let conn = db::open(db_path)?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("papr-server/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let wordcloud = Arc::new(SharedWordCloudDict::load_default());
        papr_core::wordcloud_dict::install_process_dict(wordcloud.clone());
        // Seed dict version + file fingerprint so ingest/backfill stay aligned.
        {
            use papr_core::wordcloud;
            let _ = wordcloud::ensure_dict_version(&conn);
            let _ = wordcloud.with_dict(|dict| wordcloud::sync_dict_file_version(&conn, dict));
        }
        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            http,
            wordcloud,
            auto_tag_manual_inflight: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Raise the manual-priority gate for the duration of a sync auto-tag.
    /// Cleared on drop (success, error, or cancel).
    pub fn begin_manual_auto_tag(&self) -> ManualAutoTagGuard {
        self.auto_tag_manual_inflight
            .fetch_add(1, Ordering::SeqCst);
        ManualAutoTagGuard(self.auto_tag_manual_inflight.clone())
    }

    pub fn manual_auto_tag_busy(&self) -> bool {
        self.auto_tag_manual_inflight.load(Ordering::Acquire) > 0
    }
}

/// Decrements [`AppState::auto_tag_manual_inflight`] when dropped.
pub struct ManualAutoTagGuard(Arc<AtomicUsize>);

impl Drop for ManualAutoTagGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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
