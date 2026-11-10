//! The archive mark of a session and how its date reads.
//!
//! An archived session keeps everything it had, its status included; the mark only records
//! when the user archived it. Dates show as the day of the month and the English month
//! abbreviation, such as `3 Nov`, in UTC.

use serde::{Deserialize, Serialize};

/// The seconds in one day.
const DAY: u64 = 86_400;

/// The English month abbreviations, January first.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// When a session was archived; a session without it is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archived {
    /// When the user archived the session, in seconds since the Unix epoch.
    pub at: u64,
}

/// The UTC calendar day of `seconds` since the Unix epoch, as the day of the month and the
/// English month abbreviation.
///
/// # Examples
///
/// ```
/// use partitur_core::day_month;
///
/// assert_eq!(day_month(1_699_000_000), "3 Nov");
/// ```
#[must_use]
pub fn day_month(seconds: u64) -> String {
    let (month, day) = month_day(seconds / DAY);
    let name = usize::try_from(month - 1)
        .ok()
        .and_then(|index| MONTHS.get(index))
        .copied()
        .unwrap_or_default();
    format!("{day} {name}")
}

/// The month (1 to 12) and the day of the month (1 to 31) of the day `days` after
/// 1 January 1970, by Howard Hinnant's `civil_from_days`, which counts in 400-year eras
/// starting on 1 March so that a leap day falls at the end of its year.
fn month_day(days: u64) -> (u64, u64) {
    let shifted = days + 719_468;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    (month, day)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;

    #[test]
    fn day_month_names_the_utc_day_and_month() {
        assert_eq!(day_month(0), "1 Jan");
        assert_eq!(day_month(1_699_000_000), "3 Nov");
        assert_eq!(day_month(951_782_400), "29 Feb");
        assert_eq!(day_month(1_698_451_199), "27 Oct");
        assert_eq!(day_month(1_698_451_200), "28 Oct");
        assert_eq!(day_month(1_735_689_599), "31 Dec");
    }

    #[test]
    fn day_month_follows_the_leap_years_of_the_gregorian_calendar() {
        assert_eq!(day_month(951_739_200), "28 Feb");
        assert_eq!(day_month(951_912_000), "1 Mar");
        assert_eq!(day_month(3_981_355_200), "29 Feb");
        assert_eq!(day_month(4_107_499_200), "28 Feb");
        assert_eq!(day_month(4_107_585_600), "1 Mar");
        assert_eq!(day_month(7_263_259_200), "1 Mar");
        assert_eq!(day_month(13_569_422_400), "31 Dec");
        assert_eq!(day_month(13_574_606_400), "29 Feb");
    }

    #[test]
    fn an_archive_mark_round_trips_through_json() {
        let mark = Archived { at: 42 };

        let json = serde_json::to_string(&mark).unwrap();

        assert_eq!(serde_json::from_str::<Archived>(&json).unwrap(), mark);
    }
}
