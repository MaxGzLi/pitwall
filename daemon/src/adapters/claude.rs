//! Claude Code. Two sources answering two different questions:
//!
//!   `~/.claude/sessions/<pid>.json` — one file per live process, deleted on exit.
//!     The only trustworthy liveness signal, so it drives state and end detection.
//!   `~/.claude/projects/<slug>/*.jsonl` — append-only transcripts, hundreds of MB.
//!     Tail-parsed from a stored byte offset for tokens, turns, titles.
//!
//! The sibling `<pid>.<sha>.key` files in the registry are credential material and
//! are never opened.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::adapters::Adapter;
use crate::config::Config;
use crate::model::*;
use crate::redact::redact;
use crate::store::Store;

const TITLE_MAX: usize = 200;
const FIRST_MESSAGE_MAX: usize = 500;

/// An assistant message is written one record per content block and the token
/// counts on the earlier records are partial (`output_tokens` grows to its final
/// value only on the last one). The store dedups on `requestId` with INSERT OR
/// IGNORE, so a request may be emitted exactly once — when it is complete. A
/// request is complete when a later request supersedes it, or when the file has
/// been quiet this long.
const SETTLE_MS: i64 = 60_000;

/// Text the harness injects around slash commands and hooks; not a user prompt.
const NOT_A_PROMPT: [&str; 5] = [
    "<command-name>",
    "<local-command-stdout>",
    "<local-command-caveat>",
    "<system-reminder>",
    "<user-prompt-submit-hook>",
];

pub struct ClaudeAdapter {
    cfg: Config,
    /// Per transcript: what the last scan saw, so unchanged files are never opened.
    seen: HashMap<String, FileState>,
    /// Subagents whose transcript is still being appended to. They never appear in
    /// the process registry, so `poll_live` must be told not to end them.
    live_subagents: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct FileState {
    bytes: i64,
    mtime_ms: i64,
    offset: i64,
    counts: Counts,
}

/// Cumulative per session. The store keeps these with MAX(), so an adapter that
/// only ever saw one tail must still report the running total — hence the
/// watermark round-trip, which survives a daemon restart.
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    turn_records: i64,
    prompts: i64,
    tools: i64,
    compactions: i64,
}

impl Counts {
    fn parse(s: &str) -> Self {
        let mut it = s.split(':').map(|v| v.parse::<i64>().unwrap_or(0));
        Self {
            turn_records: it.next().unwrap_or(0),
            prompts: it.next().unwrap_or(0),
            tools: it.next().unwrap_or(0),
            compactions: it.next().unwrap_or(0),
        }
    }

    fn encode(&self) -> String {
        format!("{}:{}:{}:{}", self.turn_records, self.prompts, self.tools, self.compactions)
    }

    /// `turn_duration` records only exist in recent Claude Code versions; older
    /// transcripts have none at all, and `turns > 0` is what makes a session
    /// eligible for summarising. Fall back to counting real user prompts.
    fn turns(&self) -> i64 {
        self.turn_records.max(self.prompts)
    }
}

impl ClaudeAdapter {
    pub fn new(cfg: &Config) -> Self {
        Self { cfg: cfg.clone(), seen: HashMap::new(), live_subagents: Vec::new() }
    }

    /// One entry per live process. `None` means the registry itself was unreadable,
    /// which must never be mistaken for "nothing is running".
    fn registry(&self) -> Option<Vec<Registry>> {
        let dir = self.cfg.claude_registry_dir();
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // <pid>.json only: siblings include *.key credential files.
            let Some(pid) = name.strip_suffix(".json").and_then(|s| s.parse::<i64>().ok()) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(session_id) = v.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            let mtime = std::fs::metadata(&path).map(|m| mtime_ms(&m)).unwrap_or_else(|_| now_ms());
            out.push(Registry {
                pid,
                session_id: session_id.to_string(),
                cwd: str_of(&v, "cwd"),
                kind: str_of(&v, "kind"),
                proc_start: str_of(&v, "procStart"),
                status: str_of(&v, "status").unwrap_or_default(),
                started_at_ms: v.get("startedAt").and_then(Value::as_i64),
                updated_at_ms: v
                    .get("updatedAt")
                    .and_then(Value::as_i64)
                    .max(v.get("statusUpdatedAt").and_then(Value::as_i64)),
                mtime_ms: mtime,
            });
        }
        Some(out)
    }

    /// Every transcript under `projects/`, classified. Top-level sessions sit
    /// directly in the project slug directory; subagents live under
    /// `<slug>/<parent-uuid>/subagents/**/agent-<agentId>.jsonl`.
    fn transcripts(&self) -> Vec<Transcript> {
        let mut out = Vec::new();
        let Ok(slugs) = std::fs::read_dir(self.cfg.claude_projects_dir()) else {
            return out;
        };
        for slug in slugs.flatten() {
            let Ok(entries) = std::fs::read_dir(slug.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    collect_subagents(&path.join("subagents"), name, &mut out);
                } else if let Some(stem) = name.strip_suffix(".jsonl") {
                    push_transcript(&path, stem.to_string(), None, false, &mut out);
                }
            }
        }
        out
    }

    fn load_state(&self, store: &Store, session_id: &str) -> Result<FileState> {
        let counts = store
            .watermark(&counts_key(session_id))?
            .map(|v| Counts::parse(&v))
            .unwrap_or_default();
        Ok(FileState {
            bytes: -1,
            mtime_ms: -1,
            offset: store.scan_offset(Harness::Claude, session_id)?,
            counts,
        })
    }
}

struct Registry {
    pid: i64,
    session_id: String,
    cwd: Option<String>,
    kind: Option<String>,
    proc_start: Option<String>,
    status: String,
    started_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
    mtime_ms: i64,
}

struct Transcript {
    session_id: String,
    parent_id: Option<String>,
    subagent: bool,
    path: PathBuf,
    bytes: i64,
    mtime_ms: i64,
}

impl Adapter for ClaudeAdapter {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn poll_live(&mut self, store: &Store) -> Result<usize> {
        let Some(entries) = self.registry() else {
            return Ok(0);
        };

        let starts = proc_starts(&entries.iter().map(|e| e.pid).collect::<Vec<_>>());
        let mut alive = self.live_subagents.clone();
        let mut updates = Vec::new();

        for e in entries {
            let mut u = SessionUpdate::new(Harness::Claude, &e.session_id);
            u.patch.pid = Some(e.pid);
            u.patch.cwd = e.cwd.clone();
            u.patch.kind = e.kind.clone();
            u.patch.provider = Some("anthropic".into());
            u.patch.started_at_ms = e.started_at_ms;
            u.patch.proc_start = e.proc_start.as_deref().map(proc_start_key);

            let running = pid_alive(e.pid)
                && match (&e.proc_start, starts.get(&e.pid)) {
                    // The registry renders procStart in UTC, `ps` in local time,
                    // so both sides are normalised before comparing. A mismatch
                    // means the pid was recycled by an unrelated process.
                    (Some(want), Some(have)) => proc_start_key(want) == proc_start_key(have),
                    (_, None) => false,
                    (None, _) => true,
                };

            if running {
                alive.push(e.session_id.clone());
                u.patch.state = Some(state_of(&e.status));
                u.patch.last_activity_ms = Some(e.updated_at_ms.unwrap_or(e.mtime_ms));
            } else {
                // A registry file outliving its process means Claude Code died
                // without cleaning up; the file's mtime is its last sign of life.
                u.patch.state = Some(State::Ended);
                u.patch.ended_at_ms = Some(e.mtime_ms);
                u.patch.last_activity_ms = Some(e.mtime_ms);
                u.patch.end_signal = Some("pid_dead".into());
                u.patch.end_confidence = Some(1.0);
            }
            updates.push(u);
        }

        store.apply_updates(&updates)?;
        let ended = store.end_missing(Harness::Claude, &alive, "registry_gone", 1.0)?;
        Ok(updates.len() + ended)
    }

    fn scan(&mut self, store: &Store) -> Result<usize> {
        let now = now_ms();
        let live_ids: HashSet<String> = self
            .registry()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.session_id)
            .collect();

        let mut updates = Vec::new();
        let mut live_subagents = Vec::new();

        for t in self.transcripts() {
            let stale = now - t.mtime_ms > self.cfg.stale_after_ms;
            if t.subagent && !stale {
                live_subagents.push(t.session_id.clone());
            }

            let st = match self.seen.get(&t.session_id) {
                Some(st) => *st,
                None => self.load_state(store, &t.session_id)?,
            };
            let mut offset = st.offset;
            let mut counts = st.counts;
            if t.bytes < offset {
                // truncated or rotated underneath us
                offset = 0;
                counts = Counts::default();
            }

            let settled = now - t.mtime_ms > SETTLE_MS;
            let unchanged = t.bytes == st.bytes && t.mtime_ms == st.mtime_ms;
            if unchanged && (offset == t.bytes || !settled) {
                continue;
            }

            let parsed = match parse_tail(&t.path, offset, counts, settled) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(path = %t.path.display(), error = %e, "claude transcript unreadable");
                    continue;
                }
            };
            self.seen.insert(
                t.session_id.clone(),
                FileState {
                    bytes: t.bytes,
                    mtime_ms: t.mtime_ms,
                    offset: parsed.consumed,
                    counts: parsed.counts,
                },
            );
            if parsed.counts.encode() != st.counts.encode() {
                store.set_watermark(&counts_key(&t.session_id), &parsed.counts.encode())?;
            }

            let last_activity = parsed.last_ts.unwrap_or(t.mtime_ms);
            let mut u = SessionUpdate::new(Harness::Claude, &t.session_id);
            u.patch.source_path = Some(t.path.to_string_lossy().into_owned());
            u.patch.source_bytes = Some(t.bytes);
            u.patch.source_mtime_ms = Some(t.mtime_ms);
            u.patch.scan_offset = Some(parsed.consumed);
            u.patch.provider = Some("anthropic".into());
            // Only a tail that started at byte 0 has actually seen the launch cwd;
            // otherwise leave whatever the registry wrote alone.
            if offset == 0 {
                u.patch.cwd = parsed.cwd.clone();
            }
            u.patch.git_branch = parsed.git_branch.clone();
            u.patch.effort = parsed.effort.clone();
            u.patch.model = parsed.model.clone();
            u.patch.title = parsed.title.clone();
            u.patch.first_user_message = parsed.first_user.clone();
            u.patch.started_at_ms = parsed.first_ts;
            u.patch.last_activity_ms = Some(last_activity);
            u.patch.turns = Some(parsed.counts.turns());
            u.patch.tool_calls = Some(parsed.counts.tools);
            u.patch.compactions = Some(parsed.counts.compactions);
            u.usage = parsed.usage;

            if t.subagent {
                let meta = subagent_meta(&t.path);
                u.patch.kind = Some("subagent".into());
                u.patch.depth = Some(meta.depth);
                u.patch.parent_id = t.parent_id.clone();
                if u.patch.title.is_none() {
                    u.patch.title = meta.title;
                }
                if stale {
                    u.patch.state = Some(State::Ended);
                    u.patch.ended_at_ms = Some(last_activity);
                    u.patch.end_signal = Some("mtime_stale".into());
                    u.patch.end_confidence = Some(0.6);
                } else {
                    u.patch.state = Some(State::Working);
                }
            } else if !live_ids.contains(&t.session_id) && stale {
                // No registry file means the process is gone: Claude Code removes
                // it on exit. Only trusted once the transcript has gone quiet too,
                // so a session starting up is never buried.
                u.patch.state = Some(State::Ended);
                u.patch.ended_at_ms = Some(last_activity);
                u.patch.end_signal = Some("registry_gone".into());
                u.patch.end_confidence = Some(1.0);
            }

            updates.push(u);
        }

        self.live_subagents = live_subagents;
        store.apply_updates(&updates)?;
        Ok(updates.len())
    }
}

// -- transcript parsing ----------------------------------------------------

#[derive(Default)]
struct Parsed {
    /// New scan offset: the first byte not yet accounted for.
    consumed: i64,
    counts: Counts,
    usage: Vec<UsageDelta>,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    cwd: Option<String>,
    git_branch: Option<String>,
    effort: Option<String>,
    model: Option<String>,
    title: Option<String>,
    first_user: Option<String>,
}

/// One assistant request, accumulated across the records of its content blocks.
struct Group {
    rid: String,
    start: i64,
    counts_at_start: Counts,
    at_ms: i64,
    model: Option<String>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_create: i64,
    reasoning: i64,
}

impl Group {
    fn absorb(&mut self, usage: Option<&Value>, at_ms: Option<i64>, model: Option<String>) {
        if let Some(ms) = at_ms {
            self.at_ms = self.at_ms.max(ms);
        }
        if self.model.is_none() {
            self.model = model;
        }
        let Some(u) = usage else { return };
        // Every field is monotonic across the records of one request, so max()
        // picks the final value whichever record we happen to stop on.
        self.input = self.input.max(num(u, "input_tokens"));
        self.output = self.output.max(num(u, "output_tokens"));
        self.cache_read = self.cache_read.max(num(u, "cache_read_input_tokens"));
        self.cache_create = self.cache_create.max(num(u, "cache_creation_input_tokens"));
        let thinking = u
            .get("output_tokens_details")
            .map(|d| num(d, "thinking_tokens"))
            .unwrap_or(0);
        self.reasoning = self.reasoning.max(thinking);
    }

    fn into_delta(self, fallback_ms: i64) -> Option<UsageDelta> {
        if self.input + self.output + self.cache_read + self.cache_create == 0 {
            return None; // "<synthetic>" turns, which never hit the API
        }
        Some(UsageDelta {
            dedup_key: self.rid,
            at_ms: if self.at_ms > 0 { self.at_ms } else { fallback_ms },
            model: self.model,
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_create: self.cache_create,
            reasoning: self.reasoning,
        })
    }
}

fn parse_tail(path: &Path, from: i64, counts: Counts, settled: bool) -> Result<Parsed> {
    let file = std::fs::File::open(path)?;
    let fallback_ms = mtime_ms(&file.metadata()?);
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    reader.seek(SeekFrom::Start(from as u64))?;
    parse_lines(&mut reader, from, counts, settled, fallback_ms)
}

fn parse_lines(
    reader: &mut impl BufRead,
    from: i64,
    counts: Counts,
    settled: bool,
    fallback_ms: i64,
) -> Result<Parsed> {
    let mut p = Parsed { consumed: from, counts, ..Default::default() };
    let mut custom_title: Option<String> = None;
    let mut pending: Option<Group> = None;
    let mut pos = from;
    let mut line = Vec::new();

    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            break; // half-written record; leave it for the next scan
        }
        let start = pos;
        pos += n as i64;

        if let Ok(v) = serde_json::from_slice::<Value>(&line) {
            let ts = v.get("timestamp").and_then(Value::as_str).and_then(iso_ms);
            if let Some(ms) = ts {
                p.first_ts = Some(p.first_ts.map_or(ms, |x: i64| x.min(ms)));
                p.last_ts = Some(p.last_ts.map_or(ms, |x: i64| x.max(ms)));
            }
            // A session's cwd is where it was launched, which is what the process
            // registry reports. Later records carry whatever directory a tool had
            // cd'd into, so taking the last one renames the project mid-session.
            keep_first(&mut p.cwd, str_of(&v, "cwd"));
            // last wins: a session can change branch, effort or model mid-flight
            keep_last(&mut p.git_branch, str_of(&v, "gitBranch"));
            keep_last(&mut p.effort, str_of(&v, "effort"));

            match v.get("type").and_then(Value::as_str).unwrap_or("") {
                "assistant" => {
                    let msg = v.get("message");
                    let rid = v
                        .get("requestId")
                        .and_then(Value::as_str)
                        .or_else(|| msg.and_then(|m| m.get("id")).and_then(Value::as_str));
                    let model = msg
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .filter(|m| *m != "<synthetic>")
                        .map(str::to_string);
                    keep_last(&mut p.model, model.clone());

                    if let Some(rid) = rid {
                        let same = pending.as_ref().is_some_and(|g| g.rid == rid);
                        if !same {
                            if let Some(g) = pending.take() {
                                if let Some(d) = g.into_delta(fallback_ms) {
                                    p.usage.push(d);
                                }
                            }
                            pending = Some(Group {
                                rid: rid.to_string(),
                                start,
                                counts_at_start: p.counts,
                                at_ms: 0,
                                model: None,
                                input: 0,
                                output: 0,
                                cache_read: 0,
                                cache_create: 0,
                                reasoning: 0,
                            });
                        }
                        if let Some(g) = pending.as_mut() {
                            g.absorb(msg.and_then(|m| m.get("usage")), ts, model);
                        }
                    }
                    // Each record carries exactly one content block and tool_use
                    // ids never repeat, so this is an exact count.
                    if let Some(blocks) = msg.and_then(|m| m.get("content")).and_then(Value::as_array) {
                        p.counts.tools += blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                            .count() as i64;
                    }
                }
                "user" => {
                    if !v.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
                        if let Some(text) = user_prompt(&v) {
                            p.counts.prompts += 1;
                            if p.first_user.is_none() {
                                p.first_user = Some(truncate(&redact(&text), FIRST_MESSAGE_MAX));
                            }
                        }
                    }
                }
                "system" => match v.get("subtype").and_then(Value::as_str).unwrap_or("") {
                    "turn_duration" => p.counts.turn_records += 1,
                    "compact_boundary" => p.counts.compactions += 1,
                    _ => {}
                },
                // Redacted like every other field that reaches the summariser:
                // a title is model- or user-written prose and can carry a key.
                "ai-title" => keep_last(
                    &mut p.title,
                    str_of(&v, "aiTitle").map(|t| truncate(&redact(&t), TITLE_MAX)),
                ),
                "custom-title" => keep_last(
                    &mut custom_title,
                    str_of(&v, "customTitle").map(|t| truncate(&redact(&t), TITLE_MAX)),
                ),
                _ => {}
            }
        }

        // Anything inside the still-open request must be re-read next time.
        p.consumed = pending.as_ref().map_or(pos, |g| g.start);
    }

    if let Some(g) = pending {
        if settled {
            p.consumed = pos;
            if let Some(d) = g.into_delta(fallback_ms) {
                p.usage.push(d);
            }
        } else {
            p.consumed = g.start;
            p.counts = g.counts_at_start;
        }
    }
    if custom_title.is_some() {
        p.title = custom_title;
    }
    Ok(p)
}

/// `.message.content` is either a bare string or an array of blocks. Only real
/// prompts count — tool results and the harness's own injected text do not.
fn user_prompt(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || NOT_A_PROMPT.iter().any(|tag| text.starts_with(tag)) {
        return None;
    }
    Some(text.to_string())
}

struct SubagentMeta {
    title: Option<String>,
    depth: i64,
}

/// `agent-<id>.meta.json` sits next to the transcript and names the subagent.
fn subagent_meta(transcript: &Path) -> SubagentMeta {
    let mut meta = SubagentMeta { title: None, depth: 1 };
    let Some(name) = transcript.file_name().and_then(|s| s.to_str()) else {
        return meta;
    };
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return meta;
    };
    let path = transcript.with_file_name(format!("{stem}.meta.json"));
    let Ok(v) = std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|t| {
        serde_json::from_str::<Value>(&t).map_err(anyhow::Error::from)
    }) else {
        return meta;
    };
    meta.title = str_of(&v, "description")
        .or_else(|| str_of(&v, "agentType"))
        .map(|t| truncate(&t, TITLE_MAX));
    if let Some(d) = v.get("spawnDepth").and_then(Value::as_i64) {
        meta.depth = d.max(1);
    }
    meta
}

fn collect_subagents(dir: &Path, parent_id: &str, out: &mut Vec<Transcript>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_subagents(&path, parent_id, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // agent-<agentId>.jsonl; journal.jsonl is workflow bookkeeping, not a session.
        let Some(id) = name.strip_suffix(".jsonl").and_then(|s| s.strip_prefix("agent-")) else {
            continue;
        };
        push_transcript(&path, id.to_string(), Some(parent_id.to_string()), true, out);
    }
}

fn push_transcript(
    path: &Path,
    session_id: String,
    parent_id: Option<String>,
    subagent: bool,
    out: &mut Vec<Transcript>,
) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    out.push(Transcript {
        session_id,
        parent_id,
        subagent,
        path: path.to_path_buf(),
        bytes: meta.len() as i64,
        mtime_ms: mtime_ms(&meta),
    });
}

// -- process liveness ------------------------------------------------------

fn pid_alive(pid: i64) -> bool {
    pid > 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// Start time per pid, asked for in UTC because that is how the registry writes it.
fn proc_starts(pids: &[i64]) -> HashMap<i64, String> {
    let mut out = HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let list = pids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let Ok(output) = std::process::Command::new("ps")
        .env("TZ", "UTC")
        .args(["-o", "pid=,lstart=", "-p", &list])
        .output()
    else {
        return out;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim();
        let Some((pid, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if let Ok(pid) = pid.parse::<i64>() {
            out.insert(pid, rest.trim().to_string());
        }
    }
    out
}

/// `Tue Aug 18 06:01:03 2026` -> `2026-08-18T06:01:03Z`. Both the registry and
/// `ps` use this layout but pad the day differently, so neither can be compared
/// as a raw string.
fn proc_start_key(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // The leading weekday carries no information and chrono rejects the whole
    // string if it disagrees with the date.
    flat.split_once(' ')
        .and_then(|(_, rest)| chrono::NaiveDateTime::parse_from_str(rest, "%b %e %H:%M:%S %Y").ok())
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or(flat)
}

fn state_of(status: &str) -> State {
    match status {
        "busy" => State::Working,
        "waiting" => State::Waiting,
        // "shell" is the user dropped into a shell inside the session: alive, not working.
        "idle" | "shell" => State::Idle,
        _ => State::Unknown,
    }
}

// -- small helpers ---------------------------------------------------------

fn counts_key(session_id: &str) -> String {
    format!("claude:counts:{session_id}")
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

fn num(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Keeps the last non-empty value seen.
fn keep_last(slot: &mut Option<String>, value: Option<String>) {
    if value.is_some() {
        *slot = value;
    }
}

/// Keeps the first non-empty value seen.
fn keep_first(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value;
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(data: &[u8], from: i64, counts: Counts, settled: bool) -> Parsed {
        let mut cur = Cursor::new(&data[from as usize..]);
        parse_lines(&mut cur, from, counts, settled, 1).unwrap()
    }

    /// End of the last complete record: a transcript being written right now has a
    /// half-line at the end, which must never be consumed.
    fn last_line_end(data: &[u8]) -> i64 {
        data.iter().rposition(|b| *b == b'\n').map(|i| i as i64 + 1).unwrap_or(0)
    }

    /// Not a fixture test. It parses whatever Claude Code transcripts this machine
    /// actually has, on a frozen in-memory copy so a live session cannot race it,
    /// and asserts the properties that silently corrupt the numbers when broken.
    #[test]
    fn parses_real_transcripts() {
        let cfg = Config::load().unwrap();
        let transcripts = ClaudeAdapter::new(&cfg).transcripts();
        if transcripts.is_empty() {
            eprintln!("no local claude transcripts; nothing to prove");
            return;
        }

        let (mut bytes, mut requests, mut records, mut tokens) = (0i64, 0usize, 0usize, 0i64);
        for t in &transcripts {
            let data = std::fs::read(&t.path).unwrap();
            let end = last_line_end(&data);
            bytes += data.len() as i64;
            records += data.iter().filter(|b| **b == b'\n').count();

            let full = parse(&data, 0, Counts::default(), true);
            assert_eq!(full.consumed, end, "{} left bytes unread", t.path.display());
            requests += full.usage.len();
            tokens += full.usage.iter().map(|u| u.total()).sum::<i64>();

            let mut keys: Vec<&str> = full.usage.iter().map(|u| u.dedup_key.as_str()).collect();
            let before = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(before, keys.len(), "{} emitted a request twice", t.path.display());

            // a second pass from the recorded offset must find nothing at all
            let again = parse(&data, full.consumed, full.counts, true);
            assert!(again.usage.is_empty(), "{} re-read usage", t.path.display());
            assert_eq!(again.consumed, full.consumed);
            assert_eq!(again.counts.encode(), full.counts.encode());

            // cutting the file anywhere — as a live tail does — must not change the totals
            if end > 4096 {
                let cut = last_line_end(&data[..data.len() / 2]);
                let head = parse(&data[..cut as usize], 0, Counts::default(), false);
                let tail = parse(&data, head.consumed, head.counts, true);
                let split: i64 = head.usage.iter().chain(tail.usage.iter()).map(|u| u.total()).sum();
                let whole: i64 = full.usage.iter().map(|u| u.total()).sum();
                assert_eq!(split, whole, "{} split parse lost tokens", t.path.display());
                assert_eq!(tail.counts.encode(), full.counts.encode(), "{} split lost counts", t.path.display());
            }
        }
        eprintln!(
            "{} transcripts, {} records, {:.1} MB -> {} priced requests, {} tokens",
            transcripts.len(),
            records,
            bytes as f64 / 1e6,
            requests,
            tokens
        );
    }

    /// The registry renders procStart in UTC, `ps` in local time and pads the day
    /// differently; only the normalised forms may be compared.
    #[test]
    fn proc_start_normalises() {
        assert_eq!(proc_start_key("Tue Aug 18 06:01:03 2026"), "2026-08-18T06:01:03Z");
        // `ps` space-pads a single-digit day, the registry zero-pads it
        assert_eq!(proc_start_key("Fri Aug  7 01:33:08 2026"), "2026-08-07T01:33:08Z");
        assert_eq!(proc_start_key("Fri Aug 07 01:33:08 2026"), "2026-08-07T01:33:08Z");
        // the UTC/local trap: same wall-clock layout, eight hours apart
        assert_ne!(proc_start_key("Mon Aug 17 01:33:08 2026"), proc_start_key("Mon Aug 17 09:33:08 2026"));
    }

    /// Every live process on this machine right now must verify against `ps`.
    #[test]
    fn live_registry_matches_ps() {
        let cfg = Config::load().unwrap();
        let entries = ClaudeAdapter::new(&cfg).registry().unwrap_or_default();
        let starts = proc_starts(&entries.iter().map(|e| e.pid).collect::<Vec<_>>());
        for e in &entries {
            let want = e.proc_start.as_deref().map(proc_start_key);
            let have = starts.get(&e.pid).map(|s| proc_start_key(s));
            eprintln!(
                "pid {:<7} alive={:<5} registry={:?} ps={:?} status={} session={}",
                e.pid,
                pid_alive(e.pid),
                want,
                have,
                e.status,
                e.session_id
            );
            if pid_alive(e.pid) {
                assert_eq!(want, have, "pid {} start time did not normalise to the same value", e.pid);
            }
        }
    }
}
