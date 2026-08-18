-- agent-monitor store. One writer (the daemon), many readers.
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 3000;
PRAGMA foreign_keys = ON;

-- One row per agent session across every harness.
CREATE TABLE IF NOT EXISTS session (
  harness            TEXT    NOT NULL,          -- 'claude' | 'codex' | 'dsh'
  session_id         TEXT    NOT NULL,
  parent_id          TEXT,                      -- spawning session, when this is a subagent
  depth              INTEGER NOT NULL DEFAULT 0,
  kind               TEXT    NOT NULL DEFAULT 'interactive', -- interactive|exec|subagent|automation|review
  source_path        TEXT,                      -- absolute transcript path
  source_bytes       INTEGER,
  source_mtime_ms    INTEGER,
  scan_offset        INTEGER NOT NULL DEFAULT 0,-- byte offset already parsed (incremental tail)
  cwd                TEXT,
  project            TEXT,                      -- basename of cwd, for display
  git_branch         TEXT,
  provider           TEXT,
  model              TEXT,
  effort             TEXT,
  context_window     INTEGER,
  title              TEXT,
  first_user_message TEXT,
  started_at_ms      INTEGER NOT NULL,
  last_activity_ms   INTEGER NOT NULL,
  ended_at_ms        INTEGER,
  state              TEXT    NOT NULL,          -- working|blocked|waiting|idle|done|ended|unknown
  end_signal         TEXT,                      -- registry_gone|pid_dead|end_seed|archived|mtime_stale|herdr_released
  end_confidence     REAL    NOT NULL DEFAULT 0,
  pid                INTEGER,
  proc_start         TEXT,                      -- normalised UTC, defeats PID reuse
  turns              INTEGER NOT NULL DEFAULT 0,
  tool_calls         INTEGER NOT NULL DEFAULT 0,
  compactions        INTEGER NOT NULL DEFAULT 0,
  tok_input          INTEGER NOT NULL DEFAULT 0,
  tok_output         INTEGER NOT NULL DEFAULT 0,
  tok_cache_read     INTEGER NOT NULL DEFAULT 0,
  tok_cache_create   INTEGER NOT NULL DEFAULT 0,
  tok_reasoning      INTEGER NOT NULL DEFAULT 0,
  cost_usd           REAL    NOT NULL DEFAULT 0,-- computed locally; never read from disk
  parser_version     TEXT    NOT NULL,
  updated_at_ms      INTEGER NOT NULL,
  PRIMARY KEY (harness, session_id)
) STRICT;

CREATE INDEX IF NOT EXISTS session_live ON session(state, last_activity_ms DESC);
CREATE INDEX IF NOT EXISTS session_day  ON session(harness, started_at_ms);
CREATE INDEX IF NOT EXISTS session_tree ON session(parent_id);

-- Append-only token deltas. Enables exact per-day rollups without re-reading transcripts.
-- dedup_key: claude=requestId | codex='<sid>#<cumulative_total>' | dsh='<sid>#<seq>'
CREATE TABLE IF NOT EXISTS usage_delta (
  harness      TEXT    NOT NULL,
  session_id   TEXT    NOT NULL,
  dedup_key    TEXT    NOT NULL,
  at_ms        INTEGER NOT NULL,
  day          TEXT    NOT NULL,               -- local YYYY-MM-DD
  model        TEXT,
  d_input      INTEGER NOT NULL DEFAULT 0,
  d_output     INTEGER NOT NULL DEFAULT 0,
  d_cache_read INTEGER NOT NULL DEFAULT 0,
  d_cache_create INTEGER NOT NULL DEFAULT 0,
  d_reasoning  INTEGER NOT NULL DEFAULT 0,
  cost_usd     REAL    NOT NULL DEFAULT 0,
  PRIMARY KEY (harness, session_id, dedup_key)
) STRICT;

CREATE INDEX IF NOT EXISTS usage_day ON usage_delta(day, harness, model);

-- Auto-generated session summaries.
CREATE TABLE IF NOT EXISTS summary (
  harness      TEXT    NOT NULL,
  session_id   TEXT    NOT NULL,
  headline     TEXT    NOT NULL,               -- one line, shown in the strip
  body         TEXT,                           -- fuller markdown, shown on click
  model        TEXT    NOT NULL,               -- which model wrote it
  input_chars  INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL,
  status       TEXT    NOT NULL,               -- pending|ok|failed|skipped
  error        TEXT,
  PRIMARY KEY (harness, session_id)
) STRICT;

CREATE INDEX IF NOT EXISTS summary_recent ON summary(created_at_ms DESC);

-- Live herdr topology: which pane hosts which session.
CREATE TABLE IF NOT EXISTS herdr_pane (
  pane_id       TEXT PRIMARY KEY,
  workspace_id  TEXT,
  tab_id        TEXT,
  agent         TEXT,                          -- herdr agent kind: claude|codex|pi|...
  agent_status  TEXT,                          -- idle|working|blocked|done|unknown
  title         TEXT,
  cwd           TEXT,
  harness       TEXT,                          -- resolved harness for the join
  session_id    TEXT,                          -- agent_session.value
  focused       INTEGER NOT NULL DEFAULT 0,
  seen_at_ms    INTEGER NOT NULL,
  released      INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Rate-limit / balance snapshots. The real currency on subscription plans.
CREATE TABLE IF NOT EXISTS quota (
  provider      TEXT NOT NULL,                 -- anthropic|openai|deepseek
  window        TEXT NOT NULL,                 -- 5h|7d|weekly|balance
  used_percent  REAL,
  balance       REAL,
  currency      TEXT,
  plan          TEXT,
  resets_at_ms  INTEGER,
  sampled_at_ms INTEGER NOT NULL,
  source        TEXT NOT NULL,                 -- which file/endpoint it came from
  PRIMARY KEY (provider, window)
) STRICT;

-- USD per 1M tokens, from models.dev.
CREATE TABLE IF NOT EXISTS price (
  model        TEXT PRIMARY KEY,
  input        REAL NOT NULL DEFAULT 0,
  output       REAL NOT NULL DEFAULT 0,
  cache_read   REAL NOT NULL DEFAULT 0,
  cache_write  REAL NOT NULL DEFAULT 0,
  reasoning    REAL NOT NULL DEFAULT 0,
  updated_at_ms INTEGER NOT NULL
) STRICT;

-- Per-adapter incremental scan watermarks.
CREATE TABLE IF NOT EXISTS watermark (
  key      TEXT PRIMARY KEY,
  value    TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL
) STRICT;
