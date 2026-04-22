//! Age-encrypted secrets for lusid plans.
//!
//! A lusid project stores secrets as individual `*.age` files under a
//! `secrets/` directory. At apply time the host's [`Identity`] decrypts the
//! subset of files it's a recipient for and hands the plaintexts to
//! `@core/secret` resources by name. Plaintexts never enter the Rimu
//! evaluator — plans reference secrets by name, contents materialise at
//! apply.
//!
//! This crate provides the primitives ([`Identity`], [`Key`]), the
//! `lusid-secrets.toml` [`Recipients`] model, the apply-time [`Secrets`]
//! bundle plus [`decrypt_dir`] / [`decrypt_all`] / [`alias_for_identity`] /
//! [`reencrypt_for_machine`], and the `lusid secrets ...` CLI ([`cli`]).

mod alias;
mod check;
pub mod cli;
mod crypto;
mod decrypt_all;
mod decrypt_dir;
mod identity;
mod key;
mod recipients;
mod redactor;
mod reencrypt;
mod secrets;

pub use crate::alias::alias_for_identity;
pub use crate::check::CheckError;
pub use crate::crypto::{DecryptError, EncryptError, HeaderError};
pub use crate::decrypt_all::{DecryptAllError, decrypt_all};
pub use crate::decrypt_dir::{DecryptDirError, decrypt_dir};
pub use crate::identity::{Identity, IdentityError};
pub use crate::key::{Key, KeyParseError};
pub use crate::recipients::{FileEntry, Recipients, RecipientsError, ResolveError};
pub use crate::redactor::Redactor;
pub use crate::reencrypt::{ReencryptForMachineError, ReencryptedSecret, reencrypt_for_machine};
pub use crate::secrets::{Secret, Secrets};

use thiserror::Error;

/// Combined error across every fallible [`lusid-secrets`](crate) operation —
/// mirrors [`SshError`](../lusid_ssh/enum.SshError.html). Callers that want
/// to bubble up any failure from the crate in a single enum can use this;
/// callers that only care about a specific entry point can match on the
/// per-function error type directly.
#[derive(Debug, Error)]
pub enum SecretsError {
    #[error(transparent)]
    Identity(#[from] IdentityError),

    #[error(transparent)]
    Key(#[from] KeyParseError),

    #[error(transparent)]
    Recipients(#[from] RecipientsError),

    #[error(transparent)]
    DecryptDir(#[from] DecryptDirError),

    #[error(transparent)]
    DecryptAll(#[from] DecryptAllError),

    #[error(transparent)]
    ReencryptForMachine(#[from] ReencryptForMachineError),
}
