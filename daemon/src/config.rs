//! Every path and knob in one place.
//!
//! Three layers, highest first: an environment variable, then
//! `~/.agent-monitor/config.toml`, then a default derived from `$HOME`. The file
//! exists because a user who is not the author needs somewhere to put settings
//! that is not their shell profile -- `open`ing the panel app does not carry
//! environment variables, so env-only configuration is unreachable for the one
//! way most people will launch this.
//!
//! Nothing here is hardcoded to one machine. Every default follows the
//! convention of the tool it points at, and every one of them can be wrong
//! without the daemon failing: absent sources are absent, not broken.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    /// Where our own SQLite store, cache and config live.
    pub data_dir: PathBuf,
    /// The config file that was read, if there was one. Recorded so the daemon
    /// can say which file it obeyed rather than leaving the user to guess.
    pub config_file: Option<PathBuf>,
    /// Why the config file was ignored, if it was. Reported, never silent.
    pub config_error: Option<String>,
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
    /// A file whose whole contents are the DeepSeek key. The portable place to
    /// put one: the other sources are two other tools' credential stores, which
    /// only exist if you happen to run those tools.
    pub deepseek_key_file: Option<PathBuf>,

    // --- where the harnesses keep their data ---------------------------
    pub claude_home: PathBuf,
    pub codex_home: PathBuf,
    pub dsh_home: PathBuf,
    pub herdr_socket: PathBuf,

    /// The static UI, resolved at startup against the running executable rather
    /// than against wherever the source tree happened to sit at build time.
    pub web_dir: PathBuf,
    /// Optional models.dev snapshot to price from before the first network
    /// refresh succeeds. There is no default: the network is the normal path and
    /// the daemon's own cache covers every restart after the first.
    pub pricing_seed: Option<PathBuf>,

    // --- handing finished work to the DSH brain ------------------------
    /// Send each finished session's summary to the DSH brain's local capture
    /// API. On by default, but inert without a capture token: a machine that
    /// does not run the brain never notices this exists.
    pub brain_capture: bool,
    /// Where that API listens. Loopback; the brain's own default port.
    pub brain_url: String,
    /// File holding `DSH_BRAIN_HTTP_CAPTURE_TOKEN`. This is the one credential
    /// file the daemon opens, and only to authenticate to a server on this
    /// machine. The value is never logged and never leaves loopback.
    pub brain_token_file: PathBuf,
}

/// The on-disk form. Every field is optional -- a config file is a set of
/// overrides, not a complete description of the daemon.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    port: Option<u16>,
    live_poll_ms: Option<u64>,
    scan_poll_ms: Option<u64>,
    quota_poll_ms: Option<u64>,
    stale_after_ms: Option<i64>,
    summarize: Option<bool>,
    summary_model: Option<String>,
    summary_base_url: Option<String>,
    summary_max_chars: Option<usize>,
    deepseek_balance: Option<bool>,
    deepseek_key_file: Option<String>,
    claude_home: Option<String>,
    codex_home: Option<String>,
    dsh_home: Option<String>,
    herdr_socket: Option<String>,
    web_dir: Option<String>,
    pricing_seed: Option<String>,
    brain_capture: Option<bool>,
    brain_url: Option<String>,
    brain_token_file: Option<String>,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let home = dirs_home()?;
        let data_dir = std::env::var_os("AGENT_MONITOR_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".agent-monitor"));

        let config_path = data_dir.join("config.toml");
        let (file, config_file, config_error) = match std::fs::read_to_string(&config_path) {
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Ok(parsed) => (parsed, Some(config_path.clone()), None),
                // A typo must not take the panel down with it: the defaults are
                // all workable, so run on them and say loudly what was ignored.
                Err(e) => (
                    FileConfig::default(),
                    Some(config_path.clone()),
                    Some(format!("{}: {}", config_path.display(), first_line(&e.to_string()))),
                ),
            },
            Err(_) => (FileConfig::default(), None, None),
        };

        let path_of = |env: &str, from_file: &Option<String>, default: PathBuf| -> PathBuf {
            std::env::var_os(env)
                .map(PathBuf::from)
                .or_else(|| from_file.as_deref().map(|raw| expand(&home, raw)))
                .unwrap_or(default)
        };

        let claude_home = path_of("CLAUDE_CONFIG_DIR", &file.claude_home, home.join(".claude"));
        let codex_home = path_of("CODEX_HOME", &file.codex_home, home.join(".codex"));
        let dsh_home = path_of("DSH_HOME", &file.dsh_home, home.join(".dsh"));
        let herdr_socket = path_of(
            "HERDR_SOCKET_PATH",
            &file.herdr_socket,
            home.join(".config/herdr/herdr.sock"),
        );

        Ok(Self {
            port: env_num("AGENT_MONITOR_PORT", file.port.unwrap_or(39917)),
            live_poll_ms: env_num("AGENT_MONITOR_LIVE_POLL_MS", file.live_poll_ms.unwrap_or(2_000)),
            scan_poll_ms: env_num("AGENT_MONITOR_SCAN_POLL_MS", file.scan_poll_ms.unwrap_or(15_000)),
            quota_poll_ms: env_num("AGENT_MONITOR_QUOTA_POLL_MS", file.quota_poll_ms.unwrap_or(60_000)),
            stale_after_ms: env_num("AGENT_MONITOR_STALE_AFTER_MS", file.stale_after_ms.unwrap_or(120_000)),
            summarize: env_bool("AGENT_MONITOR_SUMMARIZE", file.summarize.unwrap_or(true)),
            summary_model: env_str(
                "AGENT_MONITOR_SUMMARY_MODEL",
                file.summary_model.as_deref().unwrap_or("deepseek-v4-flash"),
            ),
            summary_base_url: env_str(
                "AGENT_MONITOR_SUMMARY_BASE_URL",
                file.summary_base_url.as_deref().unwrap_or("https://api.deepseek.com"),
            ),
            summary_max_chars: env_num(
                "AGENT_MONITOR_SUMMARY_MAX_CHARS",
                file.summary_max_chars.unwrap_or(24_000),
            ),
            deepseek_balance: env_bool(
                "AGENT_MONITOR_DEEPSEEK_BALANCE",
                file.deepseek_balance.unwrap_or(true),
            ),
            deepseek_key_file: std::env::var_os("AGENT_MONITOR_DEEPSEEK_KEY_FILE")
                .map(PathBuf::from)
                .or_else(|| file.deepseek_key_file.as_deref().map(|raw| expand(&home, raw))),
            web_dir: resolve_web_dir(&home, &file.web_dir),
            pricing_seed: std::env::var_os("AGENT_MONITOR_PRICING_SEED")
                .map(PathBuf::from)
                .or_else(|| file.pricing_seed.as_deref().map(|raw| expand(&home, raw))),
            brain_capture: env_bool("AGENT_MONITOR_BRAIN_CAPTURE", file.brain_capture.unwrap_or(true)),
            brain_url: env_str(
                "AGENT_MONITOR_BRAIN_URL",
                file.brain_url.as_deref().unwrap_or("http://127.0.0.1:43128"),
            ),
            brain_token_file: path_of(
                "AGENT_MONITOR_BRAIN_TOKEN_FILE",
                &file.brain_token_file,
                dsh_home.join("brain-http.env"),
            ),
            claude_home,
            codex_home,
            dsh_home,
            herdr_socket,
            home,
            data_dir,
            config_file,
            config_error,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("monitor.sqlite")
    }
    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }

    // --- Claude Code ---------------------------------------------------

    /// One JSON file per live process; the file disappears when Claude Code exits.
    pub fn claude_registry_dir(&self) -> PathBuf {
        self.claude_home.join("sessions")
    }
    pub fn claude_projects_dir(&self) -> PathBuf {
        self.claude_home.join("projects")
    }
    /// Written by the claude-hud statusline plugin every 5 minutes.
    pub fn claude_hud_cache(&self) -> PathBuf {
        self.claude_home.join("plugins/claude-hud/.usage-cache.json")
    }
    pub fn claude_dotjson(&self) -> PathBuf {
        self.home.join(".claude.json")
    }

    // --- Codex ---------------------------------------------------------

    /// The thread index. Authoritative and far cheaper than the 13GB of rollouts.
    pub fn codex_state_db(&self) -> PathBuf {
        self.codex_home.join("state_5.sqlite")
    }
    pub fn codex_summaries_db(&self) -> PathBuf {
        self.codex_home.join("sqlite/codex-thread-summaries-dev.db")
    }

    // --- DSH -----------------------------------------------------------

    pub fn dsh_sessions_dir(&self) -> PathBuf {
        self.dsh_home.join("sessions")
    }
    pub fn dsh_credentials(&self) -> PathBuf {
        self.dsh_home.join(".credentials.yaml")
    }

    // --- pricing -------------------------------------------------------

    pub fn pricing_cache(&self) -> PathBuf {
        self.data_dir.join("models_dev.json")
    }
    pub fn pricing_url(&self) -> &'static str {
        "https://models.dev/api.json"
    }
}

/// Where the static UI is.
///
/// The old default was the source tree's path baked in at build time, which
/// meant moving or deleting the checkout after building left the panel blank
/// with no explanation. Look beside the running executable first -- that is the
/// only location that travels with a copied binary -- and fall back through the
/// layouts a cargo build and an app bundle produce.
fn resolve_web_dir(home: &Path, from_file: &Option<String>) -> PathBuf {
    if let Some(dir) = std::env::var_os("AGENT_MONITOR_WEB_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(raw) = from_file.as_deref() {
        return expand(home, raw);
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("web")); // beside a copied binary
            candidates.push(dir.join("../Resources/web")); // inside an app bundle
            candidates.push(dir.join("../../../web")); // target/<profile>/ in the repo
        }
    }
    candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../web")));
    // Keep the last candidate when none exist, so the failure names a real path.
    let fallback = candidates.last().cloned().unwrap_or_else(|| PathBuf::from("web"));
    candidates
        .into_iter()
        .find(|dir| dir.join("index.html").is_file())
        .unwrap_or(fallback)
}

/// Written once, on first run, if it is not already there. A config file that
/// does not exist teaches nobody what can be configured.
pub fn write_template_if_absent(cfg: &Config) -> std::io::Result<bool> {
    let path = cfg.config_path();
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, TEMPLATE)?;
    Ok(true)
}

const TEMPLATE: &str = r#"# agent-monitord settings. Every line is optional and shows the default.
# An environment variable of the same name in upper case, prefixed with
# AGENT_MONITOR_, overrides anything here. Changes take effect on restart.
#
# Paths may start with ~/ .

# --- the daemon -------------------------------------------------------
# port           = 39917      # loopback only, never bound to 0.0.0.0
# live_poll_ms   = 2000       # process liveness
# scan_poll_ms   = 15000      # transcript re-scan (expensive)
# quota_poll_ms  = 60000      # rate-limit windows and balances
# stale_after_ms = 120000     # quiet time before a session counts as ended

# --- where your agents keep their data --------------------------------
# Each default follows that tool's own convention. Anything you do not run
# simply stays empty; a missing directory is not an error.
# claude_home  = "~/.claude"
# codex_home   = "~/.codex"
# dsh_home     = "~/.dsh"
# herdr_socket = "~/.config/herdr/herdr.sock"

# --- summaries and balance (both call the DeepSeek API) ---------------
# Turn either off and nothing leaves this machine.
# summarize         = true
# deepseek_balance  = true
# summary_model     = "deepseek-v4-flash"
# summary_base_url  = "https://api.deepseek.com"
# summary_max_chars = 24000
#
# The key is looked for in this order: the DEEPSEEK_API_KEY environment
# variable, this file's deepseek_key_file, then two other tools' credential
# stores if you happen to run them. Point this at a file containing nothing
# but the key, and chmod 600 it.
# deepseek_key_file = "~/.agent-monitor/deepseek.key"

# --- handing finished work to the DSH brain ---------------------------
# When a session ends and its summary is written, the same summary is offered
# to the DSH brain's local capture API, where it lands in the inbox for you to
# keep or discard. Nothing is sent without a capture token, so a machine that
# does not run the brain never notices this setting.
# brain_capture    = true
# brain_url        = "http://127.0.0.1:43128"
# brain_token_file = "~/.dsh/brain-http.env"

# --- rarely needed ----------------------------------------------------
# web_dir      = "..."   # the panel's static files; found automatically
# pricing_seed = "..."   # a models.dev snapshot to price from before the
                         # first network refresh; the cache covers later runs
"#;

fn dirs_home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("HOME is not set"))
}

/// `~/x` against the real home. Anything else is taken literally, including a
/// bare relative path -- if someone writes one, they meant it.
fn expand(home: &Path, raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if raw == "~" => home.to_path_buf(),
        None => PathBuf::from(raw),
    }
}

/// toml's errors carry a multi-line excerpt; the log wants the sentence.
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).trim().to_string()
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

/// The DSH brain's capture-scoped token: environment first, then the file the
/// brain writes it to. Absent means the brain is not running here, which is the
/// normal case on a machine without DSH -- the caller sends nothing at all.
pub fn brain_token(cfg: &Config) -> Option<String> {
    if let Ok(v) = std::env::var("DSH_BRAIN_HTTP_CAPTURE_TOKEN") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    read_secret(&cfg.brain_token_file, "DSH_BRAIN_HTTP_CAPTURE_TOKEN")
}

/// DeepSeek key: environment, then the file named in the config, then the two
/// credential stores that only exist if you run those tools.
pub fn deepseek_key(cfg: &Config) -> Option<String> {
    if let Ok(v) = std::env::var("DEEPSEEK_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(path) = &cfg.deepseek_key_file {
        // A dedicated key file holds the key and nothing else, but accept the
        // KEY=value form too so one file can serve both purposes.
        if let Ok(text) = std::fs::read_to_string(path) {
            let trimmed = text.trim();
            if !trimmed.is_empty() && !trimmed.contains(['\n', '=', ':']) {
                return Some(trimmed.to_string());
            }
        }
        if let Some(v) = read_secret(path, "DEEPSEEK_API_KEY") {
            return Some(v);
        }
    }
    read_secret(&cfg.dsh_credentials(), "DEEPSEEK_API_KEY")
        .or_else(|| read_secret(&cfg.home.join(".hermes/.env"), "DEEPSEEK_API_KEY"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_only_at_the_front() {
        let home = Path::new("/home/u");
        assert_eq!(expand(home, "~/.codex"), Path::new("/home/u/.codex"));
        assert_eq!(expand(home, "~"), Path::new("/home/u"));
        assert_eq!(expand(home, "/etc/x"), Path::new("/etc/x"));
        // A tilde in the middle is part of the name, not a home directory.
        assert_eq!(expand(home, "/tmp/a~b"), Path::new("/tmp/a~b"));
    }

    /// The template has to survive its own parser, or first run hands the user a
    /// file that the next start refuses.
    #[test]
    fn the_written_template_parses() {
        let parsed: FileConfig = toml::from_str(TEMPLATE).expect("template is valid toml");
        // Everything in it is commented out, so it must apply no overrides.
        assert!(parsed.port.is_none() && parsed.claude_home.is_none());
    }

    /// A dedicated key file is the portable place to put a key: the other two
    /// sources are other tools' credential stores. Both shapes have to work --
    /// the bare key, and the KEY=value form, so one file can serve both.
    #[test]
    fn a_key_file_is_read_bare_or_as_an_assignment() {
        let dir = std::env::temp_dir().join(format!("agent-monitor-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The environment wins over every file, by design. Rather than mutate
        // it -- which would leak into every other test in this binary -- skip
        // where it is set and let the field under test have the field.
        if std::env::var_os("DEEPSEEK_API_KEY").is_some() {
            eprintln!("DEEPSEEK_API_KEY is set; skipping the file-resolution test");
            return;
        }
        let mut cfg = Config::load().unwrap();
        cfg.dsh_home = dir.join("no-such-dsh");
        cfg.home = dir.join("no-such-home");

        let bare = dir.join("bare.key");
        std::fs::write(&bare, "sk-EXAMPLE-BARE\n").unwrap();
        cfg.deepseek_key_file = Some(bare);
        assert_eq!(deepseek_key(&cfg).as_deref(), Some("sk-EXAMPLE-BARE"));

        let assigned = dir.join("dotenv");
        std::fs::write(&assigned, "# a comment\nDEEPSEEK_API_KEY=sk-EXAMPLE-ASSIGNED\n").unwrap();
        cfg.deepseek_key_file = Some(assigned);
        assert_eq!(deepseek_key(&cfg).as_deref(), Some("sk-EXAMPLE-ASSIGNED"));

        // A path that is not there is not an error, just no key.
        cfg.deepseek_key_file = Some(dir.join("absent"));
        assert_eq!(deepseek_key(&cfg), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every commented key in the template must be a real field, or the file
    /// documents settings that silently do nothing.
    #[test]
    fn every_documented_key_is_real() {
        for line in TEMPLATE.lines() {
            let Some(body) = line.strip_prefix("# ") else { continue };
            let Some((key, _)) = body.split_once('=') else { continue };
            let key = key.trim();
            if !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                continue;
            }
            let one = format!("{key} = 1");
            let as_str = format!("{key} = \"x\"");
            let bool_form = format!("{key} = true");
            assert!(
                [one, as_str, bool_form]
                    .iter()
                    .any(|t| toml::from_str::<FileConfig>(t).is_ok()),
                "template documents `{key}`, which FileConfig does not accept"
            );
        }
    }
}
