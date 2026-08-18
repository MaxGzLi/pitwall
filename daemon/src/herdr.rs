//! herdr socket client. Newline-delimited JSON over a unix socket.
//!
//! Two rules the protocol imposes: one request per connection (the server closes
//! after answering), except for streaming methods which hold the connection open.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tracing::{debug, info};

use crate::config::Config;
use crate::notice::Notice;
use crate::model::{now_ms, Harness, PaneRow, SessionUpdate, State};
use crate::store::Store;
use crate::Events;

/// Subscriptions that take no arguments. `pane.updated` is deliberately absent:
/// it fires on every terminal-title byte, which a spinner emits ~10x a second.
const GLOBAL_EVENTS: [&str; 8] = [
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
    "workspace.created",
    "workspace.closed",
    "tab.created",
    "tab.closed",
];

/// Claude's working/idle comes from a spinner glyph in the OSC title, so it
/// flickers. Only the working->idle edge is held back; everything else is written
/// at once, because `done` collapses to `idle` the moment the human looks at it.
const IDLE_DEBOUNCE: Duration = Duration::from_millis(2500);
const TICK: Duration = Duration::from_millis(250);

pub async fn run(cfg: Arc<Config>, store: Arc<Store>, events: Events) {
    let sock = cfg.herdr_socket.clone();
    let mut tracker = Tracker::default();
    let mut backoff = Duration::from_millis(500);
    // herdr is optional. On a machine that does not run it the socket is absent
    // forever, and the retry loop must not narrate that twice a minute for as
    // long as the daemon is up.
    let mut notice = Notice::new();

    loop {
        match session(&sock, &store, &events, &mut tracker).await {
            // Ok means "resubscribe now": the pane set moved, or the server hung
            // up. The short pause only stops a pane-churn storm from spinning.
            Ok(()) => {
                notice.report(("herdr", "stream"), "herdr", Ok(()));
                backoff = Duration::from_millis(500);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => {
                notice.report(("herdr", "stream"), "herdr", Err(e.to_string()));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// One subscription lifetime. Returns `Ok` when the pane set changed (so the
/// per-pane subscriptions have to be re-established) or the server hung up.
async fn session(
    sock: &Path,
    store: &Store,
    events: &Events,
    tracker: &mut Tracker,
) -> Result<()> {
    // Subscribe before snapshotting: anything that happens in the gap is then
    // replayed to us rather than lost.
    let subscribed: BTreeSet<String> = tracker.panes.keys().cloned().collect();
    let mut lines = subscribe(sock, &subscribed).await?;

    bootstrap(sock, tracker).await?;
    tracker.flush(store, events)?;
    info!(panes = tracker.panes.len(), "herdr: subscribed");
    if tracker.pane_set() != subscribed {
        return Ok(());
    }

    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line.context("reading herdr event stream")? else {
                    return Ok(());
                };
                if let Err(e) = tracker.on_event(&line) {
                    debug!(error = %e, "herdr: skipped event");
                }
            }
            _ = tick.tick() => {
                if std::mem::take(&mut tracker.structural) {
                    // Replayed events carry stale payloads, so the snapshot — not
                    // the event body — decides what the topology actually is.
                    bootstrap(sock, tracker).await?;
                    if tracker.pane_set() != subscribed {
                        tracker.flush(store, events)?;
                        return Ok(());
                    }
                }
                tracker.expire_idle();
                tracker.flush(store, events)?;
            }
        }
    }
}

// -- transport ----------------------------------------------------------

/// One request, one connection. A second request on the same socket is silently
/// dropped, so every call opens its own.
async fn rpc(sock: &Path, method: &str, params: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(sock)
        .await
        .with_context(|| format!("connecting to {}", sock.display()))?;
    let req = json!({ "id": "agent-monitor", "method": method, "params": params });
    stream.write_all(format!("{req}\n").as_bytes()).await?;
    stream.flush().await?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    reply_of(&line, method)
}

async fn subscribe(sock: &Path, panes: &BTreeSet<String>) -> Result<Lines<BufReader<UnixStream>>> {
    let mut subs: Vec<Value> = GLOBAL_EVENTS.iter().map(|t| json!({ "type": t })).collect();
    // agent_status_changed has no global form; it must name each pane.
    subs.extend(
        panes
            .iter()
            .map(|p| json!({ "type": "pane.agent_status_changed", "pane_id": p })),
    );

    let mut stream = UnixStream::connect(sock)
        .await
        .with_context(|| format!("connecting to {}", sock.display()))?;
    let req = json!({
        "id": "agent-monitor-sub",
        "method": "events.subscribe",
        "params": { "subscriptions": subs },
    });
    stream.write_all(format!("{req}\n").as_bytes()).await?;
    stream.flush().await?;

    let mut lines = BufReader::new(stream).lines();
    let first = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("events.subscribe: connection closed before the ack"))?;
    let result = reply_of(&first, "events.subscribe")?;
    if result.get("type").and_then(Value::as_str) != Some("subscription_started") {
        bail!("events.subscribe: unexpected ack {result}");
    }
    Ok(lines)
}

fn reply_of(line: &str, method: &str) -> Result<Value> {
    let line = line.trim();
    if line.is_empty() {
        bail!("{method}: herdr closed the connection without replying");
    }
    let v: Value =
        serde_json::from_str(line).with_context(|| format!("{method}: unparsable reply"))?;
    if let Some(err) = v.get("error") {
        bail!(
            "{method}: {} ({})",
            err.get("message").and_then(Value::as_str).unwrap_or("unknown error"),
            err.get("code").and_then(Value::as_str).unwrap_or("?")
        );
    }
    v.get("result")
        .cloned()
        .ok_or_else(|| anyhow!("{method}: reply carries no result"))
}

/// session.snapshot is the single authoritative bootstrap: topology, agents and
/// the pane->session join in one call.
async fn bootstrap(sock: &Path, tracker: &mut Tracker) -> Result<()> {
    let result = rpc(sock, "session.snapshot", json!({})).await?;
    let panes = result
        .get("snapshot")
        .and_then(|s| s.get("panes"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("session.snapshot: no panes array"))?;

    let mut seen = BTreeSet::new();
    for pane in panes {
        let Some(pane_id) = pane.get("pane_id").and_then(Value::as_str) else {
            continue;
        };
        seen.insert(pane_id.to_string());
        tracker.observe(pane_id, pane);
    }
    for pane_id in tracker.pane_set().difference(&seen) {
        tracker.close_pane(pane_id, "herdr_pane_gone", 0.9);
    }
    tracker.changed = true;
    Ok(())
}

// -- state --------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Pane {
    workspace_id: Option<String>,
    tab_id: Option<String>,
    agent: Option<String>,
    status: Option<String>,
    title: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    focused: bool,
    released: bool,
}

impl Pane {
    fn harness(&self) -> Option<Harness> {
        self.agent.as_deref().and_then(Harness::parse)
    }

    fn row(&self, pane_id: &str, now: i64) -> PaneRow {
        PaneRow {
            pane_id: pane_id.to_string(),
            workspace_id: self.workspace_id.clone(),
            tab_id: self.tab_id.clone(),
            agent: self.agent.clone(),
            agent_status: self.status.clone(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            harness: self.harness().map(|h| h.as_str().to_string()),
            session_id: self.session_id.clone(),
            focused: self.focused,
            released: self.released,
            seen_at_ms: now,
        }
    }
}

#[derive(Default)]
struct Tracker {
    panes: HashMap<String, Pane>,
    /// Deadline at which a held-back working->idle transition becomes real.
    pending_idle: HashMap<String, Instant>,
    dropped: Vec<String>,
    updates: Vec<SessionUpdate>,
    changed: bool,
    /// A pane/workspace/tab appeared or vanished; re-snapshot on the next tick.
    structural: bool,
}

impl Tracker {
    fn pane_set(&self) -> BTreeSet<String> {
        self.panes.keys().cloned().collect()
    }

    fn on_event(&mut self, line: &str) -> Result<()> {
        let v: Value = serde_json::from_str(line.trim())?;
        // Late RPC acks share the stream; they are not events.
        let Some(event) = v.get("event").and_then(Value::as_str) else {
            return Ok(());
        };
        let data = v.get("data").unwrap_or(&Value::Null);

        // Both envelope shapes arrive here: the snake_case EventEnvelope and the
        // dotted SubscriptionEventEnvelope, which carries a narrower payload.
        match event {
            "pane_created" | "pane.created" | "workspace_created" | "workspace.created"
            | "workspace_closed" | "workspace.closed" | "tab_created" | "tab.created"
            | "tab_closed" | "tab.closed" => {
                self.structural = true;
            }
            "pane_exited" | "pane.exited" => {
                if let Some(pane_id) = data.get("pane_id").and_then(Value::as_str) {
                    let pane_id = pane_id.to_string();
                    self.end_session(&pane_id, "herdr_pane_exited", 0.95);
                    if let Some(pane) = self.panes.get_mut(&pane_id) {
                        pane.released = true;
                        pane.status = None;
                    }
                    self.pending_idle.remove(&pane_id);
                    self.changed = true;
                }
            }
            "pane_closed" | "pane.closed" => {
                if let Some(pane_id) = data.get("pane_id").and_then(Value::as_str) {
                    self.close_pane(&pane_id.to_string(), "herdr_pane_exited", 0.95);
                }
            }
            "pane_agent_detected" | "pane.agent_detected" => {
                self.on_agent_detected(data);
            }
            "pane_agent_status_changed" | "pane.agent_status_changed" => {
                let (Some(pane_id), Some(status)) = (
                    data.get("pane_id").and_then(Value::as_str),
                    data.get("agent_status").and_then(Value::as_str),
                ) else {
                    return Ok(());
                };
                let pane_id = pane_id.to_string();
                if let Some(agent) = data.get("agent").and_then(Value::as_str) {
                    self.panes.entry(pane_id.clone()).or_default().agent = Some(agent.to_string());
                }
                self.set_status(&pane_id, status);
            }
            _ => {}
        }
        Ok(())
    }

    /// `released: true` with a null agent is herdr saying the agent process is
    /// really gone — the one end signal this transport gives us directly.
    fn on_agent_detected(&mut self, data: &Value) {
        let Some(pane_id) = data.get("pane_id").and_then(Value::as_str) else {
            return;
        };
        let pane_id = pane_id.to_string();
        let released = data.get("released").and_then(Value::as_bool).unwrap_or(false);

        if released {
            self.end_session(&pane_id, "herdr_released", 0.9);
            let final_status = data
                .get("final_status")
                .and_then(Value::as_str)
                .map(str::to_string);
            let pane = self.panes.entry(pane_id.clone()).or_default();
            pane.released = true;
            if final_status.is_some() {
                pane.status = final_status;
            }
            self.pending_idle.remove(&pane_id);
            self.changed = true;
            return;
        }

        let pane = self.panes.entry(pane_id).or_default();
        pane.released = false;
        if let Some(agent) = data.get("agent").and_then(Value::as_str) {
            pane.agent = Some(agent.to_string());
        }
        if let Some(ws) = data.get("workspace_id").and_then(Value::as_str) {
            pane.workspace_id = Some(ws.to_string());
        }
        self.changed = true;
        // The event has no agent_session, so the join has to come from a snapshot.
        self.structural = true;
    }

    /// Authoritative pane state out of session.snapshot.
    fn observe(&mut self, pane_id: &str, p: &Value) {
        let agent = p.get("agent").and_then(Value::as_str).map(str::to_string);
        let session_id = p
            .get("agent_session")
            .filter(|s| s.get("kind").and_then(Value::as_str) == Some("id"))
            .and_then(|s| s.get("value").and_then(Value::as_str))
            .map(str::to_string);
        let status = p.get("agent_status").and_then(Value::as_str).map(str::to_string);
        let hold_idle = status.as_deref() == Some("idle") && self.pending_idle.contains_key(pane_id);

        let pane = self.panes.entry(pane_id.to_string()).or_default();
        pane.workspace_id = p
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| pane.workspace_id.clone());
        pane.tab_id = p
            .get("tab_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| pane.tab_id.clone());
        // foreground_cwd routinely points at an MCP/plugin dir, so it is ignored.
        pane.cwd = p
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| pane.cwd.clone());
        pane.title = p
            .get("terminal_title_stripped")
            .or_else(|| p.get("terminal_title"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| pane.title.clone());
        pane.focused = p.get("focused").and_then(Value::as_bool).unwrap_or(false);
        // A released pane keeps the flag only until herdr gives it an agent again.
        pane.released = pane.released && agent.is_none();
        if agent.is_some() {
            pane.agent = agent;
        }
        if session_id.is_some() {
            pane.session_id = session_id;
        }
        if !hold_idle {
            pane.status = status;
        }

        self.changed = true;
        if !hold_idle {
            self.push_state(pane_id);
        }
    }

    fn set_status(&mut self, pane_id: &str, status: &str) {
        let pane = self.panes.entry(pane_id.to_string()).or_default();
        if pane.status.as_deref() == Some(status) {
            self.pending_idle.remove(pane_id);
            return;
        }
        if status == "idle" && pane.status.as_deref() == Some("working") {
            self.pending_idle
                .entry(pane_id.to_string())
                .or_insert_with(|| Instant::now() + IDLE_DEBOUNCE);
            return;
        }
        pane.status = Some(status.to_string());
        pane.released = false;
        self.pending_idle.remove(pane_id);
        self.changed = true;
        self.push_state(pane_id);
    }

    fn expire_idle(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .pending_idle
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for pane_id in due {
            self.pending_idle.remove(&pane_id);
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.status = Some("idle".to_string());
            }
            self.changed = true;
            self.push_state(&pane_id);
        }
    }

    fn push_state(&mut self, pane_id: &str) {
        let Some(pane) = self.panes.get(pane_id) else {
            return;
        };
        if pane.released {
            return;
        }
        let (Some(harness), Some(session_id)) = (pane.harness(), pane.session_id.clone()) else {
            return;
        };
        // "unknown" is never evidence of anything, least of all completion.
        let Some(state) = pane.status.as_deref().and_then(map_state) else {
            return;
        };

        let mut update = SessionUpdate::new(harness, session_id);
        update.patch.state = Some(state);
        update.patch.cwd = pane.cwd.clone();
        update.patch.title = pane.title.clone();
        if matches!(state, State::Working | State::Blocked) {
            update.patch.last_activity_ms = Some(now_ms());
        }
        self.updates.push(update);
    }

    fn end_session(&mut self, pane_id: &str, signal: &str, confidence: f64) {
        let Some(pane) = self.panes.get(pane_id) else {
            return;
        };
        let (Some(harness), Some(session_id)) = (pane.harness(), pane.session_id.clone()) else {
            return;
        };
        let mut update = SessionUpdate::new(harness, session_id);
        update.patch.state = Some(State::Ended);
        update.patch.ended_at_ms = Some(now_ms());
        update.patch.end_signal = Some(signal.to_string());
        update.patch.end_confidence = Some(confidence);
        self.updates.push(update);
    }

    fn close_pane(&mut self, pane_id: &str, signal: &str, confidence: f64) {
        self.end_session(pane_id, signal, confidence);
        self.panes.remove(pane_id);
        self.pending_idle.remove(pane_id);
        self.dropped.push(pane_id.to_string());
        self.changed = true;
    }

    fn flush(&mut self, store: &Store, events: &Events) -> Result<()> {
        if !self.changed && self.updates.is_empty() && self.dropped.is_empty() {
            return Ok(());
        }
        for pane_id in self.dropped.drain(..) {
            store.drop_pane(&pane_id)?;
        }
        if self.changed {
            let now = now_ms();
            let rows: Vec<PaneRow> =
                self.panes.iter().map(|(id, pane)| pane.row(id, now)).collect();
            store.upsert_panes(&rows)?;
        }
        if !self.updates.is_empty() {
            store.apply_updates(&self.updates)?;
            self.updates.clear();
        }
        self.changed = false;
        let _ = events.send(());
        Ok(())
    }
}

fn map_state(status: &str) -> Option<State> {
    match status {
        "working" => Some(State::Working),
        "blocked" => Some(State::Blocked),
        "idle" => Some(State::Idle),
        "done" => Some(State::Done),
        _ => None,
    }
}
