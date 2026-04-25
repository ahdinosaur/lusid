//! Decrypted secret plaintexts and the bundle that owns them.

use std::collections::HashMap;
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretBox};

use crate::redactor::{REDACT_MIN_LEN, Redactor};

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
///
/// `Secrets` owns its plaintexts via [`Secret`] (an `Arc<SecretBox<String>>`)
/// so `Debug` is redacted and the plaintext is zeroised when the last clone
/// is dropped. Build a [`Redactor`] from a bundle via [`Secrets::redactor`]
/// before handing the bundle off (e.g. to `Context::set_secrets`); the
/// redactor holds independent `Arc` clones and stays valid after the
/// original bundle is moved.
#[derive(Debug, Default, Clone)]
pub struct Secrets {
    values: HashMap<String, Secret>,
}

impl Secrets {
    pub fn empty() -> Self {
        Self::default()
    }

    pub(crate) fn from_values(values: HashMap<String, Secret>) -> Self {
        Self { values }
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

    /// Build a [`Redactor`] over every secret whose plaintext is at least
    /// [`REDACT_MIN_LEN`] bytes. Shorter secrets are skipped because
    /// substring-replacing e.g. a 2-byte secret against arbitrary process
    /// output would match far too aggressively (`"ab"` would redact every
    /// occurrence of those two bytes in every log line).
    pub fn redactor(&self) -> Redactor {
        let mut secrets: Vec<Secret> = self
            .values
            .values()
            .filter(|s| s.expose_secret().len() >= REDACT_MIN_LEN)
            .cloned()
            .collect();
        // Longest-first: if secret B is a substring of secret A, redacting
        // A first ensures B never partially matches inside A's plaintext.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.expose_secret().len()));
        Redactor::new(secrets)
    }
}
