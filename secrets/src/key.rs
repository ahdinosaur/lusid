//! Recipient-side keys.
//!
//! Parsed from the strings declared in `lusid-secrets.toml`'s `[operators]`
//! and `[machines]` tables. Each variant wraps an age crate recipient: an
//! age x25519 public key (`age1...`) for operators, or an SSH public key
//! (`ssh-ed25519 ...` / `ssh-rsa ...`) for machines.

use std::collections::HashSet;
use std::str::FromStr;

use age_core::format::{FileKey, Stanza};
use displaydoc::Display;
use thiserror::Error;

/// A single parsed recipient key. Parsed eagerly so malformed keys surface
/// at load time rather than on first use.
#[derive(Debug, Clone)]
pub enum Key {
    X25519(age::x25519::Recipient),
    Ssh(age::ssh::Recipient),
}

impl age::Recipient for Key {
    fn wrap_file_key(
        &self,
        file_key: &FileKey,
    ) -> Result<(Vec<Stanza>, HashSet<String>), age::EncryptError> {
        match self {
            Key::X25519(r) => r.wrap_file_key(file_key),
            Key::Ssh(r) => r.wrap_file_key(file_key),
        }
    }
}

impl FromStr for Key {
    type Err = KeyParseError;

    /// Parse a recipient by prefix: `age1...` → x25519; `ssh-ed25519 ...` or
    /// `ssh-rsa ...` → SSH. Trailing SSH comments (`... user@host`) are
    /// tolerated and stripped.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.starts_with("age1") {
            let r = age::x25519::Recipient::from_str(trimmed).map_err(KeyParseError::X25519)?;
            Ok(Key::X25519(r))
        } else if trimmed.starts_with("ssh-") {
            let mut parts = trimmed.split_whitespace();
            let kind = parts.next().ok_or(KeyParseError::Empty)?;
            let body = parts.next().ok_or(KeyParseError::Empty)?;
            let canonical = format!("{kind} {body}");
            let r = age::ssh::Recipient::from_str(&canonical).map_err(KeyParseError::Ssh)?;
            Ok(Key::Ssh(r))
        } else {
            Err(KeyParseError::UnknownPrefix)
        }
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::X25519(r) => write!(f, "{r}"),
            Key::Ssh(r) => write!(f, "{r}"),
        }
    }
}

#[derive(Debug, Error, Display)]
pub enum KeyParseError {
    /// Empty recipient
    Empty,

    /// Unknown recipient prefix (expected age1... or ssh-...)
    UnknownPrefix,

    /// Invalid x25519 recipient: {0}
    X25519(&'static str),

    /// Invalid SSH recipient: {0:?}
    Ssh(age::ssh::ParseRecipientKeyError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SSH_ED25519_PUB: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHsKLqeplhpW+uObz5dvMgjz1OxfM/XXUB+VHtZ6isGN";

    #[test]
    fn parses_x25519_recipient() {
        let id = age::x25519::Identity::generate();
        let pub_str = id.to_public().to_string();
        let key: Key = pub_str.parse().unwrap();
        assert!(matches!(key, Key::X25519(_)));
    }

    #[test]
    fn parses_ssh_ed25519_recipient() {
        let key: Key = TEST_SSH_ED25519_PUB.parse().unwrap();
        assert!(matches!(key, Key::Ssh(_)));
    }

    #[test]
    fn parses_ssh_with_comment() {
        let with_comment = format!("{TEST_SSH_ED25519_PUB} user@host");
        let key: Key = with_comment.parse().unwrap();
        assert!(matches!(key, Key::Ssh(_)));
    }

    #[test]
    fn rejects_unknown_prefix() {
        assert!(matches!(
            "not-a-key".parse::<Key>().unwrap_err(),
            KeyParseError::UnknownPrefix
        ));
    }
}
