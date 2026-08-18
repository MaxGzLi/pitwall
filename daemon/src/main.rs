//! agent-monitord — reads what the local AI agents are doing, never writes to them.
//!
//! Three sources, deliberately layered:
//!   herdr socket  -> live state machine + pane->session index (push, no polling)
//!   registries    -> process liveness per harness (cheap poll)
//!   transcripts   -> tokens, turns, and the text the summariser reads (incremental)

mod adapters;
mod api;
mod config;
mod herdr;
mod model;
mod pricing;
mod quota;
mod redact;
mod store;
mod summarize;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::store::Store;

/// Fired whenever something changed, so the strip's SSE stream can wake up.
pub type Events = broadcast::Sender<()>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("agent_monitord=info")),
        )
        .init();

    let cfg = Arc::new(Config::load()?);
    std::fs::create_dir_all(&cfg.data_dir)?;
    let store = Arc::new(Store::open(&cfg.db_path())?);
    info!(db = %cfg.db_path().display(), port = cfg.port, "agent-monitord starting");

    if store.price_count()? == 0 {
        match pricing::load_seed(&cfg, &store) {
            Ok(n) => info!(models = n, "seeded price table from local models.dev snapshot"),
            Err(e) => warn!(error = %e, "no local price seed; costs stay zero until refresh"),
        }
    }

    let (events, _) = broadcast::channel::<()>(64);

    tokio::spawn(herdr::run(cfg.clone(), store.clone(), events.clone()));
    tokio::spawn(collectors(cfg.clone(), store.clone(), events.clone()));

    api::serve(cfg, store, events).await
}

/// Everything that runs on a timer. One task, so the intervals can never overlap
/// themselves and pile up.
async fn collectors(cfg: Arc<Config>, store: Arc<Store>, events: Events) {
    let mut adapters = adapters::all(&cfg);

    let mut live = tokio::time::interval(Duration::from_millis(cfg.live_poll_ms));
    let mut scan = tokio::time::interval(Duration::from_millis(cfg.scan_poll_ms));
    let mut quota_tick = tokio::time::interval(Duration::from_millis(cfg.quota_poll_ms));
    let mut price_tick = tokio::time::interval(Duration::from_secs(6 * 3600));
    live.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = live.tick() => {
                let mut changed = false;
                for adapter in adapters.iter_mut() {
                    match adapter.poll_live(&store) {
                        Ok(n) => changed |= n > 0,
                        Err(e) => warn!(adapter = adapter.name(), error = %e, "live poll failed"),
                    }
                }
                if changed { let _ = events.send(()); }
            }
            _ = scan.tick() => {
                let mut changed = false;
                for adapter in adapters.iter_mut() {
                    match adapter.scan(&store) {
                        Ok(n) => changed |= n > 0,
                        Err(e) => warn!(adapter = adapter.name(), error = %e, "scan failed"),
                    }
                }
                if cfg.summarize {
                    match summarize::run_once(&cfg, &store).await {
                        Ok(n) if n > 0 => { changed = true; info!(count = n, "summarised ended sessions"); }
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "summariser failed"),
                    }
                }
                if changed { let _ = events.send(()); }
            }
            _ = quota_tick.tick() => {
                if let Err(e) = quota::refresh(&cfg, &store).await {
                    warn!(error = %e, "quota refresh failed");
                }
                let _ = events.send(());
            }
            _ = price_tick.tick() => {
                match pricing::refresh(&cfg, &store).await {
                    Ok(n) => info!(models = n, "refreshed price table"),
                    Err(e) => error!(error = %e, "price refresh failed"),
                }
            }
        }
    }
}
