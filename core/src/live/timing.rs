//! Opt-in stage timings for diagnosing a slow preview.
//!
//! A stage reports how long it took and how many service calls it made.
//! Durations and counts only: no shift, helper, account or service content
//! ever reaches this output. Nothing is printed unless `TEAMUP_TIMINGS` is
//! set, so an ordinary run is unchanged.

use std::{sync::OnceLock, time::Instant};

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("TEAMUP_TIMINGS").is_some_and(|value| value != "0"))
}

/// One timed read stage. Start it before the work and finish it with the
/// number of service calls the stage made, so a slow stage is separable from
/// a merely chatty one.
pub(crate) struct Stage {
    name: &'static str,
    started: Instant,
}

impl Stage {
    pub(crate) fn start(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
        }
    }
    pub(crate) fn done(self, calls: usize) {
        if enabled() {
            eprintln!(
                "timing {:<18} {:>7} ms  {calls:>3} calls",
                self.name,
                self.started.elapsed().as_millis()
            );
        }
    }
}
