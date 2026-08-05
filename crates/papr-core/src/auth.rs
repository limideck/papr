//! Password hashing and session helpers for the multi-user Web server.
//!
//! Hash format: `v1$<hex-salt>$<hex-sha256(salt || password)>`. Sessions are
//! opaque random tokens stored in the `sessions` table.

use crate::error::{AppError, AppResult};
use hex::{decode as hex_decode, encode as hex_encode};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SALT_LEN: usize = 16;
const TOKEN_LEN: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub created_at: String,
}

/// Hash a password for storage.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    let digest = Sha256::new()
        .chain_update(salt)
        .chain_update(password.as_bytes())
        .finalize();
    format!("v1${}${}", hex_encode(salt), hex_encode(digest))
}

/// Verify a password against a stored `hash_password` string.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("v1"), Some(salt_hex), Some(hash_hex), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Ok(salt) = hex_decode(salt_hex) else {
        return false;
    };
    let Ok(expected) = hex_decode(hash_hex) else {
        return false;
    };
    let digest = Sha256::new()
        .chain_update(&salt)
        .chain_update(password.as_bytes())
        .finalize();
    // Constant-time-ish compare for equal-length digests.
    expected.len() == digest.len() && expected.iter().zip(digest.iter()).all(|(a, b)| a == b)
}

/// Generate a new opaque session token (hex).
pub fn new_session_token() -> String {
    let mut bytes = [0u8; TOKEN_LEN];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex_encode(bytes)
}

pub fn create_user(
    conn: &Connection,
    username: &str,
    password: &str,
    is_admin: bool,
) -> AppResult<i64> {
    let username = username.trim();
    if username.is_empty() {
        return Err(AppError::code("emptyUsername"));
    }
    if password.len() < 6 {
        return Err(AppError::code("passwordTooShort"));
    }
    let hash = hash_password(password);
    conn.execute(
        "INSERT INTO users(username, password_hash, is_admin) VALUES (?1, ?2, ?3)",
        params![username, hash, is_admin as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn find_user_by_username(conn: &Connection, username: &str) -> AppResult<Option<(User, String)>> {
    Ok(conn
        .query_row(
            "SELECT id, username, is_admin, created_at, password_hash
             FROM users WHERE username = ?1 COLLATE NOCASE",
            params![username.trim()],
            |r| {
                Ok((
                    User {
                        id: r.get(0)?,
                        username: r.get(1)?,
                        is_admin: r.get::<_, i64>(2)? != 0,
                        created_at: r.get(3)?,
                    },
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?)
}

pub fn get_user(conn: &Connection, id: i64) -> AppResult<Option<User>> {
    Ok(conn
        .query_row(
            "SELECT id, username, is_admin, created_at FROM users WHERE id = ?1",
            params![id],
            |r| {
                Ok(User {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    is_admin: r.get::<_, i64>(2)? != 0,
                    created_at: r.get(3)?,
                })
            },
        )
        .optional()?)
}

pub fn list_users(conn: &Connection) -> AppResult<Vec<User>> {
    let mut stmt =
        conn.prepare("SELECT id, username, is_admin, created_at FROM users ORDER BY id")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                is_admin: r.get::<_, i64>(2)? != 0,
                created_at: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn delete_user(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM users WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn set_password(conn: &Connection, id: i64, password: &str) -> AppResult<()> {
    if password.len() < 6 {
        return Err(AppError::code("passwordTooShort"));
    }
    let hash = hash_password(password);
    conn.execute(
        "UPDATE users SET password_hash = ?2 WHERE id = ?1",
        params![id, hash],
    )?;
    Ok(())
}

/// Promote or demote a user. Returns an error if the user does not exist.
pub fn set_user_admin(conn: &Connection, id: i64, is_admin: bool) -> AppResult<()> {
    let n = conn.execute(
        "UPDATE users SET is_admin = ?2 WHERE id = ?1",
        params![id, is_admin as i64],
    )?;
    if n == 0 {
        return Err(AppError::code("userNotFound"));
    }
    Ok(())
}

pub fn create_session(conn: &Connection, user_id: i64) -> AppResult<String> {
    let token = new_session_token();
    conn.execute(
        "INSERT INTO sessions(token, user_id) VALUES (?1, ?2)",
        params![token, user_id],
    )?;
    Ok(token)
}

pub fn delete_session(conn: &Connection, token: &str) -> AppResult<()> {
    conn.execute("DELETE FROM sessions WHERE token = ?1", params![token])?;
    Ok(())
}

pub fn delete_user_sessions(conn: &Connection, user_id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
    Ok(())
}

/// Resolve a session token to the owning user, if still valid.
pub fn user_for_session(conn: &Connection, token: &str) -> AppResult<Option<User>> {
    if token.is_empty() {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT u.id, u.username, u.is_admin, u.created_at
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token = ?1
               AND (s.expires_at IS NULL OR datetime(s.expires_at) > datetime('now'))",
            params![token],
            |r| {
                Ok(User {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    is_admin: r.get::<_, i64>(2)? != 0,
                    created_at: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Ensure an admin user exists with the given credentials. Idempotent: if the
/// username already exists, its password is updated when `reset_password` is
/// true; otherwise the existing row is left alone. Returns the user id.
pub fn ensure_admin(
    conn: &Connection,
    username: &str,
    password: &str,
    reset_password: bool,
) -> AppResult<i64> {
    if let Some((user, _)) = find_user_by_username(conn, username)? {
        if reset_password {
            set_password(conn, user.id, password)?;
        }
        // Promote to admin if somehow not.
        conn.execute(
            "UPDATE users SET is_admin = 1 WHERE id = ?1",
            params![user.id],
        )?;
        return Ok(user.id);
    }
    create_user(conn, username, password, true)
}

/// Copy legacy article-level read/star/later flags into `user_article_states`
/// for `user_id` (only rows that have any flag set). Used once when seeding
/// the first admin so an upgraded single-user DB keeps its marks.
pub fn migrate_article_states_to_user(conn: &Connection, user_id: i64) -> AppResult<usize> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO user_article_states(user_id, article_id, is_read, is_starred, read_later)
         SELECT ?1, id, is_read, is_starred, read_later FROM articles
         WHERE is_read = 1 OR is_starred = 1 OR read_later = 1",
        params![user_id],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip() {
        let h = hash_password("secret12");
        assert!(verify_password("secret12", &h));
        assert!(!verify_password("wrong", &h));
    }
}
