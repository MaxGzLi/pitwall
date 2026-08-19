//! SQLite store. Single writer (the daemon), WAL so readers never block it.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::model::*;

#[derive(Debug, Clone, Copy, Default)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// A session that ended and has no summary yet.
#[derive(Debug, Clone)]
pub struct PendingSummary {
    pub harness: Harness,
    pub session_id: String,
    pub source_path: Option<String>,
    pub project: Option<String>,
    pub title: Option<String>,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(include_str!("../schema.sql"))
            .context("applying schema")?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- ingest ---------------------------------------------------------

    pub fn apply_updates(&self, updates: &[SessionUpdate]) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let now = now_ms();

        for update in updates {
            let p = &update.patch;
            let harness = update.harness.as_str();
            let project = p.cwd.as_deref().map(project_of);
            let state = p.state.map(|s| s.as_str());

            tx.execute(
                r#"
INSERT INTO session (
  harness, session_id, parent_id, depth, kind, source_path, source_bytes, source_mtime_ms,
  scan_offset, cwd, project, git_branch, provider, model, effort, context_window, title,
  first_user_message, started_at_ms, last_activity_ms, ended_at_ms, state, end_signal,
  end_confidence, pid, proc_start, turns, tool_calls, compactions, parser_version, updated_at_ms
) VALUES (
  :harness, :session_id, :parent_id, COALESCE(:depth, 0), COALESCE(:kind, 'interactive'),
  :source_path, :source_bytes, :source_mtime_ms, COALESCE(:scan_offset, 0), :cwd, :project,
  :git_branch, :provider, :model, :effort, :context_window, :title, :first_user_message,
  COALESCE(:started_at_ms, :now), COALESCE(:last_activity_ms, :now), :ended_at_ms,
  COALESCE(:state, 'unknown'), :end_signal, COALESCE(:end_confidence, 0), :pid, :proc_start,
  COALESCE(:turns, 0), COALESCE(:tool_calls, 0), COALESCE(:compactions, 0), :parser_version, :now
)
ON CONFLICT(harness, session_id) DO UPDATE SET
  parent_id          = COALESCE(:parent_id, session.parent_id),
  depth              = COALESCE(:depth, session.depth),
  kind               = COALESCE(:kind, session.kind),
  source_path        = COALESCE(:source_path, session.source_path),
  source_bytes       = COALESCE(:source_bytes, session.source_bytes),
  source_mtime_ms    = COALESCE(:source_mtime_ms, session.source_mtime_ms),
  scan_offset        = COALESCE(:scan_offset, session.scan_offset),
  cwd                = COALESCE(:cwd, session.cwd),
  project            = COALESCE(:project, session.project),
  git_branch         = COALESCE(:git_branch, session.git_branch),
  provider           = COALESCE(:provider, session.provider),
  model              = COALESCE(:model, session.model),
  effort             = COALESCE(:effort, session.effort),
  context_window     = COALESCE(:context_window, session.context_window),
  title              = COALESCE(:title, session.title),
  first_user_message = COALESCE(session.first_user_message, :first_user_message),
  started_at_ms      = MIN(session.started_at_ms, COALESCE(:started_at_ms, session.started_at_ms)),
  last_activity_ms   = MAX(session.last_activity_ms, COALESCE(:last_activity_ms, session.last_activity_ms)),
  -- a session that reports any live state again has un-ended itself (DSH resumes do this)
  ended_at_ms        = CASE WHEN :state IS NOT NULL AND :state <> 'ended' THEN NULL
                            ELSE COALESCE(session.ended_at_ms, :ended_at_ms) END,
  state              = COALESCE(:state, session.state),
  end_signal         = CASE WHEN :state IS NOT NULL AND :state <> 'ended' THEN NULL
                            ELSE COALESCE(:end_signal, session.end_signal) END,
  end_confidence     = COALESCE(:end_confidence, session.end_confidence),
  pid                = COALESCE(:pid, session.pid),
  proc_start         = COALESCE(:proc_start, session.proc_start),
  turns              = MAX(session.turns, COALESCE(:turns, session.turns)),
  tool_calls         = MAX(session.tool_calls, COALESCE(:tool_calls, session.tool_calls)),
  compactions        = MAX(session.compactions, COALESCE(:compactions, session.compactions)),
  parser_version     = :parser_version,
  updated_at_ms      = :now
"#,
                rusqlite::named_params! {
                    ":harness": harness,
                    ":session_id": update.session_id,
                    ":parent_id": p.parent_id,
                    ":depth": p.depth,
                    ":kind": p.kind,
                    ":source_path": p.source_path,
                    ":source_bytes": p.source_bytes,
                    ":source_mtime_ms": p.source_mtime_ms,
                    ":scan_offset": p.scan_offset,
                    ":cwd": p.cwd,
                    ":project": project,
                    ":git_branch": p.git_branch,
                    ":provider": p.provider,
                    ":model": p.model,
                    ":effort": p.effort,
                    ":context_window": p.context_window,
                    ":title": p.title,
                    ":first_user_message": p.first_user_message,
                    ":started_at_ms": p.started_at_ms,
                    ":last_activity_ms": p.last_activity_ms,
                    ":ended_at_ms": p.ended_at_ms,
                    ":state": state,
                    ":end_signal": p.end_signal,
                    ":end_confidence": p.end_confidence,
                    ":pid": p.pid,
                    ":proc_start": p.proc_start,
                    ":turns": p.turns,
                    ":tool_calls": p.tool_calls,
                    ":compactions": p.compactions,
                    ":parser_version": PARSER_VERSION,
                    ":now": now,
                },
            )?;

            if update.usage.is_empty() {
                continue;
            }

            for delta in &update.usage {
                let price = price_for(&tx, delta.model.as_deref())?;
                // reasoning tokens are a subset of output on every provider we read,
                // so they are reported but never priced twice.
                let cost = (delta.input as f64 * price.input
                    + delta.output as f64 * price.output
                    + delta.cache_read as f64 * price.cache_read
                    + delta.cache_create as f64 * price.cache_write)
                    / 1_000_000.0;

                tx.execute(
                    r#"
INSERT OR IGNORE INTO usage_delta (
  harness, session_id, dedup_key, at_ms, day, model,
  d_input, d_output, d_cache_read, d_cache_create, d_reasoning, cost_usd
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
"#,
                    params![
                        harness,
                        update.session_id,
                        delta.dedup_key,
                        delta.at_ms,
                        day_of(delta.at_ms),
                        delta.model,
                        delta.input,
                        delta.output,
                        delta.cache_read,
                        delta.cache_create,
                        delta.reasoning,
                        cost,
                    ],
                )?;
            }

            // Totals are always derived, never accumulated in place, so a re-scan
            // that re-emits known deltas can never inflate them.
            tx.execute(
                r#"
UPDATE session SET
  tok_input        = COALESCE((SELECT SUM(d_input)        FROM usage_delta u WHERE u.harness = session.harness AND u.session_id = session.session_id), 0),
  tok_output       = COALESCE((SELECT SUM(d_output)       FROM usage_delta u WHERE u.harness = session.harness AND u.session_id = session.session_id), 0),
  tok_cache_read   = COALESCE((SELECT SUM(d_cache_read)   FROM usage_delta u WHERE u.harness = session.harness AND u.session_id = session.session_id), 0),
  tok_cache_create = COALESCE((SELECT SUM(d_cache_create) FROM usage_delta u WHERE u.harness = session.harness AND u.session_id = session.session_id), 0),
  tok_reasoning    = COALESCE((SELECT SUM(d_reasoning)    FROM usage_delta u WHERE u.harness = session.harness AND u.session_id = session.session_id), 0),
  cost_usd         = COALESCE((SELECT SUM(cost_usd)       FROM usage_delta u WHERE u.harness = session.harness AND u.session_id = session.session_id), 0)
WHERE harness = ?1 AND session_id = ?2
"#,
                params![harness, update.session_id],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Marks every live session of `harness` whose id is absent from `alive` as ended.
    /// This is how a vanished process registry becomes an end event.
    pub fn end_missing(
        &self,
        harness: Harness,
        alive: &[String],
        signal: &str,
        confidence: f64,
    ) -> Result<usize> {
        let conn = self.lock();
        let placeholders = if alive.is_empty() {
            "NULL".to_string()
        } else {
            alive.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        };
        let sql = format!(
            r#"
UPDATE session SET state = 'ended', ended_at_ms = COALESCE(ended_at_ms, last_activity_ms),
       end_signal = COALESCE(end_signal, ?), end_confidence = MAX(end_confidence, ?),
       updated_at_ms = ?
WHERE harness = ? AND state <> 'ended' AND session_id NOT IN ({placeholders})
"#
        );
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(signal.to_string()),
            Box::new(confidence),
            Box::new(now_ms()),
            Box::new(harness.as_str().to_string()),
        ];
        for id in alive {
            values.push(Box::new(id.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
        Ok(conn.execute(&sql, refs.as_slice())?)
    }

    // -- herdr ----------------------------------------------------------

    pub fn upsert_panes(&self, panes: &[PaneRow]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for pane in panes {
            tx.execute(
                r#"
INSERT INTO herdr_pane (pane_id, workspace_id, tab_id, agent, agent_status, title, cwd,
                        harness, session_id, focused, seen_at_ms, released)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT(pane_id) DO UPDATE SET
  workspace_id = COALESCE(excluded.workspace_id, herdr_pane.workspace_id),
  tab_id       = COALESCE(excluded.tab_id, herdr_pane.tab_id),
  agent        = COALESCE(excluded.agent, herdr_pane.agent),
  agent_status = COALESCE(excluded.agent_status, herdr_pane.agent_status),
  title        = COALESCE(excluded.title, herdr_pane.title),
  cwd          = COALESCE(excluded.cwd, herdr_pane.cwd),
  harness      = COALESCE(excluded.harness, herdr_pane.harness),
  session_id   = COALESCE(excluded.session_id, herdr_pane.session_id),
  focused      = excluded.focused,
  seen_at_ms   = excluded.seen_at_ms,
  released     = excluded.released
"#,
                params![
                    pane.pane_id,
                    pane.workspace_id,
                    pane.tab_id,
                    pane.agent,
                    pane.agent_status,
                    pane.title,
                    pane.cwd,
                    pane.harness,
                    pane.session_id,
                    pane.focused as i64,
                    pane.seen_at_ms,
                    pane.released as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn drop_pane(&self, pane_id: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM herdr_pane WHERE pane_id = ?1", params![pane_id])?;
        Ok(())
    }

    // -- quota & pricing ------------------------------------------------

    /// Refuses to replace a sample with an older one, and reports which happened.
    ///
    /// Several sources describe the same window at different freshness --
    /// anthropic/5h comes from the claude-hud cache, which is minutes old, and
    /// from ~/.claude.json, which is only rewritten when Claude Code itself runs
    /// and can sit a day behind. The in-memory merge in `quota::refresh` already
    /// prefers the fresher one within a single pass, but it cannot see the last
    /// pass: when the fresher file is momentarily unreadable, the slower source
    /// would otherwise install a day-old number under the same label and the
    /// panel would show the quota jumping backwards.
    pub fn put_quota(&self, q: &QuotaRow) -> Result<bool> {
        let conn = self.lock();
        let written = conn.execute(
            r#"
INSERT INTO quota (provider, window, used_percent, balance, currency, plan, resets_at_ms, sampled_at_ms, source)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(provider, window) DO UPDATE SET
  used_percent = excluded.used_percent, balance = excluded.balance, currency = excluded.currency,
  plan = excluded.plan, resets_at_ms = excluded.resets_at_ms,
  sampled_at_ms = excluded.sampled_at_ms, source = excluded.source
WHERE excluded.sampled_at_ms >= quota.sampled_at_ms
"#,
            params![
                q.provider,
                q.window,
                q.used_percent,
                q.balance,
                q.currency,
                q.plan,
                q.resets_at_ms,
                q.sampled_at_ms,
                q.source
            ],
        )?;
        Ok(written > 0)
    }

    /// Distinct models this machine has logged tokens against in the last
    /// `days`. The price-gap report reads it so the warning names models
    /// somebody here actually runs.
    pub fn models_seen(&self, days: i64) -> Result<Vec<String>> {
        let since = now_ms() - days * 86_400_000;
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT model FROM usage_delta
              WHERE model IS NOT NULL AND model <> '' AND at_ms >= ?1
              ORDER BY model",
        )?;
        let rows = stmt.query_map(params![since], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn put_prices(&self, prices: &[(String, Price)]) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let now = now_ms();
        for (model, p) in prices {
            tx.execute(
                r#"
INSERT INTO price (model, input, output, cache_read, cache_write, reasoning, updated_at_ms)
VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
ON CONFLICT(model) DO UPDATE SET
  input = excluded.input, output = excluded.output, cache_read = excluded.cache_read,
  cache_write = excluded.cache_write, updated_at_ms = excluded.updated_at_ms
"#,
                params![model, p.input, p.output, p.cache_read, p.cache_write, now],
            )?;
        }
        tx.commit()?;
        Ok(prices.len())
    }

    pub fn price_count(&self) -> Result<i64> {
        let conn = self.lock();
        Ok(conn.query_row("SELECT COUNT(*) FROM price", [], |r| r.get(0))?)
    }

    // -- summaries ------------------------------------------------------

    pub fn pending_summaries(&self, limit: i64) -> Result<Vec<PendingSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"
SELECT s.harness, s.session_id, s.source_path, s.project, s.title
FROM session s
LEFT JOIN summary m ON m.harness = s.harness AND m.session_id = s.session_id
WHERE s.state = 'ended' AND s.ended_at_ms IS NOT NULL AND m.session_id IS NULL
  AND s.turns > 0
ORDER BY s.ended_at_ms DESC
LIMIT ?1
"#,
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(PendingSummary {
                    harness: Harness::parse(&r.get::<_, String>(0)?).unwrap_or(Harness::Claude),
                    session_id: r.get(1)?,
                    source_path: r.get(2)?,
                    project: r.get(3)?,
                    title: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn put_summary(
        &self,
        harness: Harness,
        session_id: &str,
        headline: &str,
        body: Option<&str>,
        model: &str,
        input_chars: usize,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"
INSERT INTO summary (harness, session_id, headline, body, model, input_chars, created_at_ms, status, error)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(harness, session_id) DO UPDATE SET
  headline = excluded.headline, body = excluded.body, model = excluded.model,
  input_chars = excluded.input_chars, created_at_ms = excluded.created_at_ms,
  status = excluded.status, error = excluded.error
"#,
            params![
                harness.as_str(),
                session_id,
                headline,
                body,
                model,
                input_chars as i64,
                now_ms(),
                status,
                error
            ],
        )?;
        Ok(())
    }

    /// A page of summaries older than `before_ms`, newest first.
    ///
    /// Keyset, not offset: the snapshot stream keeps writing new summaries while
    /// somebody is reading, and an offset would quietly skip a row every time one
    /// arrived. `before_ms` is a place in the list, and stays that place.
    pub fn summaries_before(
        &self,
        before_ms: i64,
        harness: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SummaryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"
SELECT m.harness, m.session_id, s.project, m.headline, m.body, m.model, m.created_at_ms, m.status
FROM summary m LEFT JOIN session s ON s.harness = m.harness AND s.session_id = m.session_id
WHERE m.status = 'ok' AND m.created_at_ms < ?1 AND (?2 IS NULL OR m.harness = ?2)
ORDER BY m.created_at_ms DESC LIMIT ?3
"#,
        )?;
        let rows = stmt
            .query_map(params![before_ms, harness, limit], |r| {
                Ok(SummaryRow {
                    harness: r.get(0)?,
                    session_id: r.get(1)?,
                    project: r.get(2)?,
                    headline: r.get(3)?,
                    body: r.get(4)?,
                    model: r.get(5)?,
                    created_at_ms: r.get(6)?,
                    status: r.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Spend per clock hour over a window, for the activity curve.
    ///
    /// Only hours that actually saw usage come back. A machine that was asleep
    /// leaves no rows, and a gap is exactly what the curve should show, so the
    /// panel fills the missing hours with zero rather than the query inventing
    /// them here.
    pub fn hourly_usage(&self, from_ms: i64, to_ms: i64) -> Result<Vec<HourUsage>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"
SELECT (at_ms / 3600000) * 3600000 AS hour_ms,
       SUM(d_input + d_output + d_cache_read + d_cache_create + d_reasoning) AS tokens,
       SUM(cost_usd) AS cost_usd
FROM usage_delta
WHERE at_ms >= ?1 AND at_ms < ?2
GROUP BY hour_ms
ORDER BY hour_ms
"#,
        )?;
        let rows = stmt
            .query_map(params![from_ms, to_ms], |r| {
                Ok(HourUsage {
                    hour_ms: r.get(0)?,
                    tokens: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    cost_usd: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- the DSH brain --------------------------------------------------

    /// Finished sessions whose summary has not yet been offered to the brain.
    ///
    /// Only roots: a fan-out is one piece of work, and its subagents' summaries
    /// describe steps inside it, not things the user did. `skipped` summaries
    /// are excluded too -- "(内容过少，未总结)" is not a memory.
    pub fn uncaptured_summaries(&self, limit: i64) -> Result<Vec<CaptureCandidate>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"
SELECT m.harness, m.session_id, m.headline, m.body, m.created_at_ms,
       s.project, s.cwd, s.started_at_ms, s.ended_at_ms, s.turns,
       s.tok_input + s.tok_output + s.tok_cache_read + s.tok_cache_create AS tok_total,
       s.cost_usd
FROM summary m
JOIN session s ON s.harness = m.harness AND s.session_id = m.session_id
LEFT JOIN brain_capture b ON b.key = 'pitwall/session/' || m.harness || '/' || m.session_id
WHERE m.status = 'ok' AND COALESCE(s.depth, 0) = 0 AND b.key IS NULL
ORDER BY m.created_at_ms DESC LIMIT ?1
"#,
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(CaptureCandidate {
                    harness: r.get(0)?,
                    session_id: r.get(1)?,
                    headline: r.get(2)?,
                    body: r.get(3)?,
                    created_at_ms: r.get(4)?,
                    project: r.get(5)?,
                    cwd: r.get(6)?,
                    started_at_ms: r.get(7)?,
                    ended_at_ms: r.get(8)?,
                    turns: r.get(9)?,
                    tok_total: r.get(10)?,
                    cost_usd: r.get(11)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records that one key has been dealt with, whatever the vault said about
    /// it. A rejected capture is recorded too: retrying a payload the brain has
    /// already refused just produces the same refusal every fifteen seconds.
    pub fn mark_captured(&self, key: &str, memory_id: Option<&str>, outcome: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO brain_capture (key, memory_id, outcome, at_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET memory_id = excluded.memory_id,
                                            outcome = excluded.outcome, at_ms = excluded.at_ms",
            params![key, memory_id, outcome, now_ms()],
        )?;
        Ok(())
    }

    // -- watermarks -----------------------------------------------------

    pub fn watermark(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row("SELECT value FROM watermark WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_watermark(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            r#"
INSERT INTO watermark (key, value, updated_at_ms) VALUES (?1, ?2, ?3)
ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms
"#,
            params![key, value, now_ms()],
        )?;
        Ok(())
    }

    /// Byte offset already parsed for one transcript, so tailing never re-reads.
    pub fn scan_offset(&self, harness: Harness, session_id: &str) -> Result<i64> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT scan_offset FROM session WHERE harness = ?1 AND session_id = ?2",
                params![harness.as_str(), session_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    // -- read model -----------------------------------------------------

    pub fn snapshot(&self) -> Result<Snapshot> {
        let conn = self.lock();
        let now = now_ms();
        let today = day_of(now);

        // One row per *top-level* session, carrying its whole subtree.
        //
        // A fan-out is one agent's work, not twenty agents. Every harness here
        // spawns children -- Claude's Task tool, Codex's subagents, DSH's
        // delegation -- and a workflow can have a dozen running at once, all
        // freshly active, which is exactly the order this list sorts by. Listed
        // individually they take every visible slot and bury the session the
        // human is actually sitting in front of.
        //
        // So the children fold into their root, and the root's numbers become the
        // subtree's: they hold more than half the tokens ever recorded on this
        // machine, and a parent showing only its own usage while its children
        // spend ten times that is worse than not showing a number at all. Same
        // for activity -- a parent blocked on eleven children writes nothing to
        // its own transcript, and would age out of a list it is the reason for.
        //
        // `kids` is what is left of them: how many descendants are still live,
        // so a fan-out reads as one busy row instead of vanishing.
        let mut stmt = conn.prepare(
            r#"
WITH RECURSIVE tree(harness, root, id) AS (
  SELECT harness, session_id, session_id FROM session WHERE COALESCE(depth, 0) = 0
  UNION
  SELECT t.harness, t.root, c.session_id
    FROM session c JOIN tree t ON c.harness = t.harness AND c.parent_id = t.id
),
subtree AS (
  SELECT t.harness, t.root,
         SUM(c.tok_input + c.tok_output + c.tok_cache_read + c.tok_cache_create) AS tok_total,
         SUM(c.cost_usd) AS cost_usd,
         MAX(c.last_activity_ms) AS last_activity_ms,
         SUM(c.session_id <> t.root AND c.state <> 'ended') AS kids
    FROM tree t JOIN session c ON c.harness = t.harness AND c.session_id = t.id
   GROUP BY t.harness, t.root
)
SELECT s.harness, s.session_id, s.project, s.cwd, s.git_branch, s.title, s.model, s.state,
       s.started_at_ms, r.last_activity_ms, s.ended_at_ms,
       r.tok_total, r.cost_usd, s.turns, p.pane_id, p.agent_status, r.kids
FROM session s
JOIN subtree r ON r.harness = s.harness AND r.root = s.session_id
LEFT JOIN herdr_pane p ON p.harness = s.harness AND p.session_id = s.session_id AND p.released = 0
WHERE s.state <> 'ended' AND r.last_activity_ms > ?1
ORDER BY
  CASE s.state WHEN 'blocked' THEN 0 WHEN 'working' THEN 1 WHEN 'waiting' THEN 2
               WHEN 'done' THEN 3 ELSE 4 END,
  r.last_activity_ms DESC
LIMIT 40
"#,
        )?;
        let agents = stmt
            .query_map(params![now - 24 * 3600 * 1000], |r| {
                Ok(AgentRow {
                    harness: r.get(0)?,
                    session_id: r.get(1)?,
                    project: r.get(2)?,
                    cwd: r.get(3)?,
                    git_branch: r.get(4)?,
                    title: r.get(5)?,
                    model: r.get(6)?,
                    state: r.get(7)?,
                    started_at_ms: r.get(8)?,
                    last_activity_ms: r.get(9)?,
                    ended_at_ms: r.get(10)?,
                    tok_total: r.get(11)?,
                    cost_usd: r.get(12)?,
                    turns: r.get(13)?,
                    pane_id: r.get(14)?,
                    herdr_status: r.get(15)?,
                    kids: r.get(16)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut stmt = conn.prepare(
            "SELECT provider, window, used_percent, balance, currency, plan, resets_at_ms, sampled_at_ms, source
             FROM quota ORDER BY provider, window",
        )?;
        let quota = stmt
            .query_map([], |r| {
                Ok(QuotaRow {
                    provider: r.get(0)?,
                    window: r.get(1)?,
                    used_percent: r.get(2)?,
                    balance: r.get(3)?,
                    currency: r.get(4)?,
                    plan: r.get(5)?,
                    resets_at_ms: r.get(6)?,
                    sampled_at_ms: r.get(7)?,
                    source: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut stmt = conn.prepare(
            r#"
SELECT harness, SUM(d_input), SUM(d_output), SUM(d_cache_read), SUM(d_cache_create),
       SUM(d_reasoning), SUM(cost_usd)
FROM usage_delta WHERE day = ?1 GROUP BY harness ORDER BY harness
"#,
        )?;
        let today_usage = stmt
            .query_map(params![today], |r| {
                Ok(DayUsage {
                    harness: r.get(0)?,
                    tok_input: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    tok_output: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    tok_cache_read: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    tok_cache_create: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    tok_reasoning: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    cost_usd: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut stmt = conn.prepare(
            r#"
SELECT m.harness, m.session_id, s.project, m.headline, m.body, m.model, m.created_at_ms, m.status
FROM summary m LEFT JOIN session s ON s.harness = m.harness AND s.session_id = m.session_id
WHERE m.status = 'ok'
ORDER BY m.created_at_ms DESC LIMIT 24
"#,
        )?;
        let summaries = stmt
            .query_map([], |r| {
                Ok(SummaryRow {
                    harness: r.get(0)?,
                    session_id: r.get(1)?,
                    project: r.get(2)?,
                    headline: r.get(3)?,
                    body: r.get(4)?,
                    model: r.get(5)?,
                    created_at_ms: r.get(6)?,
                    status: r.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Snapshot {
            generated_at_ms: now,
            day: today,
            agents,
            quota,
            today: today_usage,
            summaries,
        })
    }

    /// Recently ended sessions, for the DSH plugin's "读取任务并总结".
    pub fn recent_sessions(&self, limit: i64, since_ms: i64) -> Result<Vec<(AgentRow, Option<SummaryRow>)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            r#"
SELECT s.harness, s.session_id, s.project, s.cwd, s.git_branch, s.title, s.model, s.state,
       s.started_at_ms, s.last_activity_ms, s.ended_at_ms,
       s.tok_input + s.tok_output + s.tok_cache_read + s.tok_cache_create AS tok_total,
       s.cost_usd, s.turns,
       m.headline, m.body, m.model, m.created_at_ms, m.status
FROM session s
LEFT JOIN summary m ON m.harness = s.harness AND m.session_id = s.session_id
WHERE s.last_activity_ms > ?2 AND s.turns > 0
ORDER BY s.last_activity_ms DESC LIMIT ?1
"#,
        )?;
        let rows = stmt
            .query_map(params![limit, since_ms], |r| {
                let agent = AgentRow {
                    harness: r.get(0)?,
                    session_id: r.get(1)?,
                    project: r.get(2)?,
                    cwd: r.get(3)?,
                    git_branch: r.get(4)?,
                    title: r.get(5)?,
                    model: r.get(6)?,
                    state: r.get(7)?,
                    started_at_ms: r.get(8)?,
                    last_activity_ms: r.get(9)?,
                    ended_at_ms: r.get(10)?,
                    tok_total: r.get(11)?,
                    cost_usd: r.get(12)?,
                    turns: r.get(13)?,
                    pane_id: None,
                    herdr_status: None,
                    // This list is per session, not per subtree: a subagent that
                    // ended gets its own summary, so nothing is being rolled up.
                    kids: 0,
                };
                let headline: Option<String> = r.get(14)?;
                let summary = headline.map(|h| {
                    Ok::<_, rusqlite::Error>(SummaryRow {
                        harness: agent.harness.clone(),
                        session_id: agent.session_id.clone(),
                        project: agent.project.clone(),
                        headline: h,
                        body: r.get(15)?,
                        model: r.get::<_, Option<String>>(16)?.unwrap_or_default(),
                        created_at_ms: r.get::<_, Option<i64>>(17)?.unwrap_or(0),
                        status: r.get::<_, Option<String>>(18)?.unwrap_or_default(),
                    })
                });
                let summary = match summary {
                    Some(Ok(s)) => Some(s),
                    Some(Err(e)) => return Err(e),
                    None => None,
                };
                Ok((agent, summary))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn summary_of(&self, harness: Harness, session_id: &str) -> Result<Option<SummaryRow>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                r#"
SELECT m.harness, m.session_id, s.project, m.headline, m.body, m.model, m.created_at_ms, m.status
FROM summary m LEFT JOIN session s ON s.harness = m.harness AND s.session_id = m.session_id
WHERE m.harness = ?1 AND m.session_id = ?2
"#,
                params![harness.as_str(), session_id],
                |r| {
                    Ok(SummaryRow {
                        harness: r.get(0)?,
                        session_id: r.get(1)?,
                        project: r.get(2)?,
                        headline: r.get(3)?,
                        body: r.get(4)?,
                        model: r.get(5)?,
                        created_at_ms: r.get(6)?,
                        status: r.get(7)?,
                    })
                },
            )
            .optional()?)
    }
}

/// Looks up a price, tolerating the decorations harnesses add to model ids
/// (`claude-opus-5[1m]`, `openai/gpt-5.6-sol`).
fn price_for(conn: &Connection, model: Option<&str>) -> Result<Price> {
    let Some(model) = model else {
        return Ok(Price::default());
    };
    let mut candidates = vec![model.to_string()];
    if let Some(base) = model.split('[').next() {
        candidates.push(base.to_string());
    }
    if let Some((_, tail)) = model.rsplit_once('/') {
        candidates.push(tail.to_string());
        if let Some(base) = tail.split('[').next() {
            candidates.push(base.to_string());
        }
    }
    for candidate in candidates {
        let found = conn
            .query_row(
                "SELECT input, output, cache_read, cache_write FROM price WHERE model = ?1",
                params![candidate],
                |r| {
                    Ok(Price {
                        input: r.get(0)?,
                        output: r.get(1)?,
                        cache_read: r.get(2)?,
                        cache_write: r.get(3)?,
                    })
                },
            )
            .optional()?;
        if let Some(price) = found {
            return Ok(price);
        }
    }
    Ok(Price::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source: &str, percent: f64, sampled_at_ms: i64) -> QuotaRow {
        QuotaRow {
            provider: "anthropic".into(),
            window: "7d".into(),
            used_percent: Some(percent),
            balance: None,
            currency: None,
            plan: None,
            resets_at_ms: None,
            sampled_at_ms,
            source: source.into(),
        }
    }

    fn stored(store: &Store) -> (f64, String) {
        let conn = store.lock();
        conn.query_row(
            "SELECT used_percent, source FROM quota WHERE provider = 'anthropic' AND window = '7d'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    /// The claude-hud cache going quiet for one pass must not let the day-old
    /// copy of the same window in ~/.claude.json take the row over.
    #[test]
    fn older_sample_never_replaces_newer() {
        let path = std::env::temp_dir().join(format!("agent-monitor-quota-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();

        assert!(store.put_quota(&row("hud", 92.0, 1_000_000)).unwrap());
        assert!(!store.put_quota(&row("dotjson", 64.0, 900_000)).unwrap());
        assert_eq!(stored(&store), (92.0, "hud".to_string()));

        // A genuinely newer sample still gets through, including one that moves
        // the percentage down -- windows do reset.
        assert!(store.put_quota(&row("hud", 3.0, 1_100_000)).unwrap());
        assert_eq!(stored(&store), (3.0, "hud".to_string()));

        // Same sample re-offered by the same source is a no-op, not a rejection.
        assert!(store.put_quota(&row("hud", 3.0, 1_100_000)).unwrap());

        let _ = std::fs::remove_file(&path);
    }

    fn session(id: &str, parent: Option<&str>, depth: i64, state: State, tok: i64) -> SessionUpdate {
        let mut u = SessionUpdate::new(Harness::Claude, id);
        u.patch.parent_id = parent.map(str::to_string);
        u.patch.depth = Some(depth);
        u.patch.kind = Some(if depth == 0 { "interactive" } else { "subagent" }.into());
        u.patch.state = Some(state);
        u.patch.last_activity_ms = Some(now_ms());
        u.usage = vec![UsageDelta {
            dedup_key: format!("{id}#1"),
            at_ms: now_ms(),
            model: Some("claude-opus-5".into()),
            input: tok,
            output: 0,
            cache_read: 0,
            cache_create: 0,
            reasoning: 0,
        }];
        u
    }

    /// A workflow fanning out is one row, not a dozen -- and that row has to
    /// carry the tokens its children spent, or hiding them hides the spend.
    #[test]
    fn a_fan_out_is_one_row_carrying_its_whole_subtree() {
        let path = std::env::temp_dir().join(format!("agent-monitor-tree-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();

        store
            .apply_updates(&[
                session("root", None, 0, State::Working, 100),
                session("kid-a", Some("root"), 1, State::Working, 10),
                session("kid-b", Some("root"), 1, State::Working, 20),
                // Ended children still count their tokens; they just are not live.
                session("kid-c", Some("root"), 1, State::Ended, 30),
                // Depth is not always 1: a subagent may spawn its own.
                session("grandkid", Some("kid-a"), 2, State::Working, 40),
                // An unrelated session must stay its own row.
                session("solo", None, 0, State::Working, 7),
            ])
            .unwrap();

        let agents = store.snapshot().unwrap().agents;
        let ids: Vec<&str> = agents.iter().map(|a| a.session_id.as_str()).collect();
        assert_eq!(ids.len(), 2, "only roots are listed, got {ids:?}");

        let root = agents.iter().find(|a| a.session_id == "root").unwrap();
        assert_eq!(root.tok_total, 200, "root carries 100 + 10 + 20 + 30 + 40");
        assert_eq!(root.kids, 3, "kid-a, kid-b, grandkid are live; kid-c ended");

        let solo = agents.iter().find(|a| a.session_id == "solo").unwrap();
        assert_eq!(solo.tok_total, 7);
        assert_eq!(solo.kids, 0, "a session running alone says so");

        let _ = std::fs::remove_file(&path);
    }
}
