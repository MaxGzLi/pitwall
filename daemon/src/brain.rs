//! Handing finished work to the DSH brain.
//!
//! The brain is a local-first second brain that lives inside DeepSeek Harness.
//! It already watches Claude Code through a Stop hook, but a hook only fires
//! where it is installed: sessions run under herdr, under Codex, or on a machine
//! where the hook never got wired up leave no trace in it. This daemon has
//! already read every one of those transcripts and had a summary written for
//! them, so it is in a position to tell the brain what the hooks missed.
//!
//! What is sent is exactly what the panel shows -- a headline and four lines --
//! plus the measured facts around it. No transcript text goes out beyond what
//! the summariser already condensed, and it goes to loopback only.
//!
//! Everything lands in the inbox (`candidate: true`), never straight into the
//! vault. These summaries are a model's reading of a transcript; the brain's own
//! design keeps saving separate from remembering, and a machine-written claim is
//! exactly the kind that should have to earn its place.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{debug, info};

use crate::config::{self, Config};
use crate::model::CaptureCandidate;
use crate::store::Store;

/// How many captures one tick may send. The API is loopback and fast, but a
/// first run on an established machine has hundreds of old sessions to offer
/// and there is no reason to do it in one burst.
const PER_TICK: i64 = 5;

/// Sends what has not been sent. Returns how many the vault accepted.
pub async fn run_once(cfg: &Config, store: &Store) -> Result<usize> {
    if !cfg.brain_capture {
        return Ok(0);
    }
    // No token means no brain on this machine. That is the ordinary case, not a
    // failure, and it must stay silent.
    let Some(token) = config::brain_token(cfg) else {
        return Ok(0);
    };

    let pending = store.uncaptured_summaries(PER_TICK)?;
    if pending.is_empty() {
        return Ok(0);
    }

    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let mut stored = 0;
    for c in &pending {
        let key = session_key(&c.harness, &c.session_id);
        match capture(&client, cfg, &token, &envelope(&key, c)).await? {
            // A network failure is not an answer: leave the row unmarked so the
            // next tick tries again.
            Outcome::Unreachable(e) => {
                debug!(error = %e, "brain unreachable");
                break;
            }
            Outcome::Stored(id) => {
                store.mark_captured(&key, Some(&id), "stored")?;
                stored += 1;
                info!(memory = %id, project = c.project.as_deref().unwrap_or("-"), "filed with the brain");
            }
            // The vault already holds this key. Nothing to do but remember that.
            Outcome::Duplicate(id) => store.mark_captured(&key, id.as_deref(), "duplicate")?,
            // Malformed for this vault's rules. Retrying produces the same
            // refusal every tick, so record it and move on -- once, loudly.
            Outcome::Rejected(why) => {
                store.mark_captured(&key, None, "rejected")?;
                tracing::warn!(session = %c.session_id, reason = %why, "the brain refused a capture");
            }
        }
    }
    Ok(stored)
}

fn session_key(harness: &str, session_id: &str) -> String {
    format!("pitwall/session/{harness}/{session_id}")
}

enum Outcome {
    Stored(String),
    Duplicate(Option<String>),
    Rejected(String),
    Unreachable(String),
}

async fn capture(
    client: &reqwest::Client,
    cfg: &Config,
    token: &str,
    body: &Value,
) -> Result<Outcome> {
    let url = format!("{}/v1/captures", cfg.brain_url.trim_end_matches('/'));
    let sent = client.post(&url).bearer_auth(token).json(body).send().await;
    let resp = match sent {
        Ok(r) => r,
        Err(e) => return Ok(Outcome::Unreachable(e.to_string())),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    // 409 is the vault saying it already has this key under different content --
    // a summary that was rewritten, most often. The memory is there either way.
    if status.as_u16() == 409 {
        return Ok(Outcome::Duplicate(None));
    }
    if status.is_success() {
        let id = parsed["id"].as_str().unwrap_or_default().to_string();
        return Ok(if parsed["deduplicated"].as_bool().unwrap_or(false) {
            Outcome::Duplicate(Some(id))
        } else {
            Outcome::Stored(id)
        });
    }
    if status.is_client_error() {
        let why = parsed["message"].as_str().unwrap_or(&text).to_string();
        return Ok(Outcome::Rejected(crate::redact::redact(&why)));
    }
    // 5xx is the server having a bad moment, which is worth another try.
    Ok(Outcome::Unreachable(format!("HTTP {status}")))
}

/// A capture envelope v1, as the brain's schema defines it.
fn envelope(key: &str, c: &CaptureCandidate) -> Value {
    let occurred = c.ended_at_ms.unwrap_or(c.created_at_ms);
    let mut tags = vec!["pitwall".to_string(), c.harness.clone()];
    if let Some(p) = &c.project {
        tags.push(p.clone());
    }

    let mut env = json!({
        "schemaVersion": "dsh-brain.capture/v1",
        "idempotencyKey": key,
        "title": c.headline,
        "content": content(c),
        "kind": "outcome",
        "tags": tags,
        "scope": "personal",
        "privacy": "private",
        // The four lines are a model's reading of a transcript, not something
        // the user said and not something anyone verified.
        "authority": "agent_inferred",
        "occurredAt": crate::model::iso(occurred),
        "candidate": true,
        "producer": { "name": "pitwall", "version": env!("CARGO_PKG_VERSION") },
        "sourceRefs": [{ "type": "agent_session", "title": source_title(c) }],
    });

    let mut context = json!({ "sessionId": c.session_id, "capturedFrom": "pitwall" });
    if let Some(p) = &c.project {
        env["topic"] = json!(p);
        context["project"] = json!(p);
    }
    if let Some(cwd) = &c.cwd {
        context["workspace"] = json!(cwd);
    }
    env["context"] = context;
    env
}

/// The summary, then one line of what it cost. The facts line is the part the
/// brain has no other way of knowing: hooks see turns, not spend.
fn content(c: &CaptureCandidate) -> String {
    let mut out = String::new();
    if let Some(body) = c.body.as_deref() {
        out.push_str(body.trim());
        out.push_str("\n\n");
    }
    out.push_str(&format!("— {}", source_title(c)));
    if let Some(end) = c.ended_at_ms {
        if let Some(span) = span(end - c.started_at_ms) {
            out.push_str(&format!(" · {span}"));
        }
    }
    if c.turns > 0 {
        out.push_str(&format!(" · {} 轮", c.turns));
    }
    if c.tok_total > 0 {
        out.push_str(&format!(" · {:.1}M tokens", c.tok_total as f64 / 1e6));
    }
    if c.cost_usd > 0.0 {
        out.push_str(&format!(" · ${:.2}", c.cost_usd));
    }
    out
}

/// First activity to last, in the same compact notation the panel uses. Not
/// time spent -- a session left open overnight spans a day either way -- but the
/// reader already knows how to read `1d09` from the panel, and a bare "2010
/// 分钟" reads like a claim about effort that nobody measured.
fn span(ms: i64) -> Option<String> {
    let minutes = ms.max(0) / 60_000;
    if minutes < 1 {
        return None;
    }
    if minutes < 60 {
        return Some(format!("{minutes}m"));
    }
    let hours = minutes / 60;
    if hours < 24 {
        return Some(format!("{hours}h{:02}", minutes % 60));
    }
    Some(format!("{}d{:02}", hours / 24, hours % 24))
}

/// Harness and project, and deliberately not the session's own title: for Codex
/// that title is the entire first prompt, paragraphs of it, and the summary's
/// headline already says what the session was about far better.
fn source_title(c: &CaptureCandidate) -> String {
    match &c.project {
        Some(p) => format!("{} · {p}", c.harness),
        None => c.harness.clone(),
    }
}

/// Reports what the brain channel is doing, once, at startup. A feature that
/// silently does nothing because a token is missing is indistinguishable from
/// one that is broken.
pub fn describe(cfg: &Config) -> String {
    if !cfg.brain_capture {
        return "off (brain_capture = false)".into();
    }
    match config::brain_token(cfg) {
        Some(_) => format!("on, filing to {}", cfg.brain_url),
        None => format!(
            "idle: no capture token in {} (nothing is sent)",
            cfg.brain_token_file.display()
        ),
    }
}

/// Checks the channel is actually reachable, for the same reason.
pub async fn health(cfg: &Config) -> Result<()> {
    let Some(token) = config::brain_token(cfg) else {
        return Ok(());
    };
    let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build()?;
    let url = format!("{}/v1/health", cfg.brain_url.trim_end_matches('/'));
    let resp = client.get(&url).bearer_auth(token).send().await.context("brain health")?;
    anyhow::ensure!(resp.status().is_success(), "brain health returned {}", resp.status());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> CaptureCandidate {
        CaptureCandidate {
            harness: "claude".into(),
            session_id: "abc-123".into(),
            headline: "修好配置文件".into(),
            body: Some("做了什么：加了 config.toml\n结果：成了\n待办：无".into()),
            created_at_ms: 1_755_000_000_000,
            project: Some("agent-monitor".into()),
            cwd: Some("/w/agent-monitor".into()),
            started_at_ms: 1_755_000_000_000 - 3_600_000,
            ended_at_ms: Some(1_755_000_000_000),
            turns: 12,
            tok_total: 1_500_000,
            cost_usd: 2.5,
        }
    }

    /// The envelope has to satisfy the brain's schema on the first try: a
    /// rejected capture is recorded as rejected and never retried, so a shape
    /// error here would silently drop every session.
    #[test]
    fn the_envelope_matches_the_capture_schema() {
        let c = candidate();
        let key = session_key(&c.harness, &c.session_id);
        let e = envelope(&key, &c);

        assert_eq!(e["schemaVersion"], "dsh-brain.capture/v1");
        assert_eq!(e["idempotencyKey"], "pitwall/session/claude/abc-123");
        assert_eq!(e["kind"], "outcome");
        // Enumerated by the schema; a typo in any of these is a 400.
        assert_eq!(e["privacy"], "private");
        assert_eq!(e["authority"], "agent_inferred");
        assert_eq!(e["candidate"], true);
        assert_eq!(e["producer"]["name"], "pitwall");
        assert_eq!(e["context"]["sessionId"], "abc-123");
        assert_eq!(e["context"]["workspace"], "/w/agent-monitor");
        assert_eq!(e["topic"], "agent-monitor");
        assert!(e["occurredAt"].as_str().unwrap().ends_with('Z'), "occurredAt must be RFC 3339");

        // The facts line is the part hooks cannot know.
        let content = e["content"].as_str().unwrap();
        assert!(content.contains("做了什么"), "the summary itself survives");
        assert!(content.contains("1h00") && content.contains("12 轮"), "{content}");
        assert_eq!(
            content.lines().last().unwrap(),
            "— claude · agent-monitor · 1h00 · 12 轮 · 1.5M tokens · $2.50",
            "the facts line is one short line, never the session's own first prompt"
        );
        assert!(content.contains("1.5M tokens") && content.contains("$2.50"), "{content}");
    }

    /// The panel's notation, so the same session reads the same in both places.
    #[test]
    fn a_span_reads_like_the_panel() {
        assert_eq!(span(0), None, "under a minute is not worth a field");
        assert_eq!(span(59_000), None);
        assert_eq!(span(29 * 60_000).as_deref(), Some("29m"));
        assert_eq!(span(344 * 60_000).as_deref(), Some("5h44"));
        assert_eq!(span(2010 * 60_000).as_deref(), Some("1d09"));
        assert_eq!(span(-5).as_deref(), None, "a clock that went backwards is not a negative span");
    }

    /// A session with nothing measured around it still has to produce a legal
    /// envelope -- `title` alone satisfies the schema's anyOf.
    #[test]
    fn a_bare_session_still_makes_a_legal_envelope() {
        let bare = CaptureCandidate {
            body: None,
            project: None,
            cwd: None,
            ended_at_ms: None,
            turns: 0,
            tok_total: 0,
            cost_usd: 0.0,
            ..candidate()
        };
        let e = envelope("k", &bare);
        assert!(e.get("topic").is_none(), "no project means no topic, not an empty one");
        assert_eq!(e["context"].get("workspace"), None);
        assert_eq!(e["content"], "— claude");
        assert_eq!(e["tags"], json!(["pitwall", "claude"]));
    }
}
