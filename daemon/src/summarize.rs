//! Summarises a session once it has ended.
//!
//! Only the human-readable turns are extracted — never tool arguments, tool output
//! or file contents. Transcripts reach tens of megabytes, so the head and the tail
//! are collected through a bounded ring and the middle is dropped: the head holds
//! what the user asked for, the tail holds what actually got done.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tracing::warn;

use crate::config::{self, Config};
use crate::model::Harness;
use crate::redact::redact;
use crate::store::{PendingSummary, Store};

/// How many ended sessions one tick may summarise. Bounded so a slow API call
/// can never stall the collector loop for long.
const PER_TICK: i64 = 3;
/// Below this a transcript has nothing worth a model call.
const MIN_USEFUL_CHARS: usize = 200;
/// No single turn gets to eat the whole budget.
const MAX_TURN_CHARS: usize = 4_000;
/// Lines above this are tool output or base64 signatures, never conversation.
const MAX_LINE_BYTES: usize = 2 << 20;

const SYSTEM_PROMPT: &str = "\
你在为一个本地监控面板总结 AI 编程助手（AI coding agent）的一次会话记录。
读者就是发起这次会话的开发者本人，他扫一眼就要知道：这个 agent 到底干成了什么，以及有没有需要他亲自处理的事。

严格要求：
1. 全部用中文书写。
2. 只依据转录内容，绝对不要编造转录里没有出现的结论、文件名、数字或承诺。转录中间可能被省略，缺什么就不写什么。
3. 输出恰好四行，不多不少：
   第 1 行：标题，不超过 24 个汉字，说清「这次会话在解决什么」，结尾不要标点。
   第 2 行：以 \"做了什么：\" 开头，一句话，不超过 45 个汉字。
   第 3 行：以 \"结果：\" 开头，一句话，不超过 45 个汉字，说清成了还是没成。
   第 4 行：以 \"待办：\" 开头，一句话，不超过 45 个汉字，只写需要开发者本人动手的事；没有就写「无」。
4. 每行都要能单独读懂，不要跨行接续，不要写「见上」「同上」。
5. 不要输出这四行以外的任何内容，不要用代码块包裹，不要加 \"- \" 之类的列表符号。";

pub async fn run_once(cfg: &Config, store: &Store) -> Result<usize> {
    let pending = store.pending_summaries(PER_TICK)?;
    if pending.is_empty() {
        return Ok(0);
    }

    let key = config::deepseek_key(cfg);
    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;

    let mut written = 0;
    for p in &pending {
        match summarise(cfg, store, &client, key.as_deref(), p).await {
            Ok(true) => written += 1,
            Ok(false) => {}
            Err(e) => {
                warn!(harness = %p.harness, session = %p.session_id, error = %e, "summary failed");
                // recorded, so a permanently broken session is never retried in a loop
                let msg = e.to_string();
                store.put_summary(
                    p.harness,
                    &p.session_id,
                    "(总结失败)",
                    None,
                    &cfg.summary_model,
                    0,
                    "failed",
                    Some(&msg),
                )?;
            }
        }
    }
    Ok(written)
}

/// Returns true when a real summary landed.
async fn summarise(
    cfg: &Config,
    store: &Store,
    client: &reqwest::Client,
    key: Option<&str>,
    p: &PendingSummary,
) -> Result<bool> {
    // Codex sometimes already wrote a summary of its own. Tiny coverage, but free
    // and authoritative, and it saves reading a multi-megabyte rollout.
    if p.harness == Harness::Codex {
        if let Some(headline) = codex_native_summary(&cfg.codex_summaries_db(), &p.session_id) {
            store.put_summary(p.harness, &p.session_id, &headline, None, "codex-native", 0, "ok", None)?;
            return Ok(true);
        }
    }

    let text = match p.source_path.as_deref() {
        Some(path) => {
            let (harness, path, budget) = (p.harness, PathBuf::from(path), cfg.summary_max_chars);
            tokio::task::spawn_blocking(move || extract(harness, &path, budget)).await??
        }
        None => String::new(),
    };
    let n = text.chars().count();
    if n < MIN_USEFUL_CHARS {
        store.put_summary(p.harness, &p.session_id, "(内容过少，未总结)", None, "-", n, "skipped", None)?;
        return Ok(false);
    }

    // Nothing reaches the network unscrubbed: real transcripts here carry live keys.
    let text = clamp(&redact(&text), cfg.summary_max_chars);
    let input_chars = text.chars().count();

    let Some(key) = key else {
        store.put_summary(
            p.harness,
            &p.session_id,
            "(未配置 DeepSeek key，未总结)",
            None,
            "-",
            input_chars,
            "skipped",
            None,
        )?;
        return Ok(false);
    };

    let reply = ask(client, cfg, key, p, &text).await?;
    let (headline, body) = parse_reply(&reply).ok_or_else(|| anyhow!("模型未返回可解析的总结"))?;
    store.put_summary(
        p.harness,
        &p.session_id,
        &headline,
        body.as_deref(),
        &cfg.summary_model,
        input_chars,
        "ok",
        None,
    )?;
    Ok(true)
}

// -- the model call ------------------------------------------------------

async fn ask(
    client: &reqwest::Client,
    cfg: &Config,
    key: &str,
    p: &PendingSummary,
    text: &str,
) -> Result<String> {
    let url = format!("{}/chat/completions", cfg.summary_base_url.trim_end_matches('/'));
    let user = format!(
        "会话来源：{}\n项目：{}\n标题：{}\n\n--- 转录开始 ---\n{}\n--- 转录结束 ---",
        p.harness,
        p.project.as_deref().unwrap_or("未知"),
        p.title.as_deref().unwrap_or("无"),
        text
    );
    let body = json!({
        "model": cfg.summary_model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": user },
        ],
        "stream": false,
        "max_tokens": 700,
        // deepseek-v4-flash bills reasoning against max_tokens: left thinking, it
        // spends the whole budget and returns empty content. A recap needs none.
        "thinking": { "type": "disabled" },
    });

    let resp = client.post(&url).bearer_auth(key).json(&body).send().await?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| anyhow!("HTTP {status}: {e}"))?;
    if !status.is_success() {
        // only the provider's message, never the request we sent
        let why = v["error"]["message"].as_str().unwrap_or("no error message");
        return Err(anyhow!("HTTP {status}: {why}"));
    }
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    if content.trim().is_empty() {
        let reason = v["choices"][0]["finish_reason"].as_str().unwrap_or("?");
        return Err(anyhow!("模型返回空内容 (finish_reason={reason})"));
    }
    Ok(content.to_string())
}

/// First line is the headline, the rest is the body. Lenient about the fences,
/// bullets and quotes models like to add.
fn parse_reply(raw: &str) -> Option<(String, Option<String>)> {
    let mut text = raw.trim();
    if let Some(rest) = text.strip_prefix("```") {
        text = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
        text = text.trim_end().trim_end_matches("```").trim();
    }

    let mut lines = text.lines();
    let headline = loop {
        let cleaned = clean_headline(lines.next()?);
        if !cleaned.is_empty() {
            break cleaned;
        }
    };
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Some((headline, if body.is_empty() { None } else { Some(body) }))
}

fn clean_headline(line: &str) -> String {
    let mut s = line.trim();
    s = s.trim_start_matches('#').trim();
    s = s.trim_start_matches("- ").trim();
    s = s.trim_matches(|c| c == '*' || c == '"' || c == '「' || c == '」').trim();
    for prefix in ["标题：", "标题:", "一句话标题：", "一句话标题:"] {
        s = s.strip_prefix(prefix).unwrap_or(s).trim();
    }
    s = s.trim_end_matches(|c| c == '。' || c == '.' || c == '！' || c == '!').trim();
    // over-long headlines get cut, not thrown away
    take_head(s, 48).to_string()
}

// -- transcript extraction ----------------------------------------------

fn extract(harness: Harness, path: &Path, budget: usize) -> Result<String> {
    let file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut c = Collector::new(budget);

    match harness {
        Harness::Claude => {
            let mut seen = HashSet::new();
            scan_lines(BufReader::with_capacity(256 << 10, file), |l| {
                claude_line(l, &mut c, &mut seen)
            })?;
        }
        Harness::Codex => {
            let mut last_agent = String::new();
            scan_lines(BufReader::with_capacity(256 << 10, file), |l| {
                codex_line(l, &mut c, &mut last_agent)
            })?;
        }
        Harness::Dsh => {
            let decoder = zstd::stream::read::Decoder::new(file)
                .with_context(|| format!("zstd decoding {}", path.display()))?;
            scan_lines(BufReader::with_capacity(256 << 10, decoder), |l| dsh_line(l, &mut c))?;
        }
    }
    Ok(c.finish())
}

fn claude_line(line: &[u8], c: &mut Collector, seen: &mut HashSet<u64>) {
    if !(contains(line, br#""type":"user""#)
        || contains(line, br#""type":"text""#)
        || contains(line, b"away_summary"))
    {
        return;
    }
    let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
    match v["type"].as_str() {
        Some("user") => {
            if let Some(t) = text_of(&v["message"]["content"]) {
                c.push("【用户】", &t);
            }
        }
        Some("assistant") => {
            let Some(blocks) = v["message"]["content"].as_array() else { return };
            let req = v["requestId"].as_str().unwrap_or("");
            for b in blocks {
                if b["type"] != "text" {
                    continue; // thinking and tool_use are not conversation
                }
                let Some(t) = b["text"].as_str() else { continue };
                // records repeat per content block, so the same answer arrives many times
                if seen.insert(fingerprint(req, t)) {
                    c.push("【助手】", t);
                }
            }
        }
        // Claude's own recap of what happened while the user was away.
        Some("system") if v["subtype"] == "away_summary" => {
            if let Some(t) = v["content"].as_str() {
                c.push("【回顾】", t);
            }
        }
        _ => {}
    }
}

fn codex_line(line: &[u8], c: &mut Collector, last_agent: &mut String) {
    if !(contains(line, b"user_message")
        || contains(line, b"agent_message")
        || contains(line, b"task_complete"))
    {
        return;
    }
    let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
    if v["type"] != "event_msg" {
        return; // response_item repeats the same messages
    }
    let p = &v["payload"];
    match p["type"].as_str() {
        Some("user_message") => {
            if let Some(t) = p["message"].as_str() {
                c.push("【用户】", t);
            }
        }
        Some("agent_message") => {
            if let Some(t) = p["message"].as_str() {
                last_agent.clear();
                last_agent.push_str(t);
                c.push("【助手】", t);
            }
        }
        // the authoritative ending, but usually identical to the last final_answer
        Some("task_complete") => {
            if let Some(t) = p["last_agent_message"].as_str() {
                if t != last_agent {
                    c.push("【最终回答】", t);
                }
            }
        }
        _ => {}
    }
}

fn dsh_line(line: &[u8], c: &mut Collector) {
    if !(contains(line, br#""type":"user/message""#)
        || contains(line, br#""type":"assistant/message""#))
    {
        return;
    }
    let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
    match v["type"].as_str() {
        Some("user/message") => {
            if let Some(t) = text_of(&v["data"]["content"]) {
                c.push("【用户】", &t);
            }
        }
        Some("assistant/message") => {
            let Some(blocks) = v["data"]["message"]["content"].as_array() else { return };
            for b in blocks {
                if b["type"] != "text" {
                    continue; // reasoning and tool-call are not conversation
                }
                if let Some(t) = b["text"].as_str() {
                    c.push("【助手】", t);
                }
            }
        }
        _ => {}
    }
}

/// Content is either a bare string or a block array; only blocks carrying `.text`
/// are conversation (a `tool_result` block carries `.content` and is skipped).
fn text_of(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let mut out = String::new();
    for b in content.as_array()? {
        if let Some(t) = b.get("text").and_then(Value::as_str) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Head plus tail within a char budget; everything between is dropped.
struct Collector {
    head: Vec<String>,
    head_chars: usize,
    head_budget: usize,
    tail: VecDeque<String>,
    tail_chars: usize,
    tail_budget: usize,
    dropped: bool,
}

impl Collector {
    fn new(budget: usize) -> Self {
        let head_budget = budget / 3;
        Self {
            head: Vec::new(),
            head_chars: 0,
            head_budget,
            tail: VecDeque::new(),
            tail_chars: 0,
            tail_budget: budget - head_budget,
            dropped: false,
        }
    }

    fn push(&mut self, label: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let turn = format!("{label} {}", take_head(text, MAX_TURN_CHARS));
        let n = turn.chars().count();

        if self.head.is_empty() || self.head_chars + n <= self.head_budget {
            self.head_chars += n;
            self.head.push(turn);
            return;
        }
        self.tail_chars += n;
        self.tail.push_back(turn);
        while self.tail_chars > self.tail_budget && self.tail.len() > 1 {
            if let Some(front) = self.tail.pop_front() {
                self.tail_chars -= front.chars().count();
                self.dropped = true;
            }
        }
    }

    fn finish(self) -> String {
        let mut out = self.head.join("\n\n");
        if self.dropped {
            out.push_str("\n\n……（中间省略）……");
        }
        for turn in self.tail {
            out.push_str("\n\n");
            out.push_str(&turn);
        }
        out.trim().to_string()
    }
}

/// Line-at-a-time over a file that may be hundreds of megabytes: one reusable
/// buffer, and over-long lines are dropped rather than ever held in memory.
fn scan_lines(mut rd: impl BufRead, mut on_line: impl FnMut(&[u8])) -> Result<()> {
    let mut buf = Vec::with_capacity(64 << 10);
    loop {
        buf.clear();
        match read_capped(&mut rd, &mut buf)? {
            None => return Ok(()),
            Some(true) => on_line(&buf),
            Some(false) => {} // over-long: tool output or a base64 signature
        }
    }
}

/// `None` at EOF, `Some(false)` when the line exceeded the cap and was discarded.
fn read_capped(rd: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    let mut started = false;
    let mut capped = false;
    loop {
        let chunk = rd.fill_buf()?;
        if chunk.is_empty() {
            return Ok(started.then_some(!capped));
        }
        started = true;
        let (used, eol) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let room = MAX_LINE_BYTES.saturating_sub(buf.len());
        capped |= room < used;
        buf.extend_from_slice(&chunk[..used.min(room)]);
        rd.consume(used);
        if eol {
            return Ok(Some(!capped));
        }
    }
}

// -- codex's own summaries ----------------------------------------------

fn codex_native_summary(db: &Path, thread_id: &str) -> Option<String> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT summary FROM thread_turn_summaries WHERE thread_id = ?1 ORDER BY updated_at DESC LIMIT 1",
        rusqlite::params![thread_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|s| !s.trim().is_empty())
}

// -- small helpers -------------------------------------------------------

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay.windows(needle.len()).any(|w| w == needle)
}

fn fingerprint(request_id: &str, text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    request_id.hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

fn take_head(s: &str, chars: usize) -> &str {
    match s.char_indices().nth(chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Safety net after redaction, which can grow the text slightly.
fn clamp(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    let head = max / 3;
    let tail_start = total - (max - head);
    let tail = match s.char_indices().nth(tail_start) {
        Some((i, _)) => &s[i..],
        None => "",
    };
    format!("{}\n\n……（中间省略）……\n\n{}", take_head(s, head), tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_fenced_bulleted_reply() {
        let raw = "```markdown\n**给示例服务补上重试与超时**\n- 做了什么：加了指数退避\n- 结果如何：三个用例通过\n- 待处理：超时值还没定\n```";
        let (headline, body) = parse_reply(raw).unwrap();
        assert_eq!(headline, "给示例服务补上重试与超时");
        assert!(body.unwrap().starts_with("- 做了什么"));
    }

    #[test]
    fn headline_only_and_empty_replies() {
        assert_eq!(parse_reply("修好了缓存失效\n").unwrap(), ("修好了缓存失效".into(), None));
        assert!(parse_reply("   \n\n").is_none());
    }

    #[test]
    fn tool_result_blocks_are_not_conversation() {
        let blocks = serde_json::json!([
            { "type": "tool_result", "tool_use_id": "toolu_1", "content": "total 0\ndrwxr-xr-x" }
        ]);
        assert!(text_of(&blocks).is_none());
    }
}

