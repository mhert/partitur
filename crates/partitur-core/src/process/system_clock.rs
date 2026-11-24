//! The [`Clock`] that reads the system's time.
//!
//! [`SystemClock`] is the one place in the core that asks the operating system for the time;
//! everything else takes it from a [`Clock`].

use std::time::{SystemTime, UNIX_EPOCH};

use crate::clock::Clock;

/// The clock of the running system. A system time before the Unix epoch reads as 0.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_reads_a_time_after_2020() {
        assert!(SystemClock.now() > 1_577_836_800);
    }
}
