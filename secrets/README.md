# lusid-secrets

Age-encrypted secrets for `.lusid` plans. Each project stores secrets as
individual `*.age` files under `secrets/`, with a `recipients.toml` mapping
each file stem to the keys that can decrypt it. At apply time every file is
decrypted up-front using the operator's [`Identity`](src/identity.rs) and
the plaintexts are exposed to plans via `ctx.secrets.<stem>`.

## Usage

```rust
use lusid_secrets::{Identity, decrypt_dir};

let identity = Identity::from_file(&identity_path).await?;
let secrets = decrypt_dir(&identity, &secrets_dir).await?;
// → pass to lusid_plan::plan(..., &secrets)
```

## Primary types

- [`Identity`](src/identity.rs) — decryption identity. Either an
  `AGE-SECRET-KEY-1...` x25519 key (operator) or an OpenSSH
  ed25519 / RSA private key (peer; e.g. `/etc/ssh/ssh_host_ed25519_key`).
  Passphrase-protected SSH keys are rejected.
- [`Recipients`](src/recipients.rs) — parsed `recipients.toml`. `[keys]`
  maps an alias to an age x25519 or SSH pubkey; `[groups]` names alias
  lists; `[files]` maps each file stem to a recipient list that may
  reference either a bare alias or `@group`.
- [`Secrets`](src/lib.rs) — map of `stem → Secret`, built by
  [`decrypt_dir`](src/lib.rs).
- [`Secret`](../params/src/lib.rs) — `Arc<SecretBox<String>>`, cheap to
  clone, redacted on `Debug`, zeroised on final drop.
- [`Redactor`](src/lib.rs) — built from a `Secrets`; substring-replaces
  every eligible plaintext with `"<redacted>"`. Used by `lusid-apply` to
  scrub per-operation stdout/stderr before streaming to the TUI.

## CLI (`lusid secrets ...`)

Implemented in [`cli.rs`](src/cli.rs); dispatched from the `lusid` wrapper.

| Command                                | What it does                                                    |
| -------------------------------------- | --------------------------------------------------------------- |
| `lusid secrets ls`                     | List every file stem and its declared recipients.               |
| `lusid secrets edit <name>`            | Decrypt → `$EDITOR` (default `vi`) → encrypt on save.           |
| `lusid secrets rekey [<name>]`         | Re-encrypt to the current recipients; no-op when header matches.|
| `lusid secrets keygen [-o <path>]`     | Generate an x25519 identity; default path `~/.config/lusid/identity`. |
| `lusid secrets check`                  | Audit: orphans / missing / drift; non-zero exit on findings.    |
| `lusid secrets cat <name>`             | Decrypt and print plaintext to stdout.                          |

`edit`, `rekey`, and `cat` require `--identity` (or `LUSID_IDENTITY`, or
`identity` in `lusid.toml`). `ls`, `check`, and `keygen` don't.

## `recipients.toml` shape

```toml
[keys]
mikey     = "age1..."                              # operator x25519 key
rpi4b-1   = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5..."  # peer SSH host key

[groups]
operators = ["mikey"]
all-pis   = ["rpi4b-1"]

[files]
"api_token"       = { recipients = ["@operators", "rpi4b-1"] }
"db_admin_pw"     = { recipients = ["@operators"] }
```

Every `*.age` file must have a matching `[files]` entry; adding a new
file means adding an entry first.

## Scope (v2)

- **Operator + peer keys** coexist on the same file. The `age` crate's
  `ssh` feature handles both via `age::Recipient` / `age::Identity` trait
  objects.
- **Local apply only.** `dev apply` / `remote apply` do not currently
  forward secrets to the target. See the crate-level module doc for the
  three candidate strategies (ship identity / decrypt-on-host / per-target
  re-encryption).
- **Eager decryption.** Every file is decrypted at apply start even if no
  plan reads it — keeps the [`Redactor`]'s table complete.
- **UTF-8 plaintext only.** Binary secrets (keymaterial blobs, PFX, etc.)
  are rejected at decrypt.
- **Missing secret = `Null`.** `ctx.secrets.<typo>` silently evaluates to
  `Null` rather than erroring; see `Note(cc)` in `plan/src/eval.rs`.
- **Redaction is best-effort.** Substring-only; skips secrets below
  [`REDACT_MIN_LEN`](src/lib.rs) (8 bytes) to avoid pathological false
  positives.

Non-goals for v2: passphrase-protected identities, age plugins
(YubiKey/TPM/1Password), per-target re-encryption at apply time, and
binary secrets.
