//! DSH transcripts: `~/.dsh/sessions/<slug>/<session-id>/session.jsonl.zstd`.
//!
//! The file is rewritten wholesale on every append, so mtime is an exact change
//! signal (it tracks the last record's timestamp to the millisecond) and byte
//! offsets are useless. The corpus is a few hundred KB, so `scan` just
//! decompresses and re-parses any file whose mtime moved; `poll_live` only stats.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::adapters::Adapter;
use crate::config::Config;
use crate::model::*;
use crate::redact::redact;
use crate::store::Store;

const TITLE_MAX: usize = 200;
const FIRST_MESSAGE_MAX: usize = 2000;

/// What the last full parse learned, so `poll_live` can decide staleness
/// without decompressing anything.
#[derive(Default)]
struct Tracked {
    mtime_ms: i64,
    parsed_mtime_ms: i64,
    ended: bool,
    /// Session died mid-turn or never started one; only those may be ended on
    /// silence alone. A session resting between turns is simply idle — DSH
    /// transcripts routinely sit untouched for hours and then resume.
    stale_eligible: bool,
    stale_sent: bool,
}

pub struct DshAdapter {
    cfg: Config,
    seen: HashMap<String, Tracked>,
}

impl DshAdapter {
    pub fn new(cfg: &Config) -> Self {
        Self { cfg: cfg.clone(), seen: HashMap::new() }
    }

    /// Every `<slug>/<session-id>/session.jsonl.zstd` under the sessions root.
    fn transcripts(&self) -> Vec<Transcript> {
        let mut out = Vec::new();
        let Ok(slugs) = std::fs::read_dir(self.cfg.dsh_sessions_dir()) else {
            return out;
        };
        for slug in slugs.flatten() {
            let Ok(sessions) = std::fs::read_dir(slug.path()) else {
                continue;
            };
            for session in sessions.flatten() {
                // The directory name is the session id: "session-<uuid>" for a
                // top-level session, a bare "<uuid>" for a subagent.
                let Some(id) = session.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if id.starts_with('.') {
                    continue;
                }
                let path = session.path().join("session.jsonl.zstd");
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                out.push(Transcript { id, path, mtime_ms: mtime_ms(&meta), bytes: meta.len() as i64 });
            }
        }
        out
    }
}

struct Transcript {
    id: String,
    path: PathBuf,
    mtime_ms: i64,
    bytes: i64,
}

impl Adapter for DshAdapter {
    fn name(&self) -> &'static str {
        "dsh"
    }

    fn poll_live(&mut self, store: &Store) -> Result<usize> {
        let now = now_ms();
        let mut updates = Vec::new();

        for t in self.transcripts() {
            let tracked = self.seen.entry(t.id.clone()).or_default();

            if t.mtime_ms != tracked.mtime_ms {
                tracked.mtime_ms = t.mtime_ms;
                tracked.stale_sent = false;
                tracked.ended = false;
                // Nothing is written for a session `scan` has never parsed:
                // cwd, model and start time all live inside the file.
                if tracked.parsed_mtime_ms == 0 {
                    continue;
                }
                let mut u = SessionUpdate::new(Harness::Dsh, &t.id);
                u.patch.source_bytes = Some(t.bytes);
                u.patch.source_mtime_ms = Some(t.mtime_ms);
                u.patch.last_activity_ms = Some(t.mtime_ms);
                updates.push(u);
            } else if tracked.stale_eligible
                && !tracked.ended
                && !tracked.stale_sent
                && now - t.mtime_ms > self.cfg.stale_after_ms
            {
                tracked.stale_sent = true;
                tracked.ended = true;
                let mut u = SessionUpdate::new(Harness::Dsh, &t.id);
                u.patch.state = Some(State::Ended);
                u.patch.ended_at_ms = Some(t.mtime_ms);
                u.patch.end_signal = Some("mtime_stale".into());
                u.patch.end_confidence = Some(0.5);
                updates.push(u);
            }
        }

        store.apply_updates(&updates)?;
        Ok(updates.len())
    }

    fn scan(&mut self, store: &Store) -> Result<usize> {
        let now = now_ms();
        let mut updates = Vec::new();

        for t in self.transcripts() {
            let tracked = self.seen.entry(t.id.clone()).or_default();
            tracked.mtime_ms = t.mtime_ms;
            if tracked.parsed_mtime_ms == t.mtime_ms {
                continue;
            }

            let parsed = match parse(&t.path, &t.id) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(session = %t.id, error = %e, "dsh transcript unreadable");
                    continue;
                }
            };
            tracked.parsed_mtime_ms = t.mtime_ms;
            tracked.stale_eligible = parsed.turns == 0 || parsed.open_turn;

            let stale = now - t.mtime_ms > self.cfg.stale_after_ms;
            let (state, signal, confidence) = parsed.verdict(tracked.stale_eligible && stale);
            tracked.ended = state == State::Ended;
            tracked.stale_sent = signal == Some("mtime_stale");

            let mut u = SessionUpdate::new(Harness::Dsh, &t.id);
            u.patch.source_path = Some(t.path.to_string_lossy().into_owned());
            u.patch.source_bytes = Some(t.bytes);
            u.patch.source_mtime_ms = Some(t.mtime_ms);
            u.patch.kind = Some(if parsed.subagent { "subagent" } else { "interactive" }.into());
            u.patch.parent_id = parsed.parent.clone();
            u.patch.depth = Some(parsed.depth);
            u.patch.cwd = parsed.cwd.clone();
            u.patch.provider = parsed.provider.clone();
            u.patch.model = parsed.model.clone();
            u.patch.effort = parsed.effort.clone();
            u.patch.context_window = parsed.context_window;
            u.patch.title = parsed.title.clone();
            u.patch.first_user_message = parsed.first_user.clone();
            u.patch.started_at_ms = Some(parsed.created_at_ms);
            u.patch.last_activity_ms = Some(parsed.last_time_ms.max(t.mtime_ms));
            u.patch.turns = Some(parsed.turns);
            u.patch.tool_calls = Some(parsed.tool_calls);
            u.patch.state = Some(state);
            u.patch.end_signal = signal.map(str::to_string);
            u.patch.end_confidence = Some(confidence);
            if state == State::Ended {
                u.patch.ended_at_ms = Some(parsed.last_time_ms.max(t.mtime_ms));
            }
            u.usage = parsed.usage;
            updates.push(u);
        }

        store.apply_updates(&updates)?;
        Ok(updates.len())
    }
}

#[derive(Default)]
struct Parsed {
    cwd: Option<String>,
    created_at_ms: i64,
    parent: Option<String>,
    depth: i64,
    subagent: bool,
    provider: Option<String>,
    model: Option<String>,
    context_window: Option<i64>,
    effort: Option<String>,
    title: Option<String>,
    first_user: Option<String>,
    last_time_ms: i64,
    last_type: String,
    last_turn_completed: bool,
    turns: i64,
    tool_calls: i64,
    open_turn: bool,
    usage: Vec<UsageDelta>,
}

impl Parsed {
    /// End detection. A top-level session is closed by a trailing `session/end-seed`;
    /// mid-file ones mark an end that was later resumed, so only the last record counts.
    /// Subagents never emit it — they stop at a completed `turn/end` with nothing after.
    fn verdict(&self, stale: bool) -> (State, Option<&'static str>, f64) {
        if !self.subagent && self.last_type == "session/end-seed" {
            (State::Ended, Some("end_seed"), 0.85)
        } else if self.subagent && self.last_type == "turn/end" && self.last_turn_completed {
            (State::Ended, Some("turn_end"), 0.8)
        } else if stale {
            (State::Ended, Some("mtime_stale"), 0.5)
        } else if self.open_turn {
            (State::Working, None, 0.0)
        } else {
            (State::Idle, None, 0.0)
        }
    }
}

fn parse(path: &Path, session_id: &str) -> Result<Parsed> {
    let raw = zstd::stream::decode_all(std::fs::File::open(path)?)?;
    let text = String::from_utf8_lossy(&raw);
    let mut p = Parsed::default();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A torn final line is possible while DSH is rewriting the file.
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ty = v["type"].as_str().unwrap_or_default();
        if ty.is_empty() {
            continue;
        }
        p.last_type = ty.to_string();
        // Batched chunk records carry `time0`/`seq0` instead of `time`/`seq`.
        if let Some(t) = v["time"].as_i64().or_else(|| v["time0"].as_i64()) {
            p.last_time_ms = p.last_time_ms.max(t);
        }
        let d = &v["data"];

        match ty {
            // The header record, and the only one with no `seq`.
            "session" => {
                p.created_at_ms = v["createdAt"].as_i64().unwrap_or_default();
                p.cwd = v["cwd"].as_str().map(str::to_string);
                p.parent = v["parentSession"].as_str().map(str::to_string);
                p.depth = v["delegationDepth"].as_i64().unwrap_or(0);
                p.subagent = v["origin"].as_str() == Some("subagent");
                p.last_time_ms = p.last_time_ms.max(p.created_at_ms);
            }
            "request/context" => {
                p.provider = d["provider"].as_str().map(str::to_string);
                p.model = d["model"].as_str().map(str::to_string);
                p.context_window = d["contextWindow"].as_i64();
            }
            "request/header" => {
                p.effort = d["header"]["config"]["reasoningEffort"].as_str().map(str::to_string);
            }
            // Emitted twice: a truncated-prompt fallback, then the LLM's title.
            "session/title" => {
                if let Some(t) = d["title"].as_str() {
                    p.title = Some(clip(&redact(t), TITLE_MAX));
                }
            }
            "user/message" => {
                if p.first_user.is_none() {
                    let text = join_text(&d["content"]);
                    if !text.is_empty() {
                        p.first_user = Some(clip(&redact(&text), FIRST_MESSAGE_MAX));
                    }
                }
            }
            "turn/start" => {
                p.turns = p.turns.max(d["turn"].as_i64().unwrap_or(p.turns + 1));
                p.open_turn = true;
            }
            "turn/end" => {
                p.open_turn = false;
                p.last_turn_completed = d["reason"]["kind"].as_str() == Some("completed");
            }
            "tool/call" => p.tool_calls += 1,
            // One materialised record per assistant step, each carrying its own
            // non-cumulative usage. The streaming chunk types duplicate its
            // content and are ignored entirely.
            "assistant/message" => {
                let u = &d["usage"];
                let Some(seq) = v["seq"].as_i64() else {
                    continue;
                };
                if !u.is_object() {
                    continue;
                }
                p.usage.push(UsageDelta {
                    dedup_key: format!("{session_id}#{seq}"),
                    at_ms: p.last_time_ms,
                    model: p.model.clone(),
                    input: u["inputTokens"].as_i64().unwrap_or(0),
                    // outputTokens already includes reasoningTokens here, so
                    // reasoning is reported but never added on top.
                    output: u["outputTokens"].as_i64().unwrap_or(0),
                    cache_read: u["cacheReadTokens"].as_i64().unwrap_or(0),
                    cache_create: 0,
                    reasoning: u["reasoningTokens"].as_i64().unwrap_or(0),
                });
            }
            _ => {}
        }
    }

    if p.created_at_ms == 0 {
        p.created_at_ms = p.last_time_ms;
    }
    Ok(p)
}

/// Concatenates the `text` parts of a content array, skipping images and tool payloads.
fn join_text(content: &Value) -> String {
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Char-safe truncation; these transcripts are mostly Chinese.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real adapter over the real `~/.dsh/sessions` tree and prints what
    /// landed in the store. Skips when this machine has no DSH sessions.
    #[test]
    fn scans_local_sessions() {
        let cfg = Config::load().unwrap();
        if !cfg.dsh_sessions_dir().is_dir() {
            eprintln!("skip: no {}", cfg.dsh_sessions_dir().display());
            return;
        }
        let db = std::env::temp_dir().join(format!("dsh-adapter-test-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();
        let mut a = DshAdapter::new(&cfg);

        let n = a.scan(&store).unwrap();
        let again = a.scan(&store).unwrap();
        let live = a.poll_live(&store).unwrap();
        println!("scan={n} rescan={again} poll_live={live}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT session_id, kind, depth, parent_id, project, provider, model, effort,
                        context_window, state, end_signal, end_confidence, turns, tool_calls,
                        tok_input, tok_output, tok_cache_read, tok_reasoning, title
                 FROM session ORDER BY started_at_ms",
            )
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                Ok(format!(
                    "{}\n  kind={} depth={} parent={:?}\n  project={:?} {}/{} effort={:?} ctx={:?}\n  \
                     state={} signal={:?} conf={} turns={} tools={}\n  tok in={} out={} cacheR={} reasoning={}\n  title={:?}",
                    r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(7)?, r.get::<_, Option<i64>>(8)?,
                    r.get::<_, String>(9)?, r.get::<_, Option<String>>(10)?, r.get::<_, f64>(11)?,
                    r.get::<_, i64>(12)?, r.get::<_, i64>(13)?, r.get::<_, i64>(14)?,
                    r.get::<_, i64>(15)?, r.get::<_, i64>(16)?, r.get::<_, i64>(17)?,
                    r.get::<_, Option<String>>(18)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for row in &rows {
            println!("{row}");
        }
        let deltas: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_delta", [], |r| r.get(0))
            .unwrap();
        let totals: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT SUM(d_input), SUM(d_output), SUM(d_cache_read), SUM(d_reasoning) FROM usage_delta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        println!("usage_delta rows={deltas} totals in/out/cacheR/reasoning={totals:?}");
        let _ = std::fs::remove_file(&db);
        assert!(!rows.is_empty());
    }
}
