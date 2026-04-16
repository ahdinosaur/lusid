//! Age-encrypted secrets for lusid plans.
//!
//! A lusid project stores secrets as individual `*.age` files, decrypted at
//! the start of an `apply` with a single project-scoped [`Identity`]. The
//! decrypted values are exposed to plans via the `ctx.secrets` Rimu object
//! (see `lusid-plan`).
//!
//! # Scope (v1)
//!
//! - Local apply only (running on the machine being configured).
//! - x25519 identities only — no passphrase-wrapped keys.
//! - A single flat directory of `*.age` files — no nested namespaces.
//! - Eager decryption: every secret is decrypted up-front at `load_all`,
//!   regardless of whether any given plan actually uses it. This keeps the
//!   redaction table complete (so output scanning cannot miss a secret that
//!   a plan happened to forward through an operation we didn't anticipate).
//!
//! # Remote / dev apply (`TODO(cc)`)
//!
//! `lusid-apply` currently expects the decrypted secrets + identity live on
//! the same machine as the plan target. For `cmd_dev_apply` (libvirt VM) and
//! `cmd_remote_apply` (SSH) this is not obviously safe. Three options,
//! none implemented yet:
//!
//! 1. **Ship the identity** to the target machine and decrypt there. Simple
//!    but widens the trust radius — the VM/remote host holds the decryption
//!    key.
//! 2. **Decrypt on the host, ship the plaintext** inside the apply stdio
//!    pipe. Keeps the identity local but puts plaintext on the wire (SSH
//!    is encrypted, but we still have to hold plaintext in-memory on two
//!    machines).
//! 3. **Re-encrypt per target**: each target machine has its own age
//!    recipient, the host re-encrypts secrets for that recipient before
//!    shipping. Best security, most setup cost (per-target key management).
//!    This is also the natural destination even without a remote story:
//!    today every machine that holds the project identity can decrypt every
//!    secret, with no way to scope (e.g.) a laptop-only secret away from a
//!    VPS. agenix / sops-nix both model secrets as "encrypted to a list of
//!    recipients" for exactly this reason.
//!
//! Option 2 is the likely first cut. Whichever we pick, the [`Secrets`]
//! type here is what the remote side would need to reconstruct. Until one
//! is picked, `cmd_dev_apply` errors with `AppError::SecretsNotYetSupported`
//! when the project has secrets configured (see `lusid/src/lib.rs`).
//!
//! # Key rotation (`TODO(cc)`)
//!
//! No rotation tooling today. If the project identity is ever exposed, the
//! correct response is to (1) rotate each secret plaintext, (2) generate a
//! new identity, and (3) re-encrypt each `*.age` file to the new recipient.
//! agenix ships this as `agenix -r`. Worth a small CLI surface here once
//! per-target recipients land — the two features share the re-encryption
//! primitive.
//!
//! # UTF-8 plaintext only (`Note(cc)`)
//!
//! [`decrypt_bytes`] decodes every decrypted payload as UTF-8 and errors
//! with [`DecryptError::NotUtf8`] otherwise. This blocks binary secrets
//! (raw keymaterial, PFX blobs, encrypted tarballs). If we need those,
//! change [`Secret`] to wrap `Vec<u8>` and teach [`Redactor`] to substring-
//! match on bytes. Cost is a minor API churn across every crate that
//! currently calls `expose_secret()` and gets a `&String`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use displaydoc::Display;
use lusid_params::Secret;
use secrecy::{ExposeSecret, SecretBox};
use thiserror::Error;
use tokio::fs;

/// A single decryption identity — an x25519 secret key.
pub struct Identity {
    inner: age::x25519::Identity,
}

impl FromStr for Identity {
    type Err = IdentityError;

    /// Parse an identity from a string like `AGE-SECRET-KEY-1...`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let inner = age::x25519::Identity::from_str(s).map_err(IdentityError::Parse)?;
        Ok(Self { inner })
    }
}

impl Identity {
    /// Read an identity file from disk. The file must contain a single
    /// `AGE-SECRET-KEY-...` line (comments prefixed with `#` are stripped).
    pub async fn from_file(path: &Path) -> Result<Self, IdentityError> {
        let text = fs::read_to_string(path)
            .await
            .map_err(|source| IdentityError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| IdentityError::Empty {
                path: path.to_path_buf(),
            })?;
        line.parse()
    }
}

#[derive(Debug, Error, Display)]
pub enum IdentityError {
    /// Failed to read identity file {path}: {source}
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Identity file {path} has no key line
    Empty { path: PathBuf },

    /// Failed to parse identity: {0}
    Parse(&'static str),
}

/// A bundle of decrypted secrets, keyed by filename stem (e.g. the file
/// `secrets/api_key.age` becomes `api_key`).
///
/// `Secrets` owns its plaintexts via [`Secret`] (an `Arc<SecretBox<String>>`)
/// so `Debug` is redacted and the plaintext is zeroised when the last clone
/// is dropped.
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
        Redactor { secrets }
    }
}

/// Minimum plaintext length eligible for redaction. Shorter secrets are
/// skipped to avoid pathological false positives when substring-matching
/// against arbitrary process output.
pub const REDACT_MIN_LEN: usize = 8;

/// Placeholder string substituted in place of matched secret plaintext.
pub const REDACTED: &str = "<redacted>";

/// Substring-replaces secret plaintexts with [`REDACTED`] in arbitrary
/// strings. Intended for scrubbing `lusid-apply`'s per-operation stdout
/// and stderr lines before they are streamed to the TUI.
///
/// Limitations (read before trusting this for anything load-bearing):
///
/// - **Substring-only.** A secret that appears base64-encoded, escaped,
///   JSON-serialised, or chunked across multiple read boundaries will not
///   be caught. This is a best-effort scrub, not a guarantee.
/// - **Short secrets are skipped.** See [`REDACT_MIN_LEN`].
/// - **Emits plaintext briefly** via [`ExposeSecret`] during each call;
///   the plaintext is not copied but is borrowed for the length of one
///   `String::replace`.
#[derive(Clone)]
pub struct Redactor {
    secrets: Vec<Secret>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("len", &self.secrets.len())
            .finish()
    }
}

impl Redactor {
    /// No-op redactor (no secrets).
    pub fn empty() -> Self {
        Self {
            secrets: Vec::new(),
        }
    }

    /// Replace every occurrence of every registered secret plaintext in
    /// `input` with [`REDACTED`]. Returns `input` unchanged when no
    /// secrets match (including the trivial empty-redactor case).
    pub fn redact(&self, input: &str) -> String {
        if self.secrets.is_empty() || input.is_empty() {
            return input.to_string();
        }
        let mut out = input.to_string();
        for secret in &self.secrets {
            let plaintext = secret.expose_secret();
            if out.contains(plaintext.as_str()) {
                out = out.replace(plaintext.as_str(), REDACTED);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }
}

/// Decrypt every `*.age` file in `dir` with `identity`, returning a [`Secrets`]
/// keyed by filename stem.
///
/// Missing `dir` returns an empty [`Secrets`] — projects with no `secrets/`
/// directory should work unchanged. Non-`.age` files in `dir` are ignored.
#[tracing::instrument(skip(identity), fields(dir = %dir.display()))]
pub async fn decrypt_dir(identity: &Identity, dir: &Path) -> Result<Secrets, DecryptError> {
    if !fs::try_exists(dir)
        .await
        .map_err(|source| DecryptError::ScanDir {
            dir: dir.to_path_buf(),
            source,
        })?
    {
        tracing::debug!("secrets dir does not exist; returning empty Secrets");
        return Ok(Secrets::empty());
    }

    let mut values: HashMap<String, Secret> = HashMap::new();
    let mut read_dir = fs::read_dir(dir)
        .await
        .map_err(|source| DecryptError::ScanDir {
            dir: dir.to_path_buf(),
            source,
        })?;

    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|source| DecryptError::ScanDir {
            dir: dir.to_path_buf(),
            source,
        })?
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("age") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };

        let ciphertext = fs::read(&path)
            .await
            .map_err(|source| DecryptError::ReadFile {
                path: path.clone(),
                source,
            })?;

        let plaintext = decrypt_bytes(identity, &ciphertext).map_err(|source| match source {
            DecryptError::Decrypt { source, .. } => DecryptError::Decrypt {
                path: path.clone(),
                source,
            },
            DecryptError::DecryptIo { source, .. } => DecryptError::DecryptIo {
                path: path.clone(),
                source,
            },
            DecryptError::NotUtf8 { .. } => DecryptError::NotUtf8 { path: path.clone() },
            other => other,
        })?;

        values.insert(stem, plaintext);
    }

    tracing::info!(count = values.len(), "decrypted secrets");
    Ok(Secrets { values })
}

fn decrypt_bytes(identity: &Identity, ciphertext: &[u8]) -> Result<Secret, DecryptError> {
    let decryptor = age::Decryptor::new(ciphertext).map_err(|source| DecryptError::Decrypt {
        path: PathBuf::new(),
        source,
    })?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity.inner as &dyn age::Identity))
        .map_err(|source| DecryptError::Decrypt {
            path: PathBuf::new(),
            source,
        })?;

    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .map_err(|source| DecryptError::DecryptIo {
            path: PathBuf::new(),
            source,
        })?;

    let plaintext = String::from_utf8(plaintext).map_err(|_| DecryptError::NotUtf8 {
        path: PathBuf::new(),
    })?;
    Ok(Arc::new(SecretBox::new(Box::new(plaintext))))
}

#[derive(Debug, Error, Display)]
pub enum DecryptError {
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

    /// Failed to decrypt {path}: {source}
    Decrypt {
        path: PathBuf,
        #[source]
        source: age::DecryptError,
    },

    /// I/O error while decrypting {path}: {source}
    DecryptIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Decrypted bytes for {path} are not valid UTF-8
    NotUtf8 { path: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_of(s: &str) -> Secret {
        Arc::new(SecretBox::new(Box::new(s.to_string())))
    }

    fn secrets_from(pairs: &[(&str, &str)]) -> Secrets {
        let values = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), secret_of(v)))
            .collect();
        Secrets { values }
    }

    #[test]
    fn redactor_empty_is_noop() {
        let redactor = Redactor::empty();
        assert_eq!(redactor.redact("hello world"), "hello world");
        assert!(redactor.is_empty());
    }

    #[test]
    fn redactor_replaces_occurrences() {
        let secrets = secrets_from(&[("api_key", "supersecretvalue")]);
        let redactor = secrets.redactor();
        assert_eq!(
            redactor.redact("auth: supersecretvalue; retrying supersecretvalue"),
            "auth: <redacted>; retrying <redacted>"
        );
    }

    #[test]
    fn redactor_skips_short_secrets() {
        // Below REDACT_MIN_LEN (8) — skipped entirely to avoid false
        // positives on common short substrings.
        let secrets = secrets_from(&[("pin", "12345")]);
        let redactor = secrets.redactor();
        assert!(redactor.is_empty());
        assert_eq!(redactor.redact("pin is 12345"), "pin is 12345");
    }

    #[test]
    fn redactor_prefers_longer_patterns() {
        // Two secrets where one plaintext is a substring of the other:
        // longer-first ordering ensures the outer pattern is redacted as
        // a whole rather than leaving a fragment after the inner match.
        let secrets = secrets_from(&[("outer", "aaaaaaaabbbbbbbb"), ("inner", "aaaaaaaabb")]);
        let redactor = secrets.redactor();
        assert_eq!(
            redactor.redact("value=aaaaaaaabbbbbbbb done"),
            "value=<redacted> done"
        );
    }

    #[test]
    fn redactor_handles_empty_input() {
        let secrets = secrets_from(&[("k", "eightchars")]);
        let redactor = secrets.redactor();
        assert_eq!(redactor.redact(""), "");
    }

    #[test]
    fn redactor_no_match_returns_input_unchanged() {
        let secrets = secrets_from(&[("k", "eightchars")]);
        let redactor = secrets.redactor();
        assert_eq!(redactor.redact("nothing to see"), "nothing to see");
    }
}
