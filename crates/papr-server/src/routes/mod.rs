//! REST JSON API routes.

mod articles;
mod ai;
mod auth_routes;
mod auto_tag;
mod feed_sources;
mod feeds;
mod folders;
mod highlights;
mod misc;
mod opml;
mod rules;
mod settings;
mod stats;
mod tags;
mod users;
mod wordcloud;

use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use crate::state::AppState;

pub fn api_router() -> Router<AppState> {
    Router::new()
        // auth
        .route("/api/auth/login", post(auth_routes::login))
        .route("/api/auth/logout", post(auth_routes::logout))
        .route("/api/auth/me", get(auth_routes::me))
        // users (admin)
        .route("/api/users", get(users::list).post(users::create))
        .route(
            "/api/users/{id}",
            patch(users::patch).delete(users::delete),
        )
        .route("/api/users/me/password", post(users::change_password))
        // folders
        .route("/api/folders", get(folders::list).post(folders::create))
        .route(
            "/api/folders/{id}",
            patch(folders::rename).delete(folders::delete),
        )
        // feeds
        .route("/api/feeds", get(feeds::list).post(feeds::add))
        .route("/api/feeds/discover", get(feeds::discover))
        .route("/api/feeds/refresh", post(feeds::refresh))
        .route(
            "/api/feeds/{id}",
            patch(feeds::update).delete(feeds::delete),
        )
        // articles
        .route("/api/articles", get(articles::list).post(articles::list_post))
        .route("/api/articles/index", post(articles::index))
        .route("/api/articles/mark-all-read", post(articles::mark_all_read))
        .route("/api/articles/{id}", get(articles::get))
        .route("/api/articles/{id}/read", put(articles::mark_read))
        .route("/api/articles/{id}/starred", put(articles::mark_starred))
        .route("/api/articles/{id}/read-later", put(articles::mark_read_later))
        .route(
            "/api/articles/{id}/extract",
            post(articles::extract_fulltext),
        )
        .route(
            "/api/articles/{id}/translate-preview",
            post(ai::translate_preview),
        )
        .route("/api/ai/summarize", post(ai::summarize))
        .route("/api/ai/ask", post(ai::ask))
        .route("/api/ai/digest", post(ai::digest))
        .route("/api/ai/translate", post(ai::translate))
        .route("/api/ai/usage", get(ai::usage))
        .route("/api/smart-counts", get(articles::smart_counts))
        // OPML
        .route("/api/opml/import", post(opml::import))
        .route("/api/opml/export", get(opml::export))
        // settings
        .route("/api/settings/{key}", get(settings::get).put(settings::set))
        // auto-tag (admin)
        .route("/api/auto-tag/status", get(auto_tag::status))
        .route("/api/auto-tag/backfill", post(auto_tag::backfill))
        // stats (admin)
        .route("/api/stats/overview", get(stats::overview))
        // tags
        .route("/api/tags", get(tags::list).post(tags::create))
        .route(
            "/api/tags/{id}",
            patch(tags::update).delete(tags::delete),
        )
        .route("/api/tags/reorder", post(tags::reorder))
        .route(
            "/api/articles/{article_id}/tags/{tag_id}",
            put(tags::set_article_tag),
        )
        // rules
        .route("/api/rules", get(rules::list).post(rules::create))
        .route(
            "/api/rules/{id}",
            put(rules::update).delete(rules::delete),
        )
        .route("/api/rules/preview", post(rules::preview))
        .route("/api/rules/apply", post(rules::apply))
        // feed sources (admin)
        .route(
            "/api/feed-sources",
            get(feed_sources::list).post(feed_sources::create),
        )
        .route(
            "/api/feed-sources/{id}",
            delete(feed_sources::delete),
        )
        .route("/api/feed-sources/{id}/scan", post(feed_sources::scan_one))
        .route("/api/feed-sources/scan", post(feed_sources::scan_all))
        // wordcloud
        .route("/api/wordcloud", get(wordcloud::get))
        .route("/api/wordcloud/stopwords", get(wordcloud::get_stopwords))
        .route("/api/wordcloud/entities", get(wordcloud::get_entities))
        .route("/api/wordcloud/status", get(wordcloud::status))
        .route("/api/wordcloud/backfill", post(wordcloud::backfill))
        // highlights
        .route("/api/highlights", get(highlights::list).post(highlights::create))
        .route(
            "/api/highlights/{id}",
            patch(highlights::patch).delete(highlights::delete),
        )
        // misc
        .route("/api/fetch-image", get(misc::fetch_image))
        .route("/api/storage/stats", get(misc::storage_stats))
        .route("/api/storage/cleanup", post(misc::cleanup))
        .route("/api/storage/vacuum", post(misc::vacuum))
        .route("/api/storage/clear", post(misc::clear_all_data))
        .route("/api/storage/reset-settings", post(misc::reset_settings))
        .route("/api/health", get(misc::health))
}
