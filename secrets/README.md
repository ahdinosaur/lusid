# lusid-secrets

Age-encrypted secrets for `.lusid` plans. Decrypts every `*.age` file
under the project's `secrets/` directory up-front using a single
project-scoped identity, and hands the plaintexts to plans via
`ctx.secrets.<stem>`.

## Usage

```rust
use lusid_secrets::{Identity, decrypt_dir};

let identity = Identity::from_file(&identity_path).await?;
let secrets = decrypt_dir(&identity, &secrets_dir).await?;
// → pass to lusid_plan::plan(..., &secrets)
```

The primary types:

- [`Identity`](src/lib.rs) — an x25519 age secret key, loaded from a
  string or from a one-line file.
- [`Secrets`](src/lib.rs) — map of `name → Secret`, keyed by filename
  stem (`api_key.age` → `api_key`).
- [`Secret`](../params/src/lib.rs) — `Arc<SecretBox<String>>`, cheap
  to clone, redacted on `Debug`, zeroised on final drop.
- [`Redactor`](src/lib.rs) — built from a `Secrets`, substring-replaces
  every eligible plaintext with `"<redacted>"`. Used by `lusid-apply` to
  scrub per-operation stdout/stderr before streaming to the TUI.

## Scope (v1)

- **Local apply only.** `dev apply` and `remote apply` do not currently
  forward secrets to the target. See the crate-level module doc for the
  three candidate strategies (ship identity / decrypt-on-host / per-target
  recipient).
- **x25519 identities only** — no passphrase-wrapped keys yet.
- **Flat directory.** A single `secrets/` dir of `*.age` files; no nested
  namespaces.
- **Eager decryption.** Every file is decrypted at load even if no plan
  reads it — keeps the [`Redactor`]'s table complete.
- **Missing secret = `Null`.** `ctx.secrets.<typo>` is `Null`, not an
  error. Typos propagate silently today; see the `Note(cc)` in
  `plan/src/eval.rs`.
- **Redaction is best-effort.** Substring-only; skips secrets below
  [`REDACT_MIN_LEN`](src/lib.rs) (8 bytes) to avoid pathological false
  positives.
