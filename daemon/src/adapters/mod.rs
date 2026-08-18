//! One adapter per harness. Each owns its own on-disk formats and end-detection
//! signal; none of them know about each other.

use anyhow::Result;

use crate::config::Config;
use crate::store::Store;

pub mod claude;
pub mod codex;
pub mod dsh;

pub trait Adapter: Send {
    fn name(&self) -> &'static str;

    /// Cheap, frequent: process registries and liveness. Returns how many
    /// sessions changed, so the caller knows whether to wake the UI.
    fn poll_live(&mut self, store: &Store) -> Result<usize>;

    /// Expensive, infrequent: parse transcripts for tokens, turns and titles.
    /// Must be incremental — these files reach hundreds of megabytes.
    fn scan(&mut self, store: &Store) -> Result<usize>;
}

pub fn all(cfg: &Config) -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude::ClaudeAdapter::new(cfg)),
        Box::new(codex::CodexAdapter::new(cfg)),
        Box::new(dsh::DshAdapter::new(cfg)),
    ]
}
