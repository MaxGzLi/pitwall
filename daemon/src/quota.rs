//! Rate-limit windows and balances. On subscription plans the percentage is the
//! real budget; dollars are an estimate we compute ourselves.
//!
//! Every source is best-effort: one missing file or a dead endpoint must never
//! stop the others from being written.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tracing::{debug, warn};

use crate::config::{deepseek_key, Config};
use crate::model::{now_ms, QuotaRow};
use crate::store::Store;

/// ~/.claude.json is the whole Claude Code config, not just usage; refuse to
/// parse it if it has grown into something that would cost real memory.
const MAX_DOTJSON_BYTES: u64 = 64 * 1024 * 1024;
/// Codex rollouts reach 100MB+; the token_count we want is always near the end.
const ROLLOUT_TAIL_BYTES: u64 = 256 * 1024;
/// The newest rollout may have been opened before its first turn, so look back
/// over a few sessions. Rate limits are account-wide, any recent one will do.
const ROLLOUT_CANDIDATES: usize = 5;

pub async fn refresh(cfg: &Config, store: &Store) -> Result<()> {
    let mut rows: Vec<QuotaRow> = Vec::new();

    claude_hud(cfg, &mut rows);
    claude_dotjson(cfg, &mut rows);
    codex(cfg, &mut rows);
    if cfg.deepseek_balance {
        deepseek(cfg, &mut rows).await;
    }

    for row in &rows {
        if !store.put_quota(row)? {
            // Not fatal, but worth a line: it means a source that normally wins
            // this window went quiet, and the panel is still showing the value
            // it left behind.
            warn!(
                provider = %row.provider, window = %row.window, source = %row.source,
                "older than the stored sample, kept the stored one"
            );
        }
    }
    debug!(rows = rows.len(), "quota refreshed");
    Ok(())
}

// -- anthropic ----------------------------------------------------------

/// The claude-hud statusline plugin refreshes this every 5 minutes, which makes
/// it the freshest local view of the Claude limits.
fn claude_hud(cfg: &Config, rows: &mut Vec<QuotaRow>) {
    let path = cfg.claude_hud_cache();
    let Some(root) = read_json(&path) else { return };

    // `data` goes null-filled whenever the plugin's own API call fails, and the
    // plugin keeps the last successful read beside it.
    let data = [root.get("data"), root.get("lastGoodData")]
        .into_iter()
        .flatten()
        .find(|d| ["fiveHour", "sevenDay"].iter().any(|k| d.get(k).and_then(Value::as_f64).is_some()));
    let Some(data) = data else {
        debug!(path = %path.display(), "hud cache holds no usable percentages");
        return;
    };

    let sampled_at_ms = root.get("timestamp").and_then(Value::as_i64).unwrap_or_else(now_ms);
    let plan = data.get("planName").and_then(Value::as_str).map(str::to_string);
    let source = path.display().to_string();

    for (window, percent_key, reset_key) in [
        ("5h", "fiveHour", "fiveHourResetAt"),
        ("7d", "sevenDay", "sevenDayResetAt"),
    ] {
        let Some(percent) = data.get(percent_key).and_then(Value::as_f64) else { continue };
        merge(
            rows,
            QuotaRow {
                provider: "anthropic".into(),
                window: window.into(),
                used_percent: Some(percent),
                balance: None,
                currency: None,
                plan: plan.clone(),
                resets_at_ms: data.get(reset_key).and_then(Value::as_str).and_then(rfc3339_ms),
                sampled_at_ms,
                source: source.clone(),
            },
        );
    }
}

/// Richer (per-model buckets, dollar figures) but only refreshed when Claude Code
/// itself runs, so it fills gaps rather than overriding the hud cache.
fn claude_dotjson(cfg: &Config, rows: &mut Vec<QuotaRow>) {
    let path = cfg.claude_dotjson();
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            debug!(path = %path.display(), error = %e, "no claude config");
            return;
        }
    };
    if meta.len() > MAX_DOTJSON_BYTES {
        debug!(path = %path.display(), bytes = meta.len(), "claude config too large to parse");
        return;
    }
    let root: Value = match File::open(&path).map_err(anyhow::Error::from).and_then(|f| {
        serde_json::from_reader(BufReader::new(f)).map_err(anyhow::Error::from)
    }) {
        Ok(v) => v,
        Err(e) => {
            debug!(path = %path.display(), error = %e, "claude config unreadable");
            return;
        }
    };

    let Some(cached) = root.get("cachedUsageUtilization") else {
        debug!(path = %path.display(), "no cachedUsageUtilization");
        return;
    };
    let sampled_at_ms = cached.get("fetchedAtMs").and_then(Value::as_i64).unwrap_or_else(now_ms);
    let Some(util) = cached.get("utilization").and_then(Value::as_object) else { return };
    let source = path.display().to_string();

    // Most keys here are buckets; the unpopulated ones are JSON null and the
    // non-bucket ones (spend, extra_usage, limits) carry no `utilization` number.
    for (key, bucket) in util {
        let Some(percent) = bucket.get("utilization").and_then(Value::as_f64) else { continue };
        let window = match key.as_str() {
            "five_hour" => "5h",
            "seven_day" => "7d",
            other => other,
        };
        let remaining = bucket.get("remaining_dollars").and_then(Value::as_f64);
        merge(
            rows,
            QuotaRow {
                provider: "anthropic".into(),
                window: window.into(),
                used_percent: Some(percent),
                balance: remaining,
                currency: remaining.map(|_| "USD".into()),
                plan: None,
                resets_at_ms: bucket.get("resets_at").and_then(Value::as_str).and_then(rfc3339_ms),
                sampled_at_ms,
                source: source.clone(),
            },
        );
    }

    let limits = util
        .get("limits")
        .or_else(|| cached.get("limits"))
        .and_then(Value::as_array);
    for limit in limits.into_iter().flatten() {
        if limit.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
            continue;
        }
        let Some(percent) = limit.get("percent").and_then(Value::as_f64) else { continue };
        let scope = limit.pointer("/scope/model");
        let name = scope
            .and_then(|m| m.get("display_name"))
            .and_then(Value::as_str)
            .or_else(|| scope.and_then(|m| m.get("id")).and_then(Value::as_str))
            .unwrap_or("unknown");
        merge(
            rows,
            QuotaRow {
                provider: "anthropic".into(),
                window: format!("weekly:{name}"),
                used_percent: Some(percent),
                balance: None,
                currency: None,
                plan: None,
                resets_at_ms: limit.get("resets_at").and_then(Value::as_str).and_then(rfc3339_ms),
                sampled_at_ms,
                source: source.clone(),
            },
        );
    }
}

// -- openai / codex -----------------------------------------------------

/// Codex has no quota cache; the numbers only exist inside the rollout it wrote
/// them into, and they freeze as soon as the session stops taking turns.
fn codex(cfg: &Config, rows: &mut Vec<QuotaRow>) {
    let dir = cfg.codex_home().join("sessions");
    for path in newest_rollouts(&dir, ROLLOUT_CANDIDATES) {
        let Some(record) = last_token_count(&path) else { continue };
        let Some(limits) = record.pointer("/payload/rate_limits") else { continue };

        // Stale by design: this is when Codex last spoke, not now.
        let sampled_at_ms = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(rfc3339_ms)
            .unwrap_or_else(now_ms);
        let plan = limits.get("plan_type").and_then(Value::as_str).map(str::to_string);
        let source = path.display().to_string();

        if let Some(primary) = limits.get("primary") {
            merge(
                rows,
                QuotaRow {
                    provider: "openai".into(),
                    window: "weekly".into(),
                    used_percent: primary.get("used_percent").and_then(Value::as_f64),
                    balance: None,
                    currency: None,
                    plan: plan.clone(),
                    // seconds here, unlike every other source
                    resets_at_ms: primary.get("resets_at").and_then(Value::as_i64).map(|s| s * 1000),
                    sampled_at_ms,
                    source: source.clone(),
                },
            );
        }

        let credits = limits.get("credits");
        let has_credits = credits.and_then(|c| c.get("has_credits")).and_then(Value::as_bool);
        if has_credits == Some(true) {
            if let Some(balance) = credits.and_then(|c| number(c.get("balance"))) {
                merge(
                    rows,
                    QuotaRow {
                        provider: "openai".into(),
                        window: "balance".into(),
                        used_percent: None,
                        balance: Some(balance),
                        currency: None,
                        plan,
                        resets_at_ms: None,
                        sampled_at_ms,
                        source,
                    },
                );
            }
        }
        return;
    }
    debug!(dir = %dir.display(), "no codex rollout carried a token_count");
}

fn newest_rollouts(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk_rollouts(dir, 3, &mut found);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().take(limit).map(|(_, path)| path).collect()
}

/// Depth is bounded because the layout is fixed at sessions/YYYY/MM/DD.
fn walk_rollouts(dir: &Path, depth: usize, out: &mut Vec<(i64, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_dir() {
            if depth > 0 {
                walk_rollouts(&entry.path(), depth - 1, out);
            }
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        out.push((mtime, entry.path()));
    }
}

fn last_token_count(path: &Path) -> Option<Value> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(ROLLOUT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(ROLLOUT_TAIL_BYTES + 1).read_to_end(&mut buf).ok()?;

    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split('\n');
    if start > 0 {
        lines.next(); // the seek landed mid-record
    }
    let mut last = None;
    for line in lines {
        if !line.contains("token_count") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        if record.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        if record.pointer("/payload/rate_limits").is_some_and(|v| v.is_object()) {
            last = Some(record);
        }
    }
    last
}

// -- deepseek -----------------------------------------------------------

/// The only balance with no local copy anywhere, so the only outbound call.
async fn deepseek(cfg: &Config, rows: &mut Vec<QuotaRow>) {
    let Some(key) = deepseek_key(cfg) else {
        debug!("no deepseek key; skipping balance");
        return;
    };
    let url = format!("{}/user/balance", cfg.summary_base_url.trim_end_matches('/'));

    let body = match fetch_balance(&url, &key).await {
        Ok(body) => body,
        Err(e) => {
            debug!(url = %url, error = %e, "deepseek balance unavailable");
            return;
        }
    };

    let Some(info) = body.get("balance_infos").and_then(Value::as_array).and_then(|a| a.first()) else {
        debug!(url = %url, "deepseek balance response carried no balance_infos");
        return;
    };
    let Some(balance) = number(info.get("total_balance")) else {
        debug!(url = %url, "deepseek balance was not a number");
        return;
    };

    merge(
        rows,
        QuotaRow {
            provider: "deepseek".into(),
            window: "balance".into(),
            used_percent: None,
            balance: Some(balance),
            currency: info.get("currency").and_then(Value::as_str).map(str::to_string),
            plan: None,
            resets_at_ms: None,
            sampled_at_ms: now_ms(),
            source: url,
        },
    );
}

async fn fetch_balance(url: &str, key: &str) -> Result<Value> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        // The body can echo request details; only the status is safe to surface.
        anyhow::bail!("http {status}");
    }
    Ok(response.json().await?)
}

// -- helpers ------------------------------------------------------------

/// First writer of a (provider, window) wins; later sources only fill blanks.
fn merge(rows: &mut Vec<QuotaRow>, incoming: QuotaRow) {
    let Some(existing) = rows
        .iter_mut()
        .find(|r| r.provider == incoming.provider && r.window == incoming.window)
    else {
        rows.push(incoming);
        return;
    };
    let mut filled = false;
    filled |= fill(&mut existing.used_percent, incoming.used_percent);
    filled |= fill(&mut existing.balance, incoming.balance);
    filled |= fill(&mut existing.currency, incoming.currency);
    filled |= fill(&mut existing.plan, incoming.plan);
    filled |= fill(&mut existing.resets_at_ms, incoming.resets_at_ms);
    if filled {
        existing.source = format!("{} + {}", existing.source, incoming.source);
    }
}

fn fill<T>(slot: &mut Option<T>, value: Option<T>) -> bool {
    if slot.is_none() && value.is_some() {
        *slot = value;
        return true;
    }
    false
}

fn read_json(path: &Path) -> Option<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            debug!(path = %path.display(), error = %e, "unreadable");
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(e) => {
            debug!(path = %path.display(), error = %e, "unparseable");
            None
        }
    }
}

fn rfc3339_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp_millis())
}

/// These APIs are inconsistent about quoting their numbers.
fn number(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}
