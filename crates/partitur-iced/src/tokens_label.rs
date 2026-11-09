//! Token counts as the interface writes them: the plain number below a thousand, whole
//! thousands with a `k` from there, and a context usage as `41k / 200k`.

/// `count` tokens written compactly: `850` stays as it is, from 1000 up the count is
/// rounded to the nearest thousand and written as `41k`.
pub fn compact(count: u64) -> String {
    if count < 1000 {
        count.to_string()
    } else {
        format!("{}k", count.saturating_add(500) / 1000)
    }
}

/// A context usage of `used` out of `size` tokens, each written compactly: `41k / 200k`.
pub fn usage(used: u64, size: u64) -> String {
    format!("{} / {}", compact(used), compact(size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_below_a_thousand_are_written_in_full() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(850), "850");
        assert_eq!(compact(999), "999");
    }

    #[test]
    fn counts_from_a_thousand_up_are_rounded_to_the_nearest_thousand() {
        assert_eq!(compact(1000), "1k");
        assert_eq!(compact(1499), "1k");
        assert_eq!(compact(1500), "2k");
        assert_eq!(compact(41_260), "41k");
        assert_eq!(compact(200_000), "200k");
        assert_eq!(compact(u64::MAX), format!("{}k", u64::MAX / 1000));
    }

    #[test]
    fn a_usage_names_the_used_and_the_available_tokens() {
        assert_eq!(usage(41_260, 200_000), "41k / 200k");
        assert_eq!(usage(850, 1000), "850 / 1k");
    }
}
