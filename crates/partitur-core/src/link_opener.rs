//! The command a clicked link is opened with.
//!
//! The `link_opener` setting is one string, split on whitespace into a program and its
//! arguments; the link is appended as the last argument. No shell is involved and there is no
//! quoting, so `"firefox -P work"` runs `firefox` with `-P`, `work` and the link. Without the
//! setting a link is opened with `xdg-open`.

use std::fmt;

/// The program a link is opened with when no level sets `link_opener`.
pub const DEFAULT_LINK_OPENER: &str = "xdg-open";

/// A parsed `link_opener`: a program and the arguments that come before the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOpener {
    program: String,
    args: Vec<String>,
}

/// Why a `link_opener` value cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkOpenerError {
    /// The value holds no program, only whitespace or nothing.
    #[error("link_opener names no program")]
    Empty,
}

impl LinkOpener {
    /// Splits `value` on whitespace into a program and its arguments.
    ///
    /// # Errors
    ///
    /// Returns [`LinkOpenerError::Empty`] when `value` contains nothing but whitespace.
    pub fn parse(value: &str) -> Result<Self, LinkOpenerError> {
        let mut words = value.split_whitespace().map(str::to_owned);
        let program = words.next().ok_or(LinkOpenerError::Empty)?;
        Ok(Self {
            program,
            args: words.collect(),
        })
    }

    /// The program to run.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The arguments for opening `link`: the configured ones followed by `link`.
    pub fn args_for(&self, link: &str) -> Vec<String> {
        let mut args = self.args.clone();
        args.push(link.to_owned());
        args
    }
}

impl Default for LinkOpener {
    fn default() -> Self {
        Self {
            program: DEFAULT_LINK_OPENER.to_owned(),
            args: Vec::new(),
        }
    }
}

impl fmt::Display for LinkOpener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_value_splits_into_program_and_arguments_before_the_link() {
        let opener = LinkOpener::parse("  firefox\t-P  work ").unwrap();
        assert_eq!(opener.program(), "firefox");
        assert_eq!(
            opener.args_for("https://example.com"),
            ["-P", "work", "https://example.com"]
        );
        assert_eq!(opener.to_string(), "firefox -P work");
    }

    #[test]
    fn a_bare_program_gets_only_the_link() {
        let opener = LinkOpener::parse("open").unwrap();
        assert_eq!(opener.args_for("x"), ["x"]);
        assert_eq!(opener.to_string(), "open");
    }

    #[test]
    fn an_empty_value_is_rejected() {
        assert_eq!(LinkOpener::parse(" \t"), Err(LinkOpenerError::Empty));
        assert_eq!(
            LinkOpenerError::Empty.to_string(),
            "link_opener names no program"
        );
    }

    #[test]
    fn the_default_is_xdg_open() {
        let opener = LinkOpener::default();
        assert_eq!(opener.program(), "xdg-open");
        assert_eq!(opener.args_for("x"), ["x"]);
    }
}
