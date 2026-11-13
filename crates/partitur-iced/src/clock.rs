//! Telling a moment as the clock on the wall shows it: `HH:MM` in the user's time zone.
//!
//! The core keeps times as seconds since the Unix epoch and never formats them; the interface
//! turns them into the local clock time here. The local zone is the system's, and UTC when
//! the system names none Partitur can read. A moment past the latest one the calendar
//! library can represent reads `--:--` instead of a wrong time.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// What a moment that cannot be told reads.
const UNKNOWN: &str = "--:--";

/// The moment `seconds` after the Unix epoch as `HH:MM` in `zone`.
pub(crate) fn clock_time_in(seconds: u64, zone: &TimeZone) -> String {
    let Some(moment) = i64::try_from(seconds)
        .ok()
        .and_then(|seconds| Timestamp::from_second(seconds).ok())
    else {
        return UNKNOWN.to_owned();
    };
    moment.to_zoned(zone.clone()).strftime("%H:%M").to_string()
}

/// The moment `seconds` after the Unix epoch as `HH:MM` in the user's local time zone.
pub(crate) fn local_time(seconds: u64) -> String {
    clock_time_in(seconds, &TimeZone::system())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use jiff::tz::{self, TimeZone};

    use super::*;

    #[test]
    fn a_moment_reads_as_hours_and_minutes_in_the_zone() {
        assert_eq!(clock_time_in(50_520, &TimeZone::UTC), "14:02");
        assert_eq!(clock_time_in(0, &TimeZone::UTC), "00:00");
        assert_eq!(
            clock_time_in(50_520, &TimeZone::fixed(tz::offset(1))),
            "15:02"
        );
    }

    #[test]
    fn a_moment_out_of_range_reads_as_unknown() {
        assert_eq!(clock_time_in(u64::MAX, &TimeZone::UTC), "--:--");
        assert_eq!(clock_time_in(1 << 40, &TimeZone::UTC), "--:--");
    }

    #[test]
    fn the_local_time_is_the_system_zones() {
        assert_eq!(
            local_time(50_520),
            clock_time_in(50_520, &TimeZone::system())
        );
    }
}
