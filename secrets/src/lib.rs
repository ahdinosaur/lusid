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

mod check;
pub mod cli;
mod crypto;
mod identity;
mod key;
mod recipients;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use displaydoc::Display;
use secrecy::{ExposeSecret, SecretBox};
use thiserror::Error;
use tokio::fs;

pub use crate::check::{CheckError, CheckReport, DriftReason, DriftedFile, ReadError};
pub use crate::crypto::{
    DecryptError, EncryptError, HeaderError, decrypt_bytes, encrypt_bytes, read_header_stanzas,
};
pub use crate::identity::{Identity, IdentityError};
pub use crate::key::{Key, KeyParseError};
pub use crate::recipients::{
    FileEntry, Recipients, RecipientsError, ResolveError, ResolvedRecipient, SECRETS_FILE,
    to_boxed_recipients,
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

/// Decrypt every `*.age` file directly under `secrets_dir` with `identity`,
/// returning a [`Secrets`] keyed by filename stem.
///
/// Unlike [`decrypt_dir`], this does not consult `lusid-secrets.toml` — it
/// decrypts whatever ciphertexts happen to be in the directory. Used on
/// guest-mode applies (dev / remote re-encryption targets) where the host
/// has already filtered the set of files to exactly what this target should
/// see, and there's no `Recipients` config on the guest.
///
/// The asymmetry with [`decrypt_dir`] is deliberate: rather than have the
/// host synthesise a single-entry `lusid-secrets.toml` on the guest so it
/// could go through the same TOML-driven path, the guest just decrypts
/// whatever it was handed. Two small functions is simpler than one function
/// plus a synthetic-config generator.
///
/// Missing `secrets_dir` returns an empty [`Secrets`]. Non-`.age` files are
/// ignored.
#[tracing::instrument(skip(identity), fields(dir = %secrets_dir.display()))]
pub async fn decrypt_all(
    identity: &Identity,
    secrets_dir: &Path,
) -> Result<Secrets, DecryptDirError> {
    if !fs::try_exists(secrets_dir)
        .await
        .map_err(|source| DecryptDirError::ScanDir {
            dir: secrets_dir.to_path_buf(),
            source,
        })?
    {
        tracing::debug!("secrets dir does not exist; returning empty Secrets");
        return Ok(Secrets::empty());
    }

    let mut values: HashMap<String, Secret> = HashMap::new();
    let mut read_dir =
        fs::read_dir(secrets_dir)
            .await
            .map_err(|source| DecryptDirError::ScanDir {
                dir: secrets_dir.to_path_buf(),
                source,
            })?;

    while let Some(entry) =
        read_dir
            .next_entry()
            .await
            .map_err(|source| DecryptDirError::ScanDir {
                dir: secrets_dir.to_path_buf(),
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

        let plaintext = decrypt_bytes(identity, &path, &ciphertext)?;
        values.insert(stem, plaintext);
    }

    tracing::info!(count = values.len(), "decrypted secrets");
    Ok(Secrets { values })
}

/// A re-encrypted secret produced by [`reencrypt_for_machine`]: the file
/// stem (e.g. `api_token`) and the new age ciphertext encrypted to the
/// target's key. Callers typically write each back as
/// `<remote_secrets_dir>/<stem>.age` on the target.
#[derive(Debug, Clone)]
pub struct ReencryptedSecret {
    pub stem: String,
    pub ciphertext: Vec<u8>,
}

/// Decrypt every `*.age` under `secrets_dir` with `host_identity`, then
/// re-encrypt each plaintext to `machine_key` alone and return the resulting
/// ciphertexts.
///
/// Host side of per-target re-encryption: `dev apply` / `remote apply`
/// uses this to produce a bundle of ciphertexts decryptable only by the
/// target's key, ships them over SSH, and points the guest's `lusid-apply`
/// at them via `--secrets-dir` + `--guest-mode`.
///
/// Assumes `host_identity` is a recipient on every `*.age` under
/// `secrets_dir` (this uses [`decrypt_all`], not `files_for_alias` filtering).
/// In a multi-operator world where the operator running `dev apply` only has
/// access to a subset of files, `decrypt_all` will fail on the first file
/// they can't decrypt. For v1's single-operator-per-project flow this is
/// fine; revisit when operators-with-scoped-access becomes a real use case.
///
/// Plaintexts live only inside the intermediate [`Secrets`] and are zeroised
/// when it drops at function return. The operator identity never leaves the
/// host.
#[tracing::instrument(skip(host_identity, machine_key), fields(dir = %secrets_dir.display()))]
pub async fn reencrypt_for_machine(
    host_identity: &Identity,
    secrets_dir: &Path,
    machine_key: &Key,
) -> Result<Vec<ReencryptedSecret>, ReencryptForMachineError> {
    let secrets = decrypt_all(host_identity, secrets_dir).await?;
    let recipients: Vec<Box<dyn age::Recipient + Send>> = match machine_key {
        Key::X25519(k) => vec![Box::new(k.clone())],
        Key::Ssh(k) => vec![Box::new(k.clone())],
    };

    let mut out = Vec::with_capacity(secrets.len());
    for (stem, secret) in secrets.iter() {
        // `path` is only used for error labelling inside encrypt_bytes. A
        // virtual `<stem>.age` keeps diagnostics meaningful without a
        // filesystem round-trip.
        let virtual_path = Path::new(stem);
        let ciphertext =
            encrypt_bytes(&recipients, virtual_path, secret.expose_secret().as_bytes())?;
        out.push(ReencryptedSecret {
            stem: stem.to_owned(),
            ciphertext,
        });
    }

    tracing::info!(count = out.len(), "re-encrypted secrets for machine");
    Ok(out)
}

#[derive(Debug, Error, Display)]
pub enum ReencryptForMachineError {
    /// {0}
    DecryptDir(#[from] DecryptDirError),

    /// {0}
    Encrypt(#[from] EncryptError),
}

/// Find the alias in `[operators]` or `[machines]` whose key matches
/// `identity`. Implemented as an encrypt-then-decrypt round-trip so it works
/// uniformly across x25519 and SSH without leaking the identity's public
/// material out of the `age` crate.
///
/// Cost is `O(N)` encryptions plus one decryption per table entry until a
/// match is found — `age::Identity` doesn't expose a public-key accessor
/// that's uniform across x25519 and SSH, so the probe is the pragmatic
/// option. Fine for typical team / fleet sizes; worth revisiting if
/// `lusid-secrets.toml` ever grows to hundreds of entries.
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

    #[tokio::test]
    async fn decrypt_all_reads_every_age_file() {
        let id = age::x25519::Identity::generate();
        let identity: Identity = id.to_string().expose_secret().parse().unwrap();

        let dir = TempDir::new().unwrap();
        for (stem, value) in &[("a", b"aaa" as &[u8]), ("b", b"bbb")] {
            let ct = encrypt_bytes(&[Box::new(id.to_public())], Path::new(stem), value).unwrap();
            std::fs::write(dir.path().join(format!("{stem}.age")), &ct).unwrap();
        }
        // Non-.age entries alongside the ciphertexts are ignored.
        std::fs::write(dir.path().join("lusid-secrets.toml"), "# ignored").unwrap();

        let secrets = decrypt_all(&identity, dir.path()).await.unwrap();
        assert_eq!(secrets.len(), 2);
        assert_eq!(secrets.get("a").unwrap().expose_secret().as_str(), "aaa");
        assert_eq!(secrets.get("b").unwrap().expose_secret().as_str(), "bbb");
    }

    #[tokio::test]
    async fn decrypt_all_missing_dir_returns_empty() {
        let id = age::x25519::Identity::generate();
        let identity: Identity = id.to_string().expose_secret().parse().unwrap();
        let secrets = decrypt_all(&identity, Path::new("/nonexistent-lusid-dir"))
            .await
            .unwrap();
        assert!(secrets.is_empty());
    }

    #[tokio::test]
    async fn reencrypt_for_machine_round_trips() {
        // Host identity encrypts; target identity is separate. The function
        // re-encrypts each `*.age` so only the target can decrypt it.
        let host_age = age::x25519::Identity::generate();
        let host_identity: Identity = host_age.to_string().expose_secret().parse().unwrap();
        let target_age = age::x25519::Identity::generate();
        let target_identity: Identity = target_age.to_string().expose_secret().parse().unwrap();
        let machine_key = Key::X25519(target_age.to_public());

        let dir = TempDir::new().unwrap();
        for (stem, value) in &[("alpha", b"alphaplain" as &[u8]), ("beta", b"betaplain")] {
            let ct =
                encrypt_bytes(&[Box::new(host_age.to_public())], Path::new(stem), value).unwrap();
            std::fs::write(dir.path().join(format!("{stem}.age")), &ct).unwrap();
        }

        let reencrypted = reencrypt_for_machine(&host_identity, dir.path(), &machine_key)
            .await
            .unwrap();
        assert_eq!(reencrypted.len(), 2);

        let mut by_stem: std::collections::HashMap<&str, &Vec<u8>> = reencrypted
            .iter()
            .map(|r| (r.stem.as_str(), &r.ciphertext))
            .collect();
        let alpha_ct = by_stem.remove("alpha").unwrap();
        let beta_ct = by_stem.remove("beta").unwrap();

        let alpha_pt = decrypt_bytes(&target_identity, Path::new("alpha"), alpha_ct).unwrap();
        let beta_pt = decrypt_bytes(&target_identity, Path::new("beta"), beta_ct).unwrap();
        assert_eq!(alpha_pt.expose_secret().as_str(), "alphaplain");
        assert_eq!(beta_pt.expose_secret().as_str(), "betaplain");

        // Host identity can no longer decrypt the re-encrypted payload —
        // only the target recipient is on the ciphertext.
        assert!(decrypt_bytes(&host_identity, Path::new("alpha"), alpha_ct).is_err());
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

    fn secret_of(s: &str) -> Secret {
        Arc::new(SecretBox::new(Box::new(s.to_string())))
    }

    fn secrets_from(pairs: &[(&str, &str)]) -> Secrets {
        let values = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), secret_of(v)))
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
