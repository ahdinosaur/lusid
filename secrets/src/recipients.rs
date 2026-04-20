//! `<secrets_dir>/recipients.toml` — the project-level table mapping each
//! `*.age` file to the recipients that can decrypt it.
//!
//! Shape:
//!
//! ```toml
//! [operators]
//! mikey = "age1..."
//!
//! [machines]
//! rpi4b-1 = "ssh-ed25519 AAAA..."
//!
//! [groups]
//! operators = ["mikey"]
//!
//! [files]
//! "api_token" = { recipients = ["@operators", "rpi4b-1"] }
//! ```
//!
//! `@name` references in a file's `recipients` list expand via `[groups]`;
//! bare names look up in `[operators]` then `[machines]`. Expansion is shallow
//! (groups cannot reference groups) — keeps the model predictable without
//! meaningfully limiting usage.
//!
//! The operator / machine split is load-bearing for per-target re-encryption
//! at apply time: `lusid-apply`'s host uses the target machine's SSH host key
//! (looked up in `[machines]` by `machine_id`) as the sole recipient before
//! shipping ciphertext to the guest. See [`Recipients::get_machine`] and
//! [`crate::reencrypt_for_machine`].

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::Recipient;
use age_core::format::{FileKey, Stanza};
use displaydoc::Display;
use indexmap::IndexMap;
use serde::Deserialize;
use thiserror::Error;
use tokio::fs;

pub const RECIPIENTS_FILE: &str = "recipients.toml";

/// Parsed `recipients.toml`. Order preserved so listing commands match
/// on-disk order. Operator and machine aliases share a single namespace at
/// resolve time; load-time validation rejects duplicates across the two.
#[derive(Debug, Clone)]
pub struct Recipients {
    pub operators: IndexMap<String, Key>,
    pub machines: IndexMap<String, Key>,
    pub groups: IndexMap<String, Vec<String>>,
    pub files: IndexMap<String, FileEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RecipientsToml {
    #[serde(default)]
    operators: IndexMap<String, Key>,
    #[serde(default)]
    machines: IndexMap<String, Key>,
    #[serde(default)]
    groups: IndexMap<String, Vec<String>>,
    #[serde(default)]
    files: IndexMap<String, FileEntry>,
}

/// A single entry in `[keys]`. Parsed eagerly so a malformed key is surfaced
/// at load time rather than on first use.
#[derive(Debug, Clone)]
pub enum Key {
    X25519(age::x25519::Recipient),
    Ssh(age::ssh::Recipient),
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub recipients: Vec<String>,
}

impl Recipients {
    /// Load `recipients.toml` from `<secrets_dir>/recipients.toml`. Missing
    /// file returns [`RecipientsError::Missing`]. A collision between an
    /// operator and machine alias is rejected up-front — aliases share a
    /// namespace at resolve time.
    pub async fn load(secrets_dir: &Path) -> Result<Self, RecipientsError> {
        let path = secrets_dir.join(RECIPIENTS_FILE);
        let text = match fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(RecipientsError::Missing { path });
            }
            Err(source) => return Err(RecipientsError::Read { path, source }),
        };
        let raw: RecipientsToml =
            toml::from_str(&text).map_err(|source| RecipientsError::Parse { path, source })?;
        Self::from_toml(raw)
    }

    fn from_toml(raw: RecipientsToml) -> Result<Self, RecipientsError> {
        let RecipientsToml {
            operators,
            machines,
            groups,
            files,
        } = raw;
        for alias in operators.keys() {
            if machines.contains_key(alias) {
                return Err(RecipientsError::AliasCollision {
                    alias: alias.clone(),
                });
            }
        }
        Ok(Recipients {
            operators,
            machines,
            groups,
            files,
        })
    }

    /// Resolve a file stem's recipient list into concrete age recipients.
    ///
    /// Group references (`@name`) are expanded; duplicate aliases are
    /// deduplicated. Two aliases pointing at the same underlying key are
    /// kept separate — age will emit one stanza per recipient call and the
    /// reader needs the matching stanza to decrypt.
    pub fn resolve(&self, stem: &str) -> Result<Vec<ResolvedRecipient>, ResolveError> {
        let entry = self
            .files
            .get(stem)
            .ok_or_else(|| ResolveError::UnknownFile {
                stem: stem.to_owned(),
            })?;

        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out = Vec::new();
        for name in &entry.recipients {
            if let Some(group) = name.strip_prefix('@') {
                let members = self
                    .groups
                    .get(group)
                    .ok_or_else(|| ResolveError::UnknownGroup {
                        file: stem.to_owned(),
                        group: group.to_owned(),
                    })?;
                for member in members {
                    if seen.insert(member.clone()) {
                        out.push(self.lookup(stem, member, Some(group))?);
                    }
                }
            } else if seen.insert(name.clone()) {
                out.push(self.lookup(stem, name, None)?);
            }
        }
        Ok(out)
    }

    fn lookup(
        &self,
        stem: &str,
        alias: &str,
        via_group: Option<&str>,
    ) -> Result<ResolvedRecipient, ResolveError> {
        let key = self
            .operators
            .get(alias)
            .or_else(|| self.machines.get(alias))
            .ok_or_else(|| ResolveError::UnknownAlias {
                file: stem.to_owned(),
                alias: alias.to_owned(),
                via_group: via_group.map(str::to_owned),
            })?;
        Ok(ResolvedRecipient {
            alias: alias.to_owned(),
            key: key.clone(),
        })
    }

    /// Look up a machine's recipient key by `machine_id`. Returns the matching
    /// entry from `[machines]`, or `None` if the alias is absent. Deliberately
    /// does not fall back to `[operators]` — per-target re-encryption only
    /// ever encrypts to a machine's own key.
    pub fn get_machine(&self, machine_id: &str) -> Option<&Key> {
        self.machines.get(machine_id)
    }

    /// Every file stem listed in `[files]`, in declaration order.
    pub fn file_stems(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }
}

/// One recipient for a specific file, carrying its alias for display.
#[derive(Debug, Clone)]
pub struct ResolvedRecipient {
    pub alias: String,
    pub key: Key,
}

impl Key {
    /// Identifying tag used when comparing header stanzas. Deterministic per
    /// recipient — age writes the same tag into every file encrypted to this
    /// key. `None` for x25519 (x25519 stanzas are anonymous: `args[0]` is an
    /// ephemeral pubkey that varies per encryption).
    pub fn ssh_stanza_tag(&self) -> Option<String> {
        match self {
            Key::X25519(_) => None,
            Key::Ssh(ssh) => {
                // age::Recipient::wrap_file_key is deterministic on the
                // pubkey part of the stanza args, so we use it to extract
                // the SSH tag without re-implementing the tag computation.
                let dummy = FileKey::new(Box::new([0u8; 16]));
                let (stanzas, _) = ssh
                    .wrap_file_key(&dummy)
                    .expect("ssh recipient always wraps");
                stanzas
                    .into_iter()
                    .next()
                    .and_then(|s| s.args.into_iter().next())
            }
        }
    }
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

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let raw = String::deserialize(deserializer)?;
        Key::from_str(&raw).map_err(D::Error::custom)
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
pub enum RecipientsError {
    /// Missing {path}
    Missing { path: PathBuf },

    /// Failed to read {path}: {source}
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse {path}: {source}
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// Alias {alias:?} declared in both [operators] and [machines]
    AliasCollision { alias: String },
}

#[derive(Debug, Error, Display)]
pub enum ResolveError {
    /// No [files] entry for {stem:?}
    UnknownFile { stem: String },

    /// File {file:?} references unknown group @{group}
    UnknownGroup { file: String, group: String },

    /// File {file:?} references unknown key alias {alias:?} (via group {via_group:?})
    UnknownAlias {
        file: String,
        alias: String,
        via_group: Option<String>,
    },
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

/// Collect resolved recipients into the `Box<dyn Recipient + Send>` form
/// that [`crate::crypto::encrypt_bytes`] expects.
pub fn to_boxed_recipients(resolved: &[ResolvedRecipient]) -> Vec<Box<dyn Recipient + Send>> {
    resolved
        .iter()
        .map(|r| -> Box<dyn Recipient + Send> {
            match &r.key {
                Key::X25519(k) => Box::new(k.clone()),
                Key::Ssh(k) => Box::new(k.clone()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[operators]
mikey = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa"

[machines]
rpi = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHsKLqeplhpW+uObz5dvMgjz1OxfM/XXUB+VHtZ6isGN alice@rust"

[groups]
operators = ["mikey"]

[files]
"api_token" = { recipients = ["@operators", "rpi"] }
"db_pw" = { recipients = ["@operators"] }
"#;

    fn parse_toml(s: &str) -> Result<Recipients, RecipientsError> {
        let raw: RecipientsToml = toml::from_str(s).unwrap();
        Recipients::from_toml(raw)
    }

    fn parse() -> Recipients {
        parse_toml(SAMPLE).unwrap()
    }

    #[test]
    fn parses_operators_machines_groups_files() {
        let r = parse();
        assert_eq!(r.operators.len(), 1);
        assert_eq!(r.machines.len(), 1);
        assert!(matches!(r.operators["mikey"], Key::X25519(_)));
        assert!(matches!(r.machines["rpi"], Key::Ssh(_)));
        assert_eq!(r.groups["operators"], vec!["mikey"]);
        assert_eq!(r.files.len(), 2);
    }

    #[test]
    fn resolves_file_with_group_and_alias() {
        let r = parse();
        let recipients = r.resolve("api_token").unwrap();
        let aliases: Vec<&str> = recipients.iter().map(|x| x.alias.as_str()).collect();
        assert_eq!(aliases, vec!["mikey", "rpi"]);
    }

    #[test]
    fn deduplicates_across_expansion() {
        let r = parse_toml(
            r#"
[operators]
a = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa"

[groups]
g = ["a"]

[files]
"f" = { recipients = ["a", "@g", "a"] }
"#,
        )
        .unwrap();
        let recipients = r.resolve("f").unwrap();
        assert_eq!(recipients.len(), 1);
    }

    #[test]
    fn unknown_file() {
        let r = parse();
        assert!(matches!(
            r.resolve("nope").unwrap_err(),
            ResolveError::UnknownFile { .. }
        ));
    }

    #[test]
    fn unknown_alias() {
        let r = parse_toml(
            r#"
[operators]
a = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa"
[files]
"f" = { recipients = ["b"] }
"#,
        )
        .unwrap();
        assert!(matches!(
            r.resolve("f").unwrap_err(),
            ResolveError::UnknownAlias { .. }
        ));
    }

    #[test]
    fn unknown_group() {
        let r = parse_toml(
            r#"
[operators]
a = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa"
[files]
"f" = { recipients = ["@bogus"] }
"#,
        )
        .unwrap();
        assert!(matches!(
            r.resolve("f").unwrap_err(),
            ResolveError::UnknownGroup { .. }
        ));
    }

    #[test]
    fn alias_collision_errors() {
        let err = parse_toml(
            r#"
[operators]
dup = "age1t7rxyev2z3rw82stdlrrepyc39nvn86l5078zqkf5uasdy86jp6svpy7pa"

[machines]
dup = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHsKLqeplhpW+uObz5dvMgjz1OxfM/XXUB+VHtZ6isGN alice@rust"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, RecipientsError::AliasCollision { .. }));
    }

    #[test]
    fn get_machine_only_returns_from_machines_table() {
        let r = parse();
        assert!(r.get_machine("rpi").is_some());
        // Operators are deliberately excluded.
        assert!(r.get_machine("mikey").is_none());
    }

    #[test]
    fn ssh_stanza_tag_deterministic() {
        let r = parse();
        let ssh_key = r.machines["rpi"].clone();
        let tag1 = ssh_key.ssh_stanza_tag().unwrap();
        let tag2 = ssh_key.ssh_stanza_tag().unwrap();
        assert_eq!(tag1, tag2);
        assert!(!tag1.is_empty());
    }
}
