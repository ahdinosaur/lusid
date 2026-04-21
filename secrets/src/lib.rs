//! Age-encrypted secrets for lusid plans.
//!
//! A lusid project stores secrets as individual `*.age` files under a
//! `secrets/` directory. At apply time the host's [`Identity`] decrypts
//! these files and hands the plaintexts to `@core/secret` resources by name.
//! Plaintexts never enter the Rimu evaluator — plans reference secrets by
//! name, contents materialise at apply.
//!
//! This crate currently provides the primitives plus the
//! `lusid-secrets.toml` recipients model. Higher-level wiring (apply-time
//! `Secrets` bundle, CLI) lands in subsequent phases.

mod crypto;
mod identity;
mod key;
mod recipients;

use std::sync::Arc;

use secrecy::SecretBox;

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
