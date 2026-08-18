//! USD per 1M tokens, from models.dev.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{info, warn};

use crate::config::Config;
use crate::store::{Price, Store};

/// models.dev lists the same model under dozens of resellers, each with its own
/// markup (and a few with a flat 0). Prefer the vendor the tokens are billed by.
const FIRST_PARTY: &[&str] = &["anthropic", "openai", "deepseek", "zhipuai"];

/// The models this machine actually runs. A missing one prices its tokens at 0,
/// which looks exactly like a free model, so say which ones are missing.
const WATCHED: &[&str] = &[
    "claude-opus-5",
    "claude-opus-5[1m]",
    "opus[1m]",
    "claude-fable-5",
    "claude-sonnet-5",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "codex-auto-review",
    "deepseek-v4-flash",
    "glm-4.6v",
];

pub fn load_seed(cfg: &Config, store: &Store) -> Result<usize> {
    let path = cfg.pricing_seed();
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    ingest(&text, store)
}

pub async fn refresh(cfg: &Config, store: &Store) -> Result<usize> {
    match fetch(cfg).await {
        Ok(body) => match ingest(&body, store) {
            Ok(n) => {
                if let Err(e) = std::fs::write(cfg.pricing_cache(), &body) {
                    warn!(error = %e, "could not cache the models.dev response");
                }
                return Ok(n);
            }
            Err(e) => warn!(error = %e, "models.dev returned something unparseable"),
        },
        Err(e) => warn!(error = %e, "models.dev unreachable"),
    }
    // A stale price beats no price, and a price refresh must never take the daemon
    // down: fall back to our own cached copy, then to the seed, then give up quietly.
    for path in [cfg.pricing_cache(), cfg.pricing_seed()] {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match ingest(&text, store) {
            Ok(n) => {
                info!(models = n, source = %path.display(), "priced from a local copy");
                return Ok(n);
            }
            Err(e) => warn!(error = %e, path = %path.display(), "unparseable local price copy"),
        }
    }
    Ok(0)
}

async fn fetch(cfg: &Config) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    Ok(client
        .get(cfg.pricing_url())
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

fn ingest(text: &str, store: &Store) -> Result<usize> {
    let table = parse(text)?;
    report_gaps(&table);
    let rows: Vec<(String, Price)> = table.into_iter().map(|(id, e)| (id, e.price)).collect();
    store.put_prices(&rows)
}

struct Entry {
    price: Price,
    score: i32,
}

/// `{provider: {models: {id: {cost: {input, output, cache_read, cache_write}}}}}`,
/// flattened to one row per model id. Navigated as `Value` rather than typed
/// structs so one odd model in a 6000-model feed cannot lose the other 5999.
fn parse(text: &str) -> Result<HashMap<String, Entry>> {
    let root: Value = serde_json::from_str(text).context("parsing models.dev json")?;
    let providers = root
        .as_object()
        .context("models.dev json is not an object of providers")?;

    let mut table: HashMap<String, Entry> = HashMap::new();
    // Anthropic family -> (release date, price of the newest model in it).
    let mut families: HashMap<String, (String, Price)> = HashMap::new();

    for (provider_id, provider) in providers {
        let first_party = FIRST_PARTY.contains(&provider_id.as_str());
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_id, model) in models {
            let Some(cost) = model.get("cost") else {
                continue;
            };
            let price = Price {
                input: usd(cost, "input"),
                output: usd(cost, "output"),
                cache_read: usd(cost, "cache_read"),
                cache_write: usd(cost, "cache_write"),
            };
            let mut score = if first_party { 100 } else { 0 };
            if price.input > 0.0 || price.output > 0.0 {
                score += 10;
            }
            if price.cache_read > 0.0 {
                score += 2;
            }
            if price.cache_write > 0.0 {
                score += 1;
            }

            offer(&mut table, model_id, price, score);
            // Aggregators qualify ids ("openai/gpt-5.6-sol"); harnesses log the bare one.
            if let Some((_, bare)) = model_id.rsplit_once('/') {
                offer(&mut table, bare, price, score);
            }

            if provider_id == "anthropic" {
                if let (Some(family), Some(date)) = (
                    model.get("family").and_then(Value::as_str),
                    model.get("release_date").and_then(Value::as_str),
                ) {
                    if let Some(alias) = family.strip_prefix("claude-") {
                        let newest = families
                            .entry(alias.to_string())
                            .or_insert_with(|| (String::new(), price));
                        if date >= newest.0.as_str() {
                            *newest = (date.to_string(), price);
                        }
                    }
                }
            }
        }
    }

    // Claude Code logs the bare family name when the model was picked by alias
    // (`/model opus`). Score -1 so a real model of that name always wins.
    for (alias, (_, price)) in families {
        offer(&mut table, &alias, price, -1);
    }

    Ok(table)
}

fn offer(table: &mut HashMap<String, Entry>, model: &str, price: Price, score: i32) {
    let slot = table
        .entry(model.to_string())
        .or_insert(Entry { price, score });
    if score > slot.score {
        *slot = Entry { price, score };
    }
}

fn usd(cost: &Value, key: &str) -> f64 {
    cost.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

static REPORTED: AtomicBool = AtomicBool::new(false);

fn report_gaps(table: &HashMap<String, Entry>) {
    if REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let missing: Vec<&str> = WATCHED
        .iter()
        .copied()
        .filter(|m| !priced(table, m))
        .collect();
    if missing.is_empty() {
        info!(models = table.len(), "every model this machine runs has a price");
    } else {
        info!(models = table.len(), unpriced = ?missing, "no price for these models; their tokens cost 0");
    }
}

/// Same decoration-stripping as `store::price_for`, so this reports what the
/// daemon will actually resolve rather than what the table literally contains.
fn priced(table: &HashMap<String, Entry>, model: &str) -> bool {
    let bare = model.rsplit_once('/').map_or(model, |(_, tail)| tail);
    let undecorated = bare.split('[').next().unwrap_or(bare);
    [model, bare, undecorated]
        .iter()
        .any(|k| table.get(*k).is_some_and(|e| e.price.input > 0.0 || e.price.output > 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Mirrors `store::price_for`, which is private to the store.
    fn lookup(conn: &Connection, model: &str) -> Price {
        let bare = model.rsplit_once('/').map_or(model, |(_, tail)| tail);
        for candidate in [model, bare, bare.split('[').next().unwrap_or(bare)] {
            let found = conn
                .query_row(
                    "SELECT input, output, cache_read, cache_write FROM price WHERE model = ?1",
                    [candidate],
                    |r| {
                        Ok(Price {
                            input: r.get(0)?,
                            output: r.get(1)?,
                            cache_read: r.get(2)?,
                            cache_write: r.get(3)?,
                        })
                    },
                )
                .ok();
            if let Some(price) = found {
                return price;
            }
        }
        Price::default()
    }

    #[test]
    fn seed_prices_every_model_this_machine_runs() {
        let cfg = Config::load().unwrap();
        if !cfg.pricing_seed().exists() {
            eprintln!("no seed at {}, skipping", cfg.pricing_seed().display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("agent-monitor-pricing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("prices.sqlite");
        let store = Store::open(&db).unwrap();

        let n = load_seed(&cfg, &store).unwrap();
        assert!(n > 1000, "only {n} models in the seed");

        let conn = Connection::open(&db).unwrap();
        let mut unpriced = Vec::new();
        for model in WATCHED {
            let p = lookup(&conn, model);
            println!(
                "{model:<20} in={:<8} out={:<8} cache_read={:<8} cache_write={}",
                p.input, p.output, p.cache_read, p.cache_write
            );
            if p.input == 0.0 && p.output == 0.0 {
                unpriced.push(*model);
            }
        }
        // codex-auto-review is a Codex-internal model; models.dev has never listed it.
        assert_eq!(unpriced, vec!["codex-auto-review"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_party_price_beats_a_reseller() {
        let json = r#"{
          "kenari":    {"models": {"claude-sonnet-5": {"id": "claude-sonnet-5", "cost": {"input": 0, "output": 0}}}},
          "anthropic": {"models": {"claude-sonnet-5": {"id": "claude-sonnet-5", "family": "claude-sonnet",
                                    "release_date": "2026-06-29",
                                    "cost": {"input": 2, "output": 10, "cache_read": 0.2, "cache_write": 2.5}}}},
          "kilo":      {"models": {"anthropic/claude-sonnet-5": {"id": "anthropic/claude-sonnet-5",
                                    "cost": {"input": 9, "output": 9}}}}
        }"#;
        let table = parse(json).unwrap();
        let p = &table["claude-sonnet-5"].price;
        assert_eq!((p.input, p.output, p.cache_read, p.cache_write), (2.0, 10.0, 0.2, 2.5));
        // The qualified id is registered too, and the alias follows the newest model.
        assert_eq!(table["anthropic/claude-sonnet-5"].price.input, 9.0);
        assert_eq!(table["sonnet"].price.input, 2.0);
    }
}
