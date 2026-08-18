//! Repeated failures reported once.
//!
//! Nearly everything this daemon reads is optional. A machine that does not run
//! Codex has no `~/.codex`, and polling it every two seconds must not produce a
//! line of log every two seconds — on a clean machine the old behaviour was
//! thirty-one identical errors a minute, which buries the one line that matters
//! and reads like something is broken when nothing is.
//!
//! Each probe reports its outcome here after every attempt. Only a *change* is
//! logged: the first failure, and the recovery that follows it.

use std::collections::HashMap;

use tracing::{info, warn};

/// What was read, and which pass was reading it. The two passes over one source
/// are separate probes: a scan that fails while the liveness poll succeeds is a
/// real, different fact, and folding them together would make the pair flip-flop
/// and log on every tick — exactly what this module exists to prevent.
pub type Probe = (&'static str, &'static str);

#[derive(Debug, Default)]
pub struct Notice {
    /// Last reported failure per probe. Absent means the probe was last OK.
    failing: HashMap<Probe, String>,
}

impl Notice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports a probe's outcome. `subject` names what was being read, in words
    /// a reader who has never seen this codebase can act on.
    ///
    /// A failure whose message is unchanged is silent — the same missing file
    /// found again is not news. A *different* failure is logged, because the
    /// reason moving (missing -> unreadable -> malformed) is worth knowing.
    pub fn report(&mut self, probe: Probe, subject: &str, outcome: Result<(), String>) {
        match outcome {
            Err(error) => {
                if self.failing.get(&probe) == Some(&error) {
                    return;
                }
                warn!(
                    source = probe.0,
                    pass = probe.1,
                    error = %error,
                    "{subject} unavailable; staying quiet until this changes"
                );
                self.failing.insert(probe, error);
            }
            Ok(()) => {
                if self.failing.remove(&probe).is_some() {
                    info!(source = probe.0, pass = probe.1, "{subject} is back");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the module: the same failure, over and over, is one line.
    #[test]
    fn only_changes_are_reported() {
        let mut n = Notice::new();
        let probe = ("codex", "live");
        let missing = || Err("no such file".to_string());

        n.report(probe, "Codex", missing());
        assert_eq!(n.failing.len(), 1);
        // Repeats do not replace the entry, so nothing is logged again.
        for _ in 0..100 {
            n.report(probe, "Codex", missing());
        }
        assert_eq!(n.failing.get(&probe).map(String::as_str), Some("no such file"));

        // A different reason is news; recovery clears the slate.
        n.report(probe, "Codex", Err("permission denied".into()));
        assert_eq!(n.failing.get(&probe).map(String::as_str), Some("permission denied"));
        n.report(probe, "Codex", Ok(()));
        assert!(n.failing.is_empty());
    }

    /// Two passes over one source must not shadow each other, or a source whose
    /// scan fails while its liveness poll succeeds logs on every single tick.
    #[test]
    fn the_two_passes_are_separate_probes() {
        let mut n = Notice::new();
        n.report(("codex", "live"), "Codex", Ok(()));
        n.report(("codex", "scan"), "Codex", Err("transcript unreadable".into()));
        assert_eq!(n.failing.len(), 1);

        // The pass that works keeps working; the one that does not stays silent.
        for _ in 0..50 {
            n.report(("codex", "live"), "Codex", Ok(()));
            n.report(("codex", "scan"), "Codex", Err("transcript unreadable".into()));
        }
        assert_eq!(n.failing.len(), 1);
    }
}
