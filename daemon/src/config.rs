//! Every path and knob in one place. All defaults are derived from $HOME so
//! nothing is hardcoded to one machine.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    /// Where our own SQLite store and logs live.
    pub data_dir: PathBuf,
    /// Loopback port for the HTTP + SSE API and the strip UI.
    pub port: u16,
    /// How often to re-read the live registries (cheap, small files).
    pub live_poll_ms: u64,
    /// How often to re-scan transcripts for token accounting (expensive).
    pub scan_poll_ms: u64,
    /// How often to refresh quota/rate-limit snapshots.
    pub quota_poll_ms: u64,
    /// A session must be quiet this long before a mtime-only end signal is trusted.
    pub stale_after_ms: i64,
    /// Summarise sessions when they end.
    pub summarize: bool,
    pub summary_model: String,
    pub summary_base_url: String,
    /// Cap on transcript text handed to the summariser.
    pub summary_max_chars: usize,
    /// Ask DeepSeek for the account balance (the only balance not available locally).
    pub deepseek_balance: bool,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let home = dirs_home()?;
        let data_dir = std::env::var_os("AGENT_MONITOR_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".agent-monitor"));

        Ok(Self {
            home,
            data_dir,
            port: env_num("AGENT_MONITOR_PORT", 39917),
            live_poll_ms: env_num("AGENT_MONITOR_LIVE_POLL_MS", 2_000),
            scan_poll_ms: env_num("AGENT_MONITOR_SCAN_POLL_MS", 15_000),
            quota_poll_ms: env_num("AGENT_MONITOR_QUOTA_POLL_MS", 60_000),
            stale_after_ms: env_num("AGENT_MONITOR_STALE_AFTER_MS", 120_000),
            summarize: env_bool("AGENT_MONITOR_SUMMARIZE", true),
            summary_model: env_str("AGENT_MONITOR_SUMMARY_MODEL", "deepseek-v4-flash"),
            summary_base_url: env_str("AGENT_MONITOR_SUMMARY_BASE_URL", "https://api.deepseek.com"),
            summary_max_chars: env_num("AGENT_MONITOR_SUMMARY_MAX_CHARS", 24_000) as usize,
            deepseek_balance: env_bool("AGENT_MONITOR_DEEPSEEK_BALANCE", true),
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("monitor.sqlite")
    }

    // --- Claude Code ---------------------------------------------------

    pub fn claude_home(&self) -> PathBuf {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(".claude"))
    }
    /// One JSON file per live process; the file disappears when Claude Code exits.
    pub fn claude_registry_dir(&self) -> PathBuf {
        self.claude_home().join("sessions")
    }
    pub fn claude_projects_dir(&self) -> PathBuf {
        self.claude_home().join("projects")
    }
    /// Written by the claude-hud statusline plugin every 5 minutes.
    pub fn claude_hud_cache(&self) -> PathBuf {
        self.claude_home().join("plugins/claude-hud/.usage-cache.json")
    }
    pub fn claude_dotjson(&self) -> PathBuf {
        self.home.join(".claude.json")
    }

    // --- Codex ---------------------------------------------------------

    pub fn codex_home(&self) -> PathBuf {
        std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(".codex"))
    }
    /// The thread index. Authoritative and far cheaper than the 13GB of rollouts.
    pub fn codex_state_db(&self) -> PathBuf {
        self.codex_home().join("state_5.sqlite")
    }
    pub fn codex_summaries_db(&self) -> PathBuf {
        self.codex_home().join("sqlite/codex-thread-summaries-dev.db")
    }

    // --- DSH -----------------------------------------------------------

    pub fn dsh_home(&self) -> PathBuf {
        std::env::var_os("DSH_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(".dsh"))
    }
    pub fn dsh_sessions_dir(&self) -> PathBuf {
        self.dsh_home().join("sessions")
    }
    pub fn dsh_credentials(&self) -> PathBuf {
        self.dsh_home().join(".credentials.yaml")
    }

    // --- herdr ---------------------------------------------------------

    pub fn herdr_socket(&self) -> PathBuf {
        std::env::var_os("HERDR_SOCKET_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.join(".config/herdr/herdr.sock"))
    }

    // --- pricing -------------------------------------------------------

    /// A models.dev snapshot already on this machine, used as the seed so the
    /// daemon can price tokens before its first successful network refresh.
    pub fn pricing_seed(&self) -> PathBuf {
        self.home.join(".hermes/models_dev_cache.json")
    }
    pub fn pricing_cache(&self) -> PathBuf {
        self.data_dir.join("models_dev.json")
    }
    pub fn pricing_url(&self) -> &'static str {
        "https://models.dev/api.json"
    }
}

fn dirs_home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key).ok().as_deref() {
        Some("1") | Some("true") | Some("yes") => true,
        Some("0") | Some("false") | Some("no") => false,
        _ => default,
    }
}

/// Reads `KEY: value` / `KEY=value` out of a credentials file without pulling in a
/// YAML dependency. Only the named key is returned; nothing is logged.
pub fn read_secret(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':').or_else(|| line.split_once('=')) else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// DeepSeek key: environment first, then the DSH credentials file, then hermes.
pub fn deepseek_key(cfg: &Config) -> Option<String> {
    if let Ok(v) = std::env::var("DEEPSEEK_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    read_secret(&cfg.dsh_credentials(), "DEEPSEEK_API_KEY")
        .or_else(|| read_secret(&cfg.home.join(".hermes/.env"), "DEEPSEEK_API_KEY"))
}
