# lusid-secrets

Age-encrypted project secrets for lusid plans. Agenix-style flow: secrets
live as ciphertext in-repo, the operator's identity decrypts them at apply
time, and plaintext only ever reaches the target filesystem through
`@core/secret`'s atomic write.

## Flow

```
                              lusid-secrets.toml
                              [operators] [machines] [groups] [files]
                                     |
                                     v
 host identity  -->  Secrets::load(secrets_dir, identity_path, guest_mode)
 (x25519 / ssh)                     |
                                    v
                    Secrets { HashMap<String, SecretBox<String>> }
                                    |
                                    v
                          Context::set_secrets(...)
                                    |
                                    v
            plan refers to secret by name  -->  @core/secret { name, path, ... }
                                    |
                                    v
                    FileSource::Secret(name), atomic-write
```

A `Redactor` hangs off the same `Secrets` bundle; every per-operation
stdout/stderr line is substring-scrubbed before emit.

## Data model

Project layout:

```
<root>/
  secrets/
    lusid-secrets.toml
    api_token.age
    db_password.age
```

`lusid-secrets.toml`:

```toml
[operators]
mikey = "age1..."                     # x25519 public key

[machines]
rpi4b-1 = "ssh-ed25519 AAAA..."       # SSH host key

[groups]
admins = ["mikey"]

[files]
"api_token"   = { recipients = ["@admins", "rpi4b-1"] }
"db_password" = { recipients = ["mikey", "rpi4b-1"] }
```

- Operators and machines share one alias namespace; collisions are rejected
  at load.
- `@name` expands via `[groups]`; expansion is shallow (no nested groups).
- Unknown aliases, unknown groups, and empty recipient lists are hard errors.

Identities come in two shapes:

- `AGE-SECRET-KEY-1...` — age x25519.
- `-----BEGIN OPENSSH PRIVATE KEY-----` — OpenSSH ed25519 or RSA. Passphrase-
  protected keys are rejected up-front (prompting at apply time is out of scope).

## Plan integration

Plans refer to secrets by name via `@core/secret`:

```rimu
- module: "@core/secret"
  params:
    name: "api_token"          # -> secrets/api_token.age
    path: "/etc/myapp/token"
    mode: 384                   # optional; default 0o600
    user: "myapp"               # optional
    group: "myapp"              # optional
```

`@core/secret` delegates to `@core/file`'s state/change/operation machinery,
adding a `FileSource::Secret(name)` variant that resolves against
`ctx.secrets()` inside the apply-time operation. The plaintext copy lives
only for the duration of one atomic write. Plans never see plaintext —
`ctx.secrets` is not exposed to Rimu.

## Apply-time decryption

`Secrets::load(secrets_dir, identity_path, guest_mode)` is the single entry
point. Its behaviour depends on the two flags:

**Host mode** (`identity_path = Some`, `guest_mode = false`) — the normal
`lusid-apply` path: reads `lusid-secrets.toml`, matches the identity's public
key against `[operators]` / `[machines]` (no match is a hard error), and
decrypts only the files the matched alias is declared as a recipient for.

**Guest mode** (`identity_path = Some`, `guest_mode = true`) — used by `dev
apply` / `remote apply` targets: skips `lusid-secrets.toml` and decrypts
every `*.age` under `secrets_dir` with the supplied identity. The host has
already filtered the bundle to exactly what this guest should see via
per-target re-encryption, so no Recipients config is needed on the guest.

**No identity** (`identity_path = None`, `guest_mode = false`) — returns an
empty bundle. Plans referencing `@core/secret` will fail later with a
missing-secret error.

Callers then wrap the result with `secrets.redactor()` (for per-operation
output scrubbing) and hand the bundle to `ctx.set_secrets(...)` before
planning.

## Per-target re-encryption

`reencrypt_for_machine(host_identity_path, secrets_dir, machine_pubkey)`
decrypts every `*.age` on the host, re-encrypts each plaintext to
`machine_pubkey` alone, and returns the single-recipient ciphertexts.
Callers SFTP the bundle to the guest and run `lusid-apply --guest-mode
--identity=<guest identity>` there. `machine_pubkey` is either an `age1...`
x25519 recipient or an `ssh-ed25519 ...` / `ssh-rsa ...` SSH public key.

- **Operator identity never leaves the host.** The guest only ever holds
  ciphertext encrypted to its own key, plus the identity file it decrypts
  them with.
- `dev apply` (VM targets): reuses the ephemeral SSH keypair lusid uses for
  SSH auth as both the age recipient (host side) and the guest identity
  (guest side).
- `remote apply`: recipient is the target's entry in `[machines]` looked up
  by `machine_id`; the guest identity is the target's existing
  `/etc/ssh/ssh_host_ed25519_key`.

This assumes the operator running `dev apply` / `remote apply` is a recipient
on every `*.age` under `secrets_dir`. In a multi-operator world with
scoped access, the intermediate decrypt step will fail on the first file the
acting operator can't read. Fine for v1's one-operator-per-project flow;
revisit when scoped access is a real use case.

## Redactor

`Secrets::redactor()` builds a `Redactor` that substring-replaces every
decrypted plaintext with `<redacted>`. `lusid-apply` wraps every per-operation
stdout/stderr line through it before streaming to the TUI.

Limitations, read before trusting:

- Substring-only. Secrets that appear base64-encoded, JSON-escaped, or split
  across read boundaries are not caught.
- Secrets shorter than `REDACT_MIN_LEN` (8 bytes) are skipped, to avoid false
  positives on common short sequences.
- Longest-first ordering handles nested matches (one secret is a substring of
  another) but not the interleaved case where two secrets share a suffix
  with a prefix.

## CLI

`lusid secrets <subcommand>`:

| Command        | Needs identity | Action                                                                      |
| -------------- | :------------: | --------------------------------------------------------------------------- |
| `ls`           | no             | List `*.age` files and their declared recipients.                           |
| `cat <name>`   | yes            | Decrypt to stdout.                                                          |
| `edit <name>`  | yes            | Decrypt into a mode-0600 tmpfile in `$XDG_RUNTIME_DIR`, `$EDITOR`, re-encrypt on save. Tmpfile is scrubbed even on editor failure. |
| `rekey [name]` | yes            | Re-encrypt to the current recipient list. No-op when the header already matches. Without `<name>`, rekeys every `[files]` entry. |
| `keygen [-o]`  | no             | Generate an x25519 identity at `$XDG_CONFIG_HOME/lusid/identity` (or `$HOME/.config/lusid/identity`). Refuses to overwrite. |
| `check`        | no             | Audit `secrets/` against `lusid-secrets.toml`: orphan ciphertexts, missing ciphertexts, recipient drift. Non-zero exit on any finding; suits CI. |

## Invariants

- **Plans never see plaintext.** `@core/secret` is the only path from
  ciphertext to filesystem.
- **Plaintext lives in memory only during apply.** Wrapped in
  `SecretBox<String>` (redacted `Debug`, zeroised on drop); the target
  filesystem is the only disk location plaintext reaches, via an atomic write.
- **Selective decryption in host mode.** Only the files the alias is a
  recipient for are opened. Guest mode relies on upstream filtering.
- **UTF-8 only.** Non-UTF-8 payloads error loudly at decrypt
  (`DecryptError::NotUtf8`). Binary support is a later change.
- **Missing secrets are fatal**, not a silent empty file — both at state-probe
  time (`FileStateError::MissingSecret`) and apply time
  (`FileApplyError::MissingSecret`).
