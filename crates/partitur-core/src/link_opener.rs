//! What a clicked link is opened with.
//!
//! Without a `link_opener` setting a link goes to the system's default handler (`open` on
//! macOS, `xdg-open` on Linux). The setting is one string, split on whitespace into a program
//! and its arguments; the link is appended as the last argument. No shell is involved and
//! there is no quoting, so `"firefox -P work"` runs `firefox` with `-P`, `work` and the link.

use std::fmt;

/// How a link is opened: by the system's default handler, or by a configured command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LinkOpener {
    /// The system's default handler, used when no level sets `link_opener`.
    #[default]
    System,
    /// A program and the arguments that come before the link.
    Command {
        /// The program to run.
        program: String,
        /// The arguments placed before the link.
        args: Vec<String>,
    },
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
        Ok(Self::Command {
            program,
            args: words.collect(),
        })
    }

    /// The arguments for opening `link`: the configured ones followed by `link`.
    pub fn args_for(&self, link: &str) -> Vec<String> {
        let mut args = match self {
            Self::System => Vec::new(),
            Self::Command { args, .. } => args.clone(),
        };
        args.push(link.to_owned());
        args
    }
}

impl fmt::Display for LinkOpener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => f.write_str("the system's default handler"),
            Self::Command { program, args } => {
                f.write_str(program)?;
                for arg in args {
                    write!(f, " {arg}")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_value_splits_into_program_and_arguments_before_the_link() {
        let opener = LinkOpener::parse("  firefox\t-P  work ").unwrap();
        assert_eq!(
            opener,
            LinkOpener::Command {
                program: "firefox".into(),
                args: vec!["-P".into(), "work".into()]
            }
        );
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
    fn the_default_is_the_system_handler() {
        let opener = LinkOpener::default();
        assert_eq!(opener, LinkOpener::System);
        assert_eq!(opener.to_string(), "the system's default handler");
        assert_eq!(opener.args_for("x"), ["x"]);
    }
}
