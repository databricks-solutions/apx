//! Slash-command parsing for agent TUI input.
//!
//! This module provides pure parsing types with no I/O or terminal
//! dependencies.  Execution lives in the CLI crate.

use std::fmt;

/// Normalized command name (lowercase, without the leading `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandName(String);

impl CommandName {
    /// Parse a command name from a raw token (e.g. `"model"` or `"EXIT"`).
    ///
    /// Returns `None` if the input is empty.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self(trimmed.to_lowercase()))
    }

    /// The normalized name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Positional arguments following a command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandArgs(Vec<String>);

impl CommandArgs {
    /// Get the argument at position `n` (zero-based).
    #[must_use]
    pub fn get(&self, n: usize) -> Option<&str> {
        self.0.get(n).map(String::as_str)
    }

    /// Number of arguments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if there are no arguments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Result of parsing raw user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedInput {
    /// Plain text to send to the model.
    Message(String),
    /// A slash command with optional arguments.
    Command {
        /// The normalized command name.
        name: CommandName,
        /// Positional arguments following the command.
        args: CommandArgs,
    },
}

/// Parse raw user input into a [`ParsedInput`].
///
/// Input starting with `/` followed by a non-whitespace character is
/// treated as a command; everything else is a plain message.
#[must_use]
pub fn parse_input(raw: &str) -> ParsedInput {
    let trimmed = raw.trim();

    // Must start with `/` and have at least one non-whitespace char after it
    if let Some(rest) = trimmed.strip_prefix('/')
        && !rest.is_empty()
        && !rest.starts_with(char::is_whitespace)
    {
        let mut parts = rest.split_whitespace();
        if let Some(cmd) = parts.next()
            && let Some(name) = CommandName::new(cmd)
        {
            let args: Vec<String> = parts.map(String::from).collect();
            return ParsedInput::Command {
                name,
                args: CommandArgs(args),
            };
        }
    }

    ParsedInput::Message(trimmed.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_no_args() {
        let result = parse_input("/model");
        assert_eq!(
            result,
            ParsedInput::Command {
                name: CommandName("model".into()),
                args: CommandArgs(vec![]),
            }
        );
    }

    #[test]
    fn parse_command_with_args() {
        let result = parse_input("/model foo");
        assert_eq!(
            result,
            ParsedInput::Command {
                name: CommandName("model".into()),
                args: CommandArgs(vec!["foo".into()]),
            }
        );
    }

    #[test]
    fn parse_exit_command() {
        let result = parse_input("/exit");
        assert_eq!(
            result,
            ParsedInput::Command {
                name: CommandName("exit".into()),
                args: CommandArgs(vec![]),
            }
        );
    }

    #[test]
    fn parse_case_insensitive() {
        let result = parse_input("/EXIT");
        assert_eq!(
            result,
            ParsedInput::Command {
                name: CommandName("exit".into()),
                args: CommandArgs(vec![]),
            }
        );
    }

    #[test]
    fn parse_slash_alone_is_message() {
        let result = parse_input("/");
        assert_eq!(result, ParsedInput::Message("/".into()));
    }

    #[test]
    fn parse_plain_text() {
        let result = parse_input("hello world");
        assert_eq!(result, ParsedInput::Message("hello world".into()));
    }

    #[test]
    fn parse_leading_trailing_whitespace() {
        let result = parse_input(" /model ");
        assert_eq!(
            result,
            ParsedInput::Command {
                name: CommandName("model".into()),
                args: CommandArgs(vec![]),
            }
        );
    }

    #[test]
    fn command_name_display() {
        let name = CommandName::new("Model").unwrap();
        assert_eq!(name.to_string(), "model");
        assert_eq!(name.as_str(), "model");
    }

    #[test]
    fn command_args_accessors() {
        let args = CommandArgs(vec!["a".into(), "b".into()]);
        assert_eq!(args.len(), 2);
        assert!(!args.is_empty());
        assert_eq!(args.get(0), Some("a"));
        assert_eq!(args.get(1), Some("b"));
        assert_eq!(args.get(2), None);
    }

    #[test]
    fn empty_command_args() {
        let args = CommandArgs(vec![]);
        assert!(args.is_empty());
        assert_eq!(args.len(), 0);
    }
}
