//! The short label for how long ago something happened, as the project list shows it.

/// How long ago `then` was as seen at `now`, both in seconds since the Unix epoch: `now`
/// within the first minute, then `N min`, `N h`, `yesterday` and `N d`. A `then` after `now`
/// reads `now`.
#[must_use]
pub fn age_label(now: u64, then: u64) -> String {
    let elapsed = now.saturating_sub(then);
    if elapsed < 60 {
        "now".into()
    } else if elapsed < 3600 {
        format!("{} min", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{} h", elapsed / 3600)
    } else if elapsed < 172_800 {
        "yesterday".into()
    } else {
        format!("{} d", elapsed / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::age_label;

    #[test]
    fn the_label_steps_through_now_minutes_hours_yesterday_and_days() {
        let cases = [
            (0, "now"),
            (59, "now"),
            (60, "1 min"),
            (3599, "59 min"),
            (3600, "1 h"),
            (86_399, "23 h"),
            (86_400, "yesterday"),
            (172_799, "yesterday"),
            (172_800, "2 d"),
        ];
        for (elapsed, label) in cases {
            assert_eq!(
                age_label(1_000_000, 1_000_000 - elapsed),
                label,
                "{elapsed}"
            );
        }
    }

    #[test]
    fn a_time_in_the_future_reads_now() {
        assert_eq!(age_label(10, 500), "now");
    }
}
