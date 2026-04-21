//! Age-encrypted secrets for lusid plans.
//!
//! A lusid project stores secrets as individual `*.age` files under a
//! `secrets/` directory. At apply time the host's [`Identity`] decrypts the
//! subset of files it's a recipient for and hands the plaintexts to
//! `@core/secret` resources by name. Plaintexts never enter the Rimu
//! evaluator — plans reference secrets by name, contents materialise at
//! apply.
//!
//! This crate provides the primitives ([`Identity`], [`Key`], byte-level
//! encrypt/decrypt), the `lusid-secrets.toml` [`Recipients`] model, and the
//! apply-time [`Secrets`] bundle plus [`decrypt_dir`] / [`alias_for_identity`].
//! The CLI lands in a later phase.

mod crypto;
mod identity;
mod key;
mod recipients;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use displaydoc::Display;
use secrecy::SecretBox;
use thiserror::Error;
use tokio::fs;

pub use crate::crypto::{
    DecryptError, EncryptError, HeaderError, decrypt_bytes, encrypt_bytes, read_header_stanzas,
};
pub use crate::identity::{Identity, IdentityError};
pub use crate::key::{Key, KeyParseError};
pub use crate::recipients::{
    FileEntry, Recipients, RecipientsError, ResolveError, ResolvedRecipient, SECRETS_FILE,
};

/// Decrypted secret plaintext. Wrapped in [`Arc`] so cloning (e.g. into a
/// redactor) is cheap, and in [`SecretBox<String>`] so `Debug` is redacted
/// and the plaintext is zeroised when the last clone drops.
///
/// `SecretBox<String>` (rather than [`secrecy::SecretString`], a.k.a.
/// `SecretBox<str>`) is used because only the sized form implements
/// `serde::Deserialize`.
pub type Secret = Arc<SecretBox<String>>;

/// A bundle of decrypted secrets, keyed by filename stem (e.g. the file
/// `secrets/api_key.age` becomes `api_key`).
#[derive(Debug, Default, Clone)]
pub struct Secrets {
    values: HashMap<String, Secret>,
}

impl Secrets {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&Secret> {
        self.values.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Secret)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
}

/// Decrypt the named `*.age` files under `secrets_dir` with `identity`.
///
/// `file_stems` is the subset of secrets the caller wants to read — typically
/// the result of [`Recipients::files_for_alias`] for the alias matching the
/// host identity. Files outside this list are not opened. Each stem maps to
/// `<secrets_dir>/<stem>.age`; a missing file or a decrypt failure is fatal
/// (no silent fallback to an empty bundle).
///
/// An empty `file_stems` returns an empty [`Secrets`] without touching the
/// filesystem. Pass an empty slice on the "no identity supplied" path.
#[tracing::instrument(skip(identity, file_stems), fields(dir = %secrets_dir.display(), count = file_stems.len()))]
pub async fn decrypt_dir(
    identity: &Identity,
    secrets_dir: &Path,
    file_stems: &[&str],
) -> Result<Secrets, DecryptDirError> {
    if file_stems.is_empty() {
        return Ok(Secrets::empty());
    }

    let mut values: HashMap<String, Secret> = HashMap::with_capacity(file_stems.len());
    for stem in file_stems {
        let path = secrets_dir.join(format!("{stem}.age"));
        let ciphertext = fs::read(&path)
            .await
            .map_err(|source| DecryptDirError::ReadFile {
                path: path.clone(),
                source,
            })?;
        let plaintext = decrypt_bytes(identity, &path, &ciphertext)?;
        values.insert((*stem).to_owned(), plaintext);
    }

    tracing::info!(count = values.len(), "decrypted secrets");
    Ok(Secrets { values })
}

#[derive(Debug, Error, Display)]
pub enum DecryptDirError {
    /// Failed to scan secrets dir {dir}: {source}
    ScanDir {
        dir: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to read encrypted file {path}: {source}
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// {0}
    Decrypt(#[from] DecryptError),
}

/// Find the alias in `[operators]` or `[machines]` whose key matches
/// `identity`. Implemented as an encrypt-then-decrypt round-trip so it works
/// uniformly across x25519 and SSH without leaking the identity's public
/// material out of the `age` crate.
///
/// Returns `None` when no alias matches; callers should treat this as a hard
/// configuration error (the supplied identity isn't declared anywhere).
pub fn alias_for_identity<'a>(identity: &Identity, recipients: &'a Recipients) -> Option<&'a str> {
    let probe_path = Path::new("__alias_match__");
    for (alias, key) in recipients
        .operators
        .iter()
        .chain(recipients.machines.iter())
    {
        let boxed: Vec<Box<dyn age::Recipient + Send>> = match key {
            Key::X25519(k) => vec![Box::new(k.clone())],
            Key::Ssh(k) => vec![Box::new(k.clone())],
        };
        let Ok(ct) = encrypt_bytes(&boxed, probe_path, b"") else {
            continue;
        };
        if decrypt_bytes(identity, probe_path, &ct).is_ok() {
            return Some(alias.as_str());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;
    use secrecy::ExposeSecret;
    use tempfile::TempDir;

    use super::*;

    fn empty_recipients() -> Recipients {
        Recipients {
            operators: IndexMap::new(),
            machines: IndexMap::new(),
            groups: IndexMap::new(),
            files: IndexMap::new(),
        }
    }

    #[tokio::test]
    async fn decrypt_dir_round_trips() {
        let id = age::x25519::Identity::generate();
        let identity: Identity = id.to_string().expose_secret().parse().unwrap();
        let recipient: Box<dyn age::Recipient + Send> = Box::new(id.to_public());
        let ct = encrypt_bytes(&[recipient], Path::new("hello"), b"world").unwrap();

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.age"), &ct).unwrap();

        let secrets = decrypt_dir(&identity, dir.path(), &["hello"])
            .await
            .unwrap();
        assert_eq!(secrets.len(), 1);
        assert_eq!(
            secrets.get("hello").unwrap().expose_secret().as_str(),
            "world"
        );
    }

    #[tokio::test]
    async fn decrypt_dir_empty_stems_returns_empty() {
        let id = age::x25519::Identity::generate();
        let identity: Identity = id.to_string().expose_secret().parse().unwrap();
        let secrets = decrypt_dir(&identity, Path::new("/nonexistent"), &[])
            .await
            .unwrap();
        assert!(secrets.is_empty());
    }

    #[tokio::test]
    async fn decrypt_dir_missing_file_errors() {
        let id = age::x25519::Identity::generate();
        let identity: Identity = id.to_string().expose_secret().parse().unwrap();
        let dir = TempDir::new().unwrap();
        let err = decrypt_dir(&identity, dir.path(), &["absent"])
            .await
            .unwrap_err();
        assert!(matches!(err, DecryptDirError::ReadFile { .. }));
    }

    #[test]
    fn alias_for_identity_x25519() {
        let id = age::x25519::Identity::generate();
        let identity: Identity = id.to_string().expose_secret().parse().unwrap();
        let mut r = empty_recipients();
        r.operators
            .insert("me".to_owned(), Key::X25519(id.to_public()));
        assert_eq!(alias_for_identity(&identity, &r), Some("me"));
    }

    #[test]
    fn alias_for_identity_no_match() {
        let id_a = age::x25519::Identity::generate();
        let id_b = age::x25519::Identity::generate();
        let identity: Identity = id_b.to_string().expose_secret().parse().unwrap();
        let mut r = empty_recipients();
        r.operators
            .insert("a".to_owned(), Key::X25519(id_a.to_public()));
        assert!(alias_for_identity(&identity, &r).is_none());
    }
}
