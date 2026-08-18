//! Codex. There are 13 GB of rollouts on this machine and 71k index rows, so nothing
//! here ever walks the transcript tree: `state_5.sqlite` says which threads moved, and
//! only those rollouts get tailed from the byte offset we stopped at last time.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapters::Adapter;
use crate::config::Config;
use crate::model::*;
use crate::redact::redact;
use crate::store::Store;

/// Cursor over `threads.updated_at_ms`. Only threads past it get their rollout opened.
const WATERMARK: &str = "codex.updated_at_ms";
/// Threads quiet for longer than this are neither polled nor backfilled on a cold start.
const WINDOW_MS: i64 = 24 * 3600 * 1000;
/// Rollouts tailed per scan, oldest first, so one busy hour cannot starve the rest.
const MAX_SCAN: i64 = 64;
const TITLE_MAX: usize = 120;
const MESSAGE_MAX: usize = 400;

pub struct CodexAdapter {
    cfg: Config,
    index: Option<Connection>,
    /// id -> (updated_at_ms, ended) as last published, so a 2 s poll only writes on change.
    seen: HashMap<String, (i64, bool)>,
}

impl CodexAdapter {
    pub fn new(cfg: &Config) -> Self {
        Self { cfg: cfg.clone(), index: None, seen: HashMap::new() }
    }

    /// Read-only handle on somebody else's WAL database; dropped on any error so the
    /// next tick reopens rather than getting stuck on a dead connection.
    fn threads(&mut self, since_ms: i64, limit: i64) -> Result<Vec<Thread>> {
        if self.index.is_none() {
            let path = self.cfg.codex_state_db();
            let conn = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .with_context(|| format!("opening {}", path.display()))?;
            conn.busy_timeout(Duration::from_millis(3_000))?;
            self.index = Some(conn);
        }
        let result = query_threads(self.index.as_ref().unwrap(), since_ms, limit);
        if result.is_err() {
            self.index = None;
        }
        result
    }

    fn scan_thread(&mut self, store: &Store, t: &Thread) -> Result<Option<SessionUpdate>> {
        let path = Path::new(&t.rollout_path);
        let meta = std::fs::metadata(path)?;
        let len = meta.len() as i64;
        let mtime = mtime_ms(&meta);

        let mut offset = store.scan_offset(Harness::Codex, &t.id)?;
        let mut carry = load_carry(store, &t.id)?;
        if offset > len {
            // truncated or replaced; re-reading is safe because every delta is keyed
            // by its own cumulative total and re-inserts are ignored.
            offset = 0;
            carry = Carry::default();
        }
        if offset == len {
            return Ok(None);
        }

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut line: Vec<u8> = Vec::with_capacity(8 * 1024);
        let mut consumed = offset;
        let mut idx: i64 = 0;
        let mut events: Vec<(i64, i64, Ev)> = Vec::new();

        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                break; // half-written record: leave it for the next scan
            }
            consumed += n as i64;
            idx += 1;
            if !interesting(&line) {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let at_ms = ts_ms(&value).unwrap_or(t.updated_at_ms);
            if let Some(ev) = classify(&value, &t.id) {
                events.push((idx, at_ms, ev));
            }
        }

        // A forked or Desktop-spawned thread opens its rollout with a replay of the
        // parent's history, carrying the parent's session_meta and its cumulative token
        // counter. Everything up to the last foreign session_meta is that replay.
        let boundary = events
            .iter()
            .filter(|(_, _, ev)| matches!(ev, Ev::Foreign))
            .map(|(i, _, _)| *i)
            .last()
            .unwrap_or(-1);

        // the spawn variants that carry no thread_spawn object name their parent here
        let meta_parent = events.iter().find_map(|(_, _, ev)| match ev {
            Ev::Meta { parent } => parent.clone(),
            _ => None,
        });

        let mut prev = carry.cum;
        if prev.is_none() {
            // the inherited counter is the baseline, never billable usage
            for (i, _, ev) in &events {
                if *i > boundary {
                    break;
                }
                if let Ev::Usage { cum, .. } = ev {
                    prev = Some(*cum);
                }
            }
        }

        let mut usage = Vec::new();
        let mut model = None;
        let mut effort = None;
        let mut window = None;
        let mut last_at = 0i64;
        let mut turn_seen = false;

        for (i, at_ms, ev) in &events {
            if *i <= boundary {
                continue;
            }
            last_at = last_at.max(*at_ms);
            match ev {
                Ev::Foreign | Ev::Meta { .. } => {}
                Ev::Context { model: m, effort: e } => {
                    if m.is_some() {
                        model = m.clone();
                    }
                    if e.is_some() {
                        effort = e.clone();
                    }
                }
                Ev::TurnStart { window: w } => {
                    carry.open_turn = true;
                    turn_seen = true;
                    window = w.or(window);
                }
                Ev::TurnEnd => {
                    carry.open_turn = false;
                    carry.turns += 1;
                    turn_seen = true;
                }
                Ev::Tool => carry.tools += 1,
                Ev::Compact => carry.compactions += 1,
                Ev::Usage { cum, last, window: w } => {
                    window = w.or(window);
                    let d = match prev {
                        Some(p) => cum.since(&p),
                        // no predecessor at all: this record's own turn is the delta
                        None => last.clamp_to(cum),
                    };
                    prev = Some(*cum);
                    if d.total <= 0 {
                        continue;
                    }
                    usage.push(UsageDelta {
                        dedup_key: format!("{}#{}", t.id, cum.total),
                        at_ms: *at_ms,
                        model: model.clone().or_else(|| t.model.clone()),
                        input: (d.input - d.cached - d.cache_write).max(0),
                        output: d.output,
                        cache_read: d.cached,
                        cache_create: d.cache_write,
                        reasoning: d.reasoning,
                    });
                }
            }
        }

        carry.cum = prev;
        save_carry(store, &t.id, &carry)?;

        let mut update = SessionUpdate::new(Harness::Codex, t.id.clone());
        let mut patch = base_patch(t);
        if patch.parent_id.is_none() && t.kind == "subagent" {
            patch.parent_id = meta_parent;
        }
        patch.source_bytes = Some(consumed);
        patch.source_mtime_ms = mtime;
        patch.scan_offset = Some(consumed);
        patch.context_window = window;
        patch.model = model.or(patch.model);
        patch.effort = effort.or(patch.effort);
        patch.turns = Some(carry.turns + i64::from(carry.open_turn));
        patch.tool_calls = Some(carry.tools);
        patch.compactions = Some(carry.compactions);
        if last_at > 0 {
            patch.last_activity_ms = Some(last_at.max(t.updated_at_ms));
        }
        // Whether the thread is alive is the live poll's call; only claim working/idle
        // while it is still being written to.
        let quiet = now_ms() - mtime.unwrap_or(0) > self.cfg.stale_after_ms;
        if turn_seen && !quiet {
            patch.state = Some(if carry.open_turn { State::Working } else { State::Idle });
        }
        update.patch = patch;
        update.usage = usage;
        Ok(Some(update))
    }
}

impl Adapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn poll_live(&mut self, store: &Store) -> Result<usize> {
        let now = now_ms();
        // unlimited: the 24 h window and `tokens_used > 0` are the real bound, and
        // truncating here would drop the newest threads, which are the live ones.
        let threads = self.threads(now - WINDOW_MS, -1)?;

        let mut alive: Vec<String> = Vec::new();
        let mut updates: Vec<SessionUpdate> = Vec::new();
        let mut seen = HashMap::with_capacity(threads.len());

        for t in &threads {
            let meta = std::fs::metadata(&t.rollout_path).ok();
            let mtime = meta.as_ref().and_then(mtime_ms);

            // Ordered by how much the signal is worth. Writer locks are deliberately not
            // consulted: they go stale for hours and live `codex exec` threads have none.
            let (ended, signal, confidence, ended_at) = if t.archived {
                (true, "archived", 1.0, t.archived_at_ms.or(Some(t.updated_at_ms)))
            } else if meta.is_none() {
                (true, "archived", 0.9, Some(t.updated_at_ms))
            } else if now - t.updated_at_ms > self.cfg.stale_after_ms
                && now - mtime.unwrap_or(0) > self.cfg.stale_after_ms
            {
                (true, "mtime_stale", 0.6, Some(t.updated_at_ms.max(mtime.unwrap_or(0))))
            } else {
                (false, "", 0.0, None)
            };

            if !ended {
                alive.push(t.id.clone());
            }
            let was = self.seen.get(&t.id).copied();
            seen.insert(t.id.clone(), (t.updated_at_ms, ended));
            if was == Some((t.updated_at_ms, ended)) {
                continue;
            }

            let mut update = SessionUpdate::new(Harness::Codex, t.id.clone());
            let mut patch = base_patch(t);
            patch.source_bytes = meta.as_ref().map(|m| m.len() as i64);
            patch.source_mtime_ms = mtime;
            if ended {
                patch.state = Some(State::Ended);
                patch.end_signal = Some(signal.to_string());
                patch.end_confidence = Some(confidence);
                patch.ended_at_ms = ended_at;
            } else if !matches!(was, Some((_, false))) {
                // first sight, or a thread that got written to again after we ended it
                patch.state = Some(State::Idle);
            }
            update.patch = patch;
            updates.push(update);
        }

        self.seen = seen;
        let changed = updates.len();
        store.apply_updates(&updates)?;
        // anything we still hold as live but the index no longer shows has gone quiet
        let dropped = store.end_missing(Harness::Codex, &alive, "mtime_stale", 0.6)?;
        Ok(changed + dropped)
    }

    fn scan(&mut self, store: &Store) -> Result<usize> {
        let since = store
            .watermark(WATERMARK)?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or_else(|| now_ms() - WINDOW_MS);
        let threads = self.threads(since, MAX_SCAN)?;

        let mut updates = Vec::new();
        let mut high = since;
        for t in &threads {
            high = high.max(t.updated_at_ms);
            if !Path::new(&t.rollout_path).exists() {
                continue;
            }
            match self.scan_thread(store, t) {
                Ok(Some(update)) => updates.push(update),
                Ok(None) => {}
                Err(e) => tracing::warn!(thread = %t.id, error = %e, "codex rollout tail failed"),
            }
        }

        store.apply_updates(&updates)?;
        if high > since {
            store.set_watermark(WATERMARK, &high.to_string())?;
        }
        Ok(updates.len())
    }
}

// -- index ---------------------------------------------------------------

struct Thread {
    id: String,
    rollout_path: String,
    created_at_ms: i64,
    updated_at_ms: i64,
    kind: String,
    parent_id: Option<String>,
    depth: i64,
    model: Option<String>,
    effort: Option<String>,
    provider: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    title: Option<String>,
    first_user_message: Option<String>,
    archived: bool,
    archived_at_ms: Option<i64>,
}

fn query_threads(conn: &Connection, since_ms: i64, limit: i64) -> Result<Vec<Thread>> {
    let mut stmt = conn.prepare_cached(
        r#"
SELECT t.id, t.rollout_path, t.created_at_ms, t.updated_at_ms, t.source, t.thread_source,
       t.model, t.reasoning_effort, t.model_provider, t.cwd, t.git_branch,
       COALESCE(NULLIF(t.name, ''), NULLIF(t.title, '')), t.first_user_message,
       t.archived, t.archived_at, e.parent_thread_id
FROM threads t
LEFT JOIN thread_spawn_edges e ON e.child_thread_id = t.id
WHERE t.updated_at_ms >= ?1 AND t.tokens_used > 0
ORDER BY t.updated_at_ms ASC
LIMIT ?2
"#,
    )?;
    let rows = stmt
        .query_map(rusqlite::params![since_ms, limit], |r| {
            let updated: i64 = r.get(3)?;
            let source: String = r.get(4)?;
            let thread_source: Option<String> = r.get(5)?;
            let edge_parent: Option<String> = r.get(15)?;
            let (kind, parent, depth) = classify_source(&source, thread_source.as_deref(), edge_parent);
            Ok(Thread {
                id: r.get(0)?,
                rollout_path: r.get(1)?,
                created_at_ms: r.get::<_, Option<i64>>(2)?.unwrap_or(updated),
                updated_at_ms: updated,
                kind,
                parent_id: parent,
                depth,
                model: r.get(6)?,
                effort: r.get(7)?,
                provider: r.get(8)?,
                cwd: r.get(9)?,
                git_branch: r.get(10)?,
                title: r.get(11)?,
                first_user_message: r.get(12)?,
                archived: r.get::<_, Option<i64>>(13)?.unwrap_or(0) != 0,
                archived_at_ms: r.get::<_, Option<i64>>(14)?.map(|s| s * 1000),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `threads.source` is either a plain surface name or, for spawned subagents, an
/// embedded JSON object carrying the parent link and the nesting depth.
fn classify_source(
    source: &str,
    thread_source: Option<&str>,
    edge_parent: Option<String>,
) -> (String, Option<String>, i64) {
    let mut parent = edge_parent;
    let mut depth = 0;
    let mut subagent = thread_source == Some("subagent");

    if source.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(source) {
            if let Some(spawn) = v.pointer("/subagent/thread_spawn") {
                subagent = true;
                if let Some(p) = spawn.get("parent_thread_id").and_then(Value::as_str) {
                    parent = Some(p.to_string());
                }
                depth = spawn.get("depth").and_then(Value::as_i64).unwrap_or(1);
            }
        }
    }

    let kind = if subagent {
        if depth == 0 {
            depth = 1;
        }
        "subagent"
    } else if thread_source == Some("automation") {
        "automation"
    } else if source == "exec" {
        "exec"
    } else {
        "interactive"
    };
    if !subagent {
        parent = None;
        depth = 0;
    }
    (kind.to_string(), parent, depth)
}

fn base_patch(t: &Thread) -> SessionPatch {
    SessionPatch {
        parent_id: t.parent_id.clone(),
        depth: Some(t.depth),
        kind: Some(t.kind.clone()),
        source_path: Some(t.rollout_path.clone()),
        cwd: t.cwd.clone(),
        git_branch: t.git_branch.clone(),
        provider: t.provider.clone(),
        model: t.model.clone(),
        effort: t.effort.clone(),
        title: short(t.title.as_deref(), TITLE_MAX),
        first_user_message: short(t.first_user_message.as_deref(), MESSAGE_MAX),
        started_at_ms: Some(t.created_at_ms),
        last_activity_ms: Some(t.updated_at_ms),
        ..Default::default()
    }
}

// -- rollout records -----------------------------------------------------

enum Ev {
    /// session_meta belonging to another thread: the replayed history of a fork.
    Foreign,
    /// This thread's own header; some subagent variants name their parent only here.
    Meta { parent: Option<String> },
    Usage { cum: Usage, last: Usage, window: Option<i64> },
    TurnStart { window: Option<i64> },
    TurnEnd,
    Tool,
    Compact,
    Context { model: Option<String>, effort: Option<String> },
}

/// Every record we care about names itself within the first 82 bytes (measured over
/// 20k records), so most of a 40 MB rollout never reaches the JSON parser.
fn interesting(line: &[u8]) -> bool {
    const NEEDLES: [&[u8]; 9] = [
        b"\"session_meta\"",
        b"\"token_count\"",
        b"\"task_started\"",
        b"\"task_complete\"",
        b"\"turn_aborted\"",
        b"\"custom_tool_call\"",
        b"\"function_call\"",
        b"\"turn_context\"",
        b"\"compacted\"",
    ];
    let head = &line[..line.len().min(256)];
    NEEDLES.iter().any(|n| head.windows(n.len()).any(|w| w == *n))
}

fn classify(v: &Value, thread_id: &str) -> Option<Ev> {
    let payload = v.get("payload")?;
    match v.get("type")?.as_str()? {
        "session_meta" => {
            // NOT payload.session_id: on a subagent that field holds the root thread.
            let id = payload.get("id").and_then(Value::as_str)?;
            if id != thread_id {
                Some(Ev::Foreign)
            } else {
                Some(Ev::Meta {
                    parent: text(payload, "parent_thread_id")
                        .or_else(|| text(payload, "forked_from_id")),
                })
            }
        }
        "compacted" => Some(Ev::Compact),
        "turn_context" => Some(Ev::Context {
            model: text(payload, "model"),
            effort: text(payload, "effort"),
        }),
        "response_item" => match payload.get("type")?.as_str()? {
            "custom_tool_call" | "function_call" => Some(Ev::Tool),
            _ => None,
        },
        "event_msg" => match payload.get("type")?.as_str()? {
            "token_count" => {
                let info = payload.get("info")?;
                Some(Ev::Usage {
                    cum: usage_of(info.get("total_token_usage")?),
                    last: info.get("last_token_usage").map(usage_of).unwrap_or_default(),
                    window: info.get("model_context_window").and_then(Value::as_i64),
                })
            }
            "task_started" => Some(Ev::TurnStart {
                window: payload.get("model_context_window").and_then(Value::as_i64),
            }),
            // an aborted turn is one interrupted turn, never the end of the session
            "task_complete" | "turn_aborted" => Some(Ev::TurnEnd),
            _ => None,
        },
        _ => None,
    }
}

/// Codex reports a running total, not per-turn usage. `input_tokens` includes the
/// cached and cache-write parts; `reasoning_output_tokens` is part of `output_tokens`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Usage {
    input: i64,
    cached: i64,
    cache_write: i64,
    output: i64,
    reasoning: i64,
    total: i64,
}

impl Usage {
    fn since(&self, prev: &Usage) -> Usage {
        Usage {
            input: (self.input - prev.input).max(0),
            cached: (self.cached - prev.cached).max(0),
            cache_write: (self.cache_write - prev.cache_write).max(0),
            output: (self.output - prev.output).max(0),
            reasoning: (self.reasoning - prev.reasoning).max(0),
            total: (self.total - prev.total).max(0),
        }
    }

    /// This turn's usage can never exceed the running total it is part of.
    fn clamp_to(&self, cum: &Usage) -> Usage {
        Usage {
            input: self.input.min(cum.input),
            cached: self.cached.min(cum.cached),
            cache_write: self.cache_write.min(cum.cache_write),
            output: self.output.min(cum.output),
            reasoning: self.reasoning.min(cum.reasoning),
            total: self.total.min(cum.total),
        }
    }
}

fn usage_of(v: &Value) -> Usage {
    Usage {
        input: num(v, "input_tokens"),
        cached: num(v, "cached_input_tokens"),
        cache_write: num(v, "cache_write_input_tokens"),
        output: num(v, "output_tokens"),
        reasoning: num(v, "reasoning_output_tokens"),
        total: num(v, "total_tokens"),
    }
}

/// What survives between two tails of the same rollout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Carry {
    cum: Option<Usage>,
    turns: i64,
    tools: i64,
    compactions: i64,
    open_turn: bool,
}

fn carry_key(thread_id: &str) -> String {
    format!("codex.thread:{thread_id}")
}

fn load_carry(store: &Store, thread_id: &str) -> Result<Carry> {
    Ok(store
        .watermark(&carry_key(thread_id))?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

fn save_carry(store: &Store, thread_id: &str, carry: &Carry) -> Result<()> {
    store.set_watermark(&carry_key(thread_id), &serde_json::to_string(carry)?)
}

// -- small helpers -------------------------------------------------------

fn num(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn text(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn ts_ms(v: &Value) -> Option<i64> {
    let s = v.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Codex titles are the whole first prompt — up to 3 KB of markdown on this machine.
fn short(text: Option<&str>, max: usize) -> Option<String> {
    let flat = text?.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let flat = redact(&flat);
    match flat.char_indices().nth(max) {
        Some((cut, _)) => Some(format!("{}…", &flat[..cut])),
        None => Some(flat),
    }
}
