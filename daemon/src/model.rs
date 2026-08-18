//! Shared vocabulary. Every adapter speaks this; the store and API speak nothing else.

use serde::{Deserialize, Serialize};

pub const PARSER_VERSION: &str = "agent-monitor/0.1.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
    Dsh,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Dsh => "dsh",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "dsh" => Some(Harness::Dsh),
            _ => None,
        }
    }
}

impl std::fmt::Display for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle state of one session, unified across harnesses.
///
/// `Done` is herdr's "finished while you weren't looking" — it collapses to `Idle`
/// the moment the human focuses that tab, so it must be recorded promptly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Working,
    Blocked,
    Waiting,
    Idle,
    Done,
    Ended,
    Unknown,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Blocked => "blocked",
            State::Waiting => "waiting",
            State::Idle => "idle",
            State::Done => "done",
            State::Ended => "ended",
            State::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "working" => Some(State::Working),
            "blocked" => Some(State::Blocked),
            "waiting" => Some(State::Waiting),
            "idle" => Some(State::Idle),
            "done" => Some(State::Done),
            "ended" => Some(State::Ended),
            "unknown" => Some(State::Unknown),
            _ => None,
        }
    }

    pub fn is_live(self) -> bool {
        !matches!(self, State::Ended)
    }
}

/// One token-accounting event. `dedup_key` is what stops double counting:
/// claude = `requestId` (assistant records repeat per content block, ~3.7x otherwise),
/// codex  = `<session>#<cumulative_total>` (the counter is cumulative, so store deltas),
/// dsh    = `<session>#<seq>` (already one record per assistant turn).
#[derive(Debug, Clone)]
pub struct UsageDelta {
    pub dedup_key: String,
    pub at_ms: i64,
    pub model: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub reasoning: i64,
}

impl UsageDelta {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_create
    }
}

/// Sparse patch over a session row. `None` means "adapter has nothing new to say",
/// which is different from "set it to null" — the store leaves such columns alone.
#[derive(Debug, Clone, Default)]
pub struct SessionPatch {
    pub parent_id: Option<String>,
    pub depth: Option<i64>,
    pub kind: Option<String>,
    pub source_path: Option<String>,
    pub source_bytes: Option<i64>,
    pub source_mtime_ms: Option<i64>,
    pub scan_offset: Option<i64>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context_window: Option<i64>,
    pub title: Option<String>,
    pub first_user_message: Option<String>,
    pub started_at_ms: Option<i64>,
    pub last_activity_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub state: Option<State>,
    pub end_signal: Option<String>,
    pub end_confidence: Option<f64>,
    pub pid: Option<i64>,
    pub proc_start: Option<String>,
    pub turns: Option<i64>,
    pub tool_calls: Option<i64>,
    pub compactions: Option<i64>,
}

/// What an adapter emits per scan.
#[derive(Debug, Clone)]
pub struct SessionUpdate {
    pub harness: Harness,
    pub session_id: String,
    pub patch: SessionPatch,
    pub usage: Vec<UsageDelta>,
}

impl SessionUpdate {
    pub fn new(harness: Harness, session_id: impl Into<String>) -> Self {
        Self {
            harness,
            session_id: session_id.into(),
            patch: SessionPatch::default(),
            usage: Vec::new(),
        }
    }
}

/// Live herdr pane, joined to a session where herdr knows the native session id.
#[derive(Debug, Clone, Serialize)]
pub struct PaneRow {
    pub pane_id: String,
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub agent: Option<String>,
    pub agent_status: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub harness: Option<String>,
    pub session_id: Option<String>,
    pub focused: bool,
    pub released: bool,
    pub seen_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaRow {
    pub provider: String,
    pub window: String,
    pub used_percent: Option<f64>,
    pub balance: Option<f64>,
    pub currency: Option<String>,
    pub plan: Option<String>,
    pub resets_at_ms: Option<i64>,
    pub sampled_at_ms: i64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRow {
    pub harness: String,
    pub session_id: String,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub state: String,
    pub started_at_ms: i64,
    pub last_activity_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub tok_total: i64,
    pub cost_usd: f64,
    pub turns: i64,
    pub pane_id: Option<String>,
    pub herdr_status: Option<String>,
    /// Live descendants folded into this row. 0 for a session running alone.
    pub kids: i64,
}

/// A finished session, with everything the brain needs to file it: the summary
/// that was written for the panel, plus the measured facts around it.
#[derive(Debug, Clone)]
pub struct CaptureCandidate {
    pub harness: String,
    pub session_id: String,
    pub headline: String,
    pub body: Option<String>,
    pub created_at_ms: i64,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub turns: i64,
    pub tok_total: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryRow {
    pub harness: String,
    pub session_id: String,
    pub project: Option<String>,
    pub headline: String,
    pub body: Option<String>,
    pub model: String,
    pub created_at_ms: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayUsage {
    pub harness: String,
    pub tok_input: i64,
    pub tok_output: i64,
    pub tok_cache_read: i64,
    pub tok_cache_create: i64,
    pub tok_reasoning: i64,
    pub cost_usd: f64,
}

/// The single payload the strip UI and the DSH plugin both render.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub generated_at_ms: i64,
    pub day: String,
    pub agents: Vec<AgentRow>,
    pub quota: Vec<QuotaRow>,
    pub today: Vec<DayUsage>,
    pub summaries: Vec<SummaryRow>,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// RFC 3339 in UTC, which is the only timestamp format the brain's capture
/// schema accepts.
pub fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp_millis(0).expect("epoch is in range"))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Local-time day bucket, because "今日用量" means the user's day, not UTC's.
pub fn day_of(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// Display name for a working directory.
///
/// The leaf alone is often meaningless — an agent working in
/// `/home/u/agent-monitor/daemon/src` would be labelled `src`, which tells
/// the reader nothing about which agent this is. Generic container directories
/// are skipped so the first name that identifies something wins.
pub fn project_of(cwd: &str) -> String {
    const GENERIC: &[&str] = &[
        "src", "lib", "app", "apps", "source", "sources", "packages", "crates", "pkg", "cmd",
        "code", "repo", "repos", "projects", "workspace", "work",
    ];
    let mut segments = cwd.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>();
    while segments.len() > 1 && GENERIC.contains(&segments[segments.len() - 1]) {
        segments.pop();
    }
    segments.last().map(|s| s.to_string()).unwrap_or_else(|| cwd.to_string())
}

#[cfg(test)]
mod project_tests {
    use super::project_of;

    #[test]
    fn generic_leaves_are_skipped() {
        assert_eq!(project_of("/home/u/agent-monitor/daemon/src"), "daemon");
        assert_eq!(project_of("/home/u/my-editor/packages/app"), "my-editor");
        assert_eq!(project_of("/home/u/recipes"), "recipes");
        assert_eq!(project_of("/home/u/Downloads/"), "Downloads");
    }

    #[test]
    fn a_path_that_is_only_generic_keeps_something() {
        assert_eq!(project_of("/src"), "src");
        assert_eq!(project_of(""), "");
    }
}
