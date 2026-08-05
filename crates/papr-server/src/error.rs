//! HTTP error mapping for papr-server.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use papr_core::error::AppError;
use serde_json::json;

pub struct ApiError {
    pub status: StatusCode,
    pub error: AppError,
}

impl ApiError {
    pub fn new(status: StatusCode, error: AppError) -> Self {
        Self { status, error }
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, AppError::code("unauthorized"))
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, AppError::code("forbidden"))
    }

    pub fn bad_request(code: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, AppError::code(code))
    }

    pub fn other(status: StatusCode, msg: impl Into<String>) -> Self {
        Self::new(status, AppError::other(msg))
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        let status = match &error {
            AppError::Coded("unauthorized") => StatusCode::UNAUTHORIZED,
            AppError::Coded("forbidden") => StatusCode::FORBIDDEN,
            AppError::Coded("alreadySubscribed")
            | AppError::Coded("emptyFolderName")
            | AppError::Coded("folderNameExists")
            | AppError::Coded("passwordTooShort")
            | AppError::Coded("emptyUsername")
            | AppError::Coded("invalidIndexUrl")
            | AppError::Coded("noFeedFound")
            | AppError::Coded("badImageUrl") => StatusCode::BAD_REQUEST,
            AppError::Db(_) | AppError::Migration(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        };
        Self { status, error }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "code": match &self.error {
                AppError::Coded(c) => c.to_string(),
                AppError::Db(_) => "db".into(),
                AppError::Migration(_) => "migration".into(),
                AppError::Http(_) => "network".into(),
                AppError::FeedParse(_) => "feedParse".into(),
                AppError::Opml(_) => "opml".into(),
                AppError::Other(_) => "other".into(),
            },
            "detail": match &self.error {
                AppError::Other(s) | AppError::Opml(s) => Some(s.clone()),
                AppError::Db(e) => Some(e.to_string()),
                AppError::Migration(e) => Some(e.to_string()),
                AppError::Http(e) => Some(e.to_string()),
                AppError::FeedParse(e) => Some(e.to_string()),
                AppError::Coded(_) => None,
            },
        });
        (self.status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
