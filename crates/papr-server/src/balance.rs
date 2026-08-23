//! Official DeepSeek balance & usage fetchers for the admin cost view.
//!
//! - `fetch_balance` uses the **documented** `/user/balance` endpoint,
//!   authenticated with the normal API key — this is the source of truth for
//!   real money spent (consecutive totals give per-day spend).
//! - `fetch_monthly_usage` targets the **undocumented** platform dashboard
//!   endpoints (`platform.deepseek.com/api/v0/usage/*`) which require a
//!   signed-in platform session token, NOT the API key. They are best-effort:
//!   a token may be absent or expired, and DeepSeek may change the response
//!   shape without notice. On any failure the caller simply keeps the balance
//!   data and skips the official token/cost rows.

use papr_core::db::OfficialUsageDay;
use serde_json::Value;

/// One official balance snapshot (CNY, matching the account currency).
#[derive(Debug, Clone)]
pub struct BalanceSnapshot {
    pub total: f64,
    pub granted: f64,
    pub topped_up: f64,
}

fn parse_cny(v: Option<&Value>) -> Option<f64> {
    // The API returns balances as numeric strings ("478.64").
    v.and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
}

/// `GET /user/balance` — the only documented DeepSeek usage API. Returns the
/// first currency bucket (the account's own currency).
pub async fn fetch_balance(client: &reqwest::Client, api_key: &str) -> anyhow::Result<BalanceSnapshot> {
    let resp = client
        .get("https://api.deepseek.com/user/balance")
        .bearer_auth(api_key)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("balance endpoint {status}: {body}");
    }
    let json: Value = serde_json::from_str(&body)?;
    let info = json
        .get("balance_infos")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow::anyhow!("no balance_infos in response: {body}"))?;
    Ok(BalanceSnapshot {
        total: parse_cny(info.get("total_balance")).ok_or_else(|| {
            anyhow::anyhow!("unparsable total_balance in: {body}")
        })?,
        granted: parse_cny(info.get("granted_balance")).unwrap_or(0.0),
        topped_up: parse_cny(info.get("topped_up_balance")).unwrap_or(0.0),
    })
}

/// Pull a numeric field out of a dashboard usage object, trying the plausible
/// keys for each endpoint (they are undocumented and may drift).
fn pick_number(obj: &Value, keys: &[&str]) -> Option<i64> {
    for k in keys {
        if let Some(v) = obj.get(k) {
            if let Some(n) = v.as_i64() {
                return Some(n);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<i64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// The day key of a dashboard usage object (date / day / time / dateStr…).
fn pick_day(obj: &Value) -> Option<String> {
    for k in ["date", "day", "time", "dateStr", "usageDate"] {
        if let Some(v) = obj.get(k) {
            if let Some(s) = v.as_str() {
                let d = s.trim();
                if d.len() >= 10 {
                    return Some(d[..10].to_string());
                }
            }
        }
    }
    None
}

/// One best-effort fetch of a platform usage endpoint. Returns a raw
/// `(day, value)` map keyed by day, logging the body when the shape is
/// unrecognised so the parser can be fixed without silent failure.
async fn fetch_usage_endpoint(
    client: &reqwest::Client,
    token: &str,
    path: &str,
    month: u32,
    year: i32,
    value_keys: &[&str],
) -> Vec<(String, i64)> {
    let url = format!(
        "https://platform.deepseek.com/api/v0/{path}?month={month}&year={year}"
    );
    let resp = match client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("deepseek platform {path} request failed: {e}");
            return Vec::new();
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        log::warn!("deepseek platform {path} {status}: {}", &body[..body.len().min(200)]);
        return Vec::new();
    }
    let json: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("deepseek platform {path} unparsable ({e}): {}", &body[..body.len().min(300)]);
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for obj in json.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default() {
        let Some(day) = pick_day(&obj) else { continue };
        let Some(value) = pick_number(&obj, value_keys) else { continue };
        out.push((day, value));
    }
    if out.is_empty() {
        log::warn!(
            "deepseek platform {path}: no recognisable rows in: {}",
            &body[..body.len().min(300)]
        );
    }
    out
}

/// Merge the platform dashboard's monthly amount + cost endpoints into daily
/// official usage rows. Empty on any failure (absent/expired token, changed
/// response shape) — callers degrade gracefully.
pub async fn fetch_monthly_usage(
    client: &reqwest::Client,
    token: &str,
    year: i32,
    month: u32,
) -> Vec<OfficialUsageDay> {
    let amounts = fetch_usage_endpoint(client, token, "usage/amount", month, year, &["tokens", "amount", "total", "tokenAmount"]).await;
    let costs = fetch_usage_endpoint(client, token, "usage/cost", month, year, &["cost", "total", "amount", "spend"]).await;

    let mut by_day: std::collections::BTreeMap<String, (i64, f64)> = std::collections::BTreeMap::new();
    for (day, tokens) in amounts {
        by_day.entry(day).or_insert((0, 0.0)).0 = tokens;
    }
    for (day, cost_raw) in costs {
        let cost = cost_raw as f64 / 100.0; // dashboard costs are typically in cents
        by_day.entry(day).or_insert((0, 0.0)).1 = cost;
    }
    by_day
        .into_iter()
        .map(|(day, (tokens, cost))| OfficialUsageDay { day, tokens, cost })
        .collect()
}
