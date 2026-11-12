//! Token counts as the interface writes them: the plain number below a thousand, whole
//! thousands with a `k` from there, millions to one decimal with an `M` once the thousands
//! would round to a thousand or more (`1M`, `1.5M`), and a context usage as `68k / 1M`.

/// `count` tokens written compactly: `850` stays as it is, from 1000 up the count is
/// rounded to the nearest thousand and written as `141k`. A count that would round to
/// `1000k` or more is rounded to the nearest tenth of a million instead and written as
/// `1M` or `1.5M`; a whole number of millions has no decimal.
pub fn compact(count: u64) -> String {
    if count < 1000 {
        return count.to_string();
    }
    let thousands = count.saturating_add(500) / 1000;
    if thousands < 1000 {
        return format!("{thousands}k");
    }
    let tenths = count.saturating_add(50_000) / 100_000;
    match tenths % 10 {
        0 => format!("{}M", tenths / 10),
        tenth => format!("{}.{tenth}M", tenths / 10),
    }
}

/// A context usage of `used` out of `size` tokens, each written compactly: `68k / 1M`.
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
        assert_eq!(compact(141_000), "141k");
        assert_eq!(compact(999_499), "999k");
    }

    #[test]
    fn counts_that_round_to_a_thousand_thousands_are_written_in_millions() {
        assert_eq!(compact(999_500), "1M");
        assert_eq!(compact(1_000_000), "1M");
        assert_eq!(compact(1_049_999), "1M");
        assert_eq!(compact(1_050_000), "1.1M");
        assert_eq!(compact(1_500_000), "1.5M");
        assert_eq!(compact(12_000_000), "12M");
        assert_eq!(
            compact(u64::MAX),
            format!("{}.{}M", u64::MAX / 1_000_000, (u64::MAX / 100_000) % 10)
        );
    }

    #[test]
    fn a_usage_names_the_used_and_the_available_tokens() {
        assert_eq!(usage(41_260, 200_000), "41k / 200k");
        assert_eq!(usage(850, 1000), "850 / 1k");
        assert_eq!(usage(68_000, 1_000_000), "68k / 1M");
    }
}
