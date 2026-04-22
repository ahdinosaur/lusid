//! Host-side re-encryption of a secrets directory for a single target machine.

use std::path::Path;

use secrecy::ExposeSecret;
use thiserror::Error;

use crate::crypto::{EncryptError, encrypt_bytes};
use crate::decrypt_all::{DecryptAllError, decrypt_all};
use crate::identity::Identity;
use crate::key::Key;

/// A re-encrypted secret produced by [`reencrypt_for_machine`]: the file
/// stem (e.g. `api_token`) and the new age ciphertext encrypted to the
/// target's key. Callers typically write each back as
/// `<remote_secrets_dir>/<stem>.age` on the target.
#[derive(Debug, Clone)]
pub struct ReencryptedSecret {
    pub stem: String,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum ReencryptForMachineError {
    #[error(transparent)]
    DecryptAll(#[from] DecryptAllError),

    #[error(transparent)]
    Encrypt(#[from] EncryptError),
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
/// Plaintexts live only inside the intermediate [`crate::Secrets`] and are
/// zeroised when it drops at function return. The operator identity never
/// leaves the host.
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use secrecy::ExposeSecret;
    use tempfile::TempDir;

    use super::*;
    use crate::crypto::{decrypt_bytes, encrypt_bytes};

    #[tokio::test]
    async fn round_trips() {
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
}
