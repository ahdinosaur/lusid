//! Age-encrypted secrets for lusid plans.
//!
//! A lusid project stores secrets as individual `*.age` files under a
//! `secrets/` directory, alongside a `recipients.toml` mapping each file
//! stem to the keys that can decrypt it. At apply time the host's
//! [`Identity`] decrypts every file up-front and hands the plaintexts to
//! plans via the `ctx.secrets` Rimu object (see `lusid-plan`).
//!
//! # v2 at a glance
//!
//! - **Two key kinds on the same file.** An age x25519 operator key and an
//!   SSH ed25519 / RSA peer key can both appear in a file's recipient list.
//!   The `age` crate's `ssh` feature handles both as `age::Recipient` /
//!   `age::Identity` trait objects; see [`identity`] and [`recipients`].
//! - **`recipients.toml` is the source of truth.** Parsed by
//!   [`recipients::Recipients`]; file entries can reference either a bare
//!   alias from `[operators]`/`[machines]` or a group (`@name`) from
//!   `[groups]`. Operators decrypt on the host (x25519 identity); machines
//!   are targets keyed by `machine_id` whose SSH host key is a recipient on
//!   exactly the secrets they need.
//! - **CLI lives here.** `lusid secrets {ls, edit, rekey, keygen, check, cat}`
//!   is implemented in [`cli`] and dispatched from the `lusid` wrapper.
//! - **Eager decryption at apply.** [`decrypt_dir`] decrypts every `*.age`
//!   file in the project's `secrets/` directory, regardless of which secrets
//!   a plan happens to read. Keeps the [`Redactor`] table complete so
//!   substring-scrubbing of process output cannot miss a secret that was
//!   forwarded through an operation we didn't anticipate.
//!
//! # Remote / dev apply (`TODO(cc)`)
//!
//! `lusid-apply` still runs locally only today. For `cmd_dev_apply`
//! (libvirt VM) and `cmd_remote_apply` (SSH) the identity + secrets dir
//! live on the host, not the target. Three options, none implemented:
//!
//! 1. **Ship the identity** to the target and decrypt there. Simple but
//!    widens the trust radius.
//! 2. **Decrypt on the host, ship plaintext** over the apply stdio pipe.
//!    Trust stays local; plaintext briefly on two machines.
//! 3. **Re-encrypt per target**: each target's SSH host key is a recipient
//!    on exactly the secrets it needs; host re-encrypts before shipping.
//!    Best security, most key management. v2 already lays the ground by
//!    supporting peer SSH keys as recipients.
//!
//! Option 2 is the likely first cut. Until one is picked, `cmd_dev_apply`
//! errors with `AppError::SecretsNotYetSupported` when the project has
//! secrets configured (see `lusid/src/lib.rs`).
//!
//! # UTF-8 plaintext only (`Note(cc)`)
//!
//! [`decrypt_dir`] decodes every decrypted payload as UTF-8 and errors
//! with [`DecryptError::NotUtf8`] otherwise. This blocks binary secrets
//! (raw keymaterial, PFX blobs, encrypted tarballs). If we need those,
//! change [`Secret`] to wrap `Vec<u8>` and teach [`Redactor`] to substring-
//! match on bytes. Cost is a minor API churn across every crate that
//! currently calls `expose_secret()` and gets a `&String`.

mod check;
pub mod cli;
mod crypto;
mod identity;
mod recipients;

use std::collections::HashMap;
use std::path::Path;

use displaydoc::Display;
use lusid_params::Secret;
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::fs;

pub use crate::check::{CheckError, CheckReport, DriftReason, DriftedFile, ReadError};
pub use crate::crypto::{DecryptError, EncryptError, HeaderError};
pub use crate::identity::{Identity, IdentityError};
pub use crate::recipients::{
    FileEntry, Key, KeyParseError, RECIPIENTS_FILE, Recipients, RecipientsError, ResolveError,
    ResolvedRecipient,
};

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
/// - **Overlapping/adjacent secrets are not reliably caught.** Longest-first
///   ordering handles the nested case (secret B is a substring of secret A)
///   but not the interleaved case: if A = "foobar" and B = "barfoo" both
///   appear in "foobarfoo", only one of them will redact, leaving the
///   other's plaintext visible. In practice this would need two secrets
///   that share a suffix/prefix by coincidence; flagging anyway.
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
/// directory should work unchanged. Non-`.age` files in `dir` are ignored
/// (that's where `recipients.toml` lives).
#[tracing::instrument(skip(identity), fields(dir = %dir.display()))]
pub async fn decrypt_dir(identity: &Identity, dir: &Path) -> Result<Secrets, DecryptDirError> {
    if !fs::try_exists(dir)
        .await
        .map_err(|source| DecryptDirError::ScanDir {
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
        .map_err(|source| DecryptDirError::ScanDir {
            dir: dir.to_path_buf(),
            source,
        })?;

    while let Some(entry) =
        read_dir
            .next_entry()
            .await
            .map_err(|source| DecryptDirError::ScanDir {
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
            .map_err(|source| DecryptDirError::ReadFile {
                path: path.clone(),
                source,
            })?;

        let plaintext = crypto::decrypt_bytes(identity, &path, &ciphertext)?;

        values.insert(stem, plaintext);
    }

    tracing::info!(count = values.len(), "decrypted secrets");
    Ok(Secrets { values })
}

/// Errors from [`decrypt_dir`]: directory-scan I/O or per-file decryption
/// failures. Individual file errors come straight from [`DecryptError`].
#[derive(Debug, Error, Display)]
pub enum DecryptDirError {
    /// Failed to scan secrets dir {dir}: {source}
    ScanDir {
        dir: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to read encrypted file {path}: {source}
    ReadFile {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// {0}
    Decrypt(#[from] DecryptError),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use secrecy::SecretBox;

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
