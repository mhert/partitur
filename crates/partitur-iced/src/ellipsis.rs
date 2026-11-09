//! Cutting text to a budget of characters, with `…` standing for what was cut.
//!
//! A line of the interface has a fixed room, and text that does not fit is clipped
//! mid-letter rather than shortened, so long text is cut here before it is drawn.

/// `label` cut after `budget` characters, with `…` appended when it was longer.
pub fn keep_start(label: &str, budget: usize) -> String {
    let mut characters = label.chars();
    let mut kept: String = characters.by_ref().take(budget).collect();
    if characters.next().is_some() {
        kept.push('…');
    }
    kept
}

/// The last `budget` characters of `label`, with `…` in front when it was longer: a
/// path's last parts tell more than its first.
pub fn keep_end(label: &str, budget: usize) -> String {
    let length = label.chars().count();
    if length <= budget {
        return label.to_owned();
    }
    let kept: String = label.chars().skip(length.saturating_sub(budget)).collect();
    format!("…{kept}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_within_the_budget_is_kept_whole() {
        assert_eq!(keep_start("short", 5), "short");
        assert_eq!(keep_end("~/shop", 6), "~/shop");
    }

    #[test]
    fn longer_text_is_cut_on_a_character_boundary() {
        assert_eq!(keep_start("too long", 3), "too…");
        assert_eq!(keep_start("größer", 4), "größ…");
        assert_eq!(keep_end("~/Development/shop", 4), "…shop");
        assert_eq!(keep_end("~/größer", 4), "…ößer");
    }
}
