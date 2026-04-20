# Secrets — desired state

This document describes where `lusid-secrets` is going. The current
crate-level module doc (`src/lib.rs`) describes v1 (what exists today);
this doc describes v2 (what we want to build). See the migration section
at the end for how to get from here to there.

## Problem

Today lusid uses age-encrypted `*.age` files in a project's
`secrets/` directory, decrypted at apply time with a single x25519
identity. In the wormfarm project these files are authored, recipient-
listed, and rekeyed by [agenix](https://github.com/ryantm/agenix) — a
Nix-based tool. The wormfarm project is dropping Nix, so we need lusid
to own the entire secrets workflow end-to-end without depending on Nix.

Additionally, lusid's decryption identity is currently x25519-only
(`AGE-SECRET-KEY-...`), which doesn't match the wormfarm recipients —
those are the **SSH ed25519 host keys** of each server, chosen
deliberately so that each machine can decrypt its own secrets using the
SSH host key it already has.

## Model

Two kinds of public key can be a recipient:

- **Peer keys** — the SSH ed25519 host key of a target machine. A peer
  can decrypt secrets addressed to it using its existing
  `/etc/ssh/ssh_host_ed25519_key`, no separate age key material needed.
- **Operator keys** — an x25519 age public key held by a human operator
  on their dev machine. Operators need to decrypt every secret they
  might ever edit, so they appear on every file's recipient list.

These coexist on the same file. The `age` crate's `ssh` feature handles
both as `age::Recipient` / `age::Identity` trait objects, so the
encryption/decryption code doesn't need to care which kind is which.

## Config file

Recipients live in `<project>/secrets/recipients.toml`:

```toml
# Human-readable aliases. SSH keys start with "ssh-ed25519 " /
# "ssh-rsa "; age keys start with "age1".
[keys]
mikey     = "age1xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
rpi4b-1   = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA..."
rpi4b-2   = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA..."

# Named groups, expanded in `files` entries as "@name".
[groups]
operators = ["mikey"]
all-pis   = ["rpi4b-1", "rpi4b-2"]

# One entry per .age file; maps stem → recipient list. Entries in
# `recipients` are either key aliases (`mikey`) or group references
# (`@operators`). Duplicates from expansion are de-duplicated.
[files]
"cloudflare_dns_api_token" = { recipients = ["@operators", "rpi4b-1"] }
"grafana_admin_password"   = { recipients = ["@operators", "rpi4b-1"] }
```

File keys are the **stem** (no `.age` suffix) to match the
`ctx.secrets.<name>` convention. Recipients listed by alias so that
adding a new machine is one edit to `[keys]` plus inclusion in the
relevant groups, not a hunt-and-replace across every file entry.

No `[files]` entry ⇒ the `.age` file is orphaned; `lusid secrets
check` flags this (see CLI below). Encrypting a new file requires
adding an entry first — the mechanism refuses to guess recipients.

## Identity file

`lusid-apply --identity <path>` and `lusid.toml: identity = ...` accept
a file whose first non-comment non-blank line is **one of**:

- `AGE-SECRET-KEY-1...` — an x25519 age identity (today's behaviour).
- `-----BEGIN OPENSSH PRIVATE KEY-----` (multi-line) — an SSH
  ed25519 or RSA private key. Passphrase-protected SSH keys are
  **not** accepted in v2 (would require prompting; see future work).

This covers the two main flows:

- **Operator running `lusid-apply` locally on their dev box**: point
  `--identity` at `~/.config/lusid/identity` (x25519, generated with
  `lusid secrets keygen`).
- **Peer running `lusid-apply` on the target** (for `remote apply`
  once that lands): point `--identity` at
  `/etc/ssh/ssh_host_ed25519_key`. The peer can decrypt every secret
  whose recipients include its SSH host key.

Multiple identities in one file are **not** supported in v2 — the
natural use case (operator carrying several keys) can be satisfied by
listing their multiple pubkeys in `[keys]` and pointing `--identity`
at whichever matches the machine they're on. Revisit if this turns
out to be cumbersome.

## CLI

New subcommand group under the `lusid` binary:

```
lusid secrets ls                        # list .age files + recipients
lusid secrets edit <name>               # open $EDITOR; encrypt on save
lusid secrets rekey [<name>]            # re-encrypt to current recipients
lusid secrets keygen [-o <path>]        # generate an x25519 identity
lusid secrets check                     # recipient drift / orphan files
lusid secrets cat <name>                # print plaintext to stdout (rare)
```

Specifically not included in v2:

- No `lusid secrets encrypt` reading stdin — prefer `edit` (auditable
  via `$EDITOR` history) and explicit file writes.
- No automatic key rotation — call `rekey` after editing
  `recipients.toml` by hand.
- No per-file key generation — every file shares the project-level
  recipient set.

### `edit`

1. If the file exists and an identity is available: decrypt into a
   tmpfile in `$XDG_RUNTIME_DIR` (fallback `/tmp` with mode 0600).
   Otherwise start from empty.
2. Exec `$EDITOR` (default `vi`) on the tmpfile. Wait.
3. On save: read the tmpfile, encrypt to the file's recipient list
   from `recipients.toml`, write atomically via rename.
4. Zero + unlink the tmpfile.

Editors adding trailing newlines is a real trap for single-value
secrets. `edit` prints a one-line warning after save if the plaintext
ends in a newline, suggesting the user strip it (vim: `:set binary
noeol`). Does not strip automatically — some secrets legitimately want
trailing newlines.

### `rekey`

For every `*.age` file listed in `recipients.toml`:

1. Decrypt with the configured identity.
2. Re-encrypt to the current recipient list.
3. Write atomically.

No-op when the ciphertext header already matches the intended
recipients (age ciphertexts include stanza headers; compare before
re-encrypting). Without the no-op check, every `rekey` would change
every `.age` file and make git history noisy.

### `check`

Reports:

- Files in `secrets/` with no entry in `recipients.toml`.
- Entries in `recipients.toml` with no `.age` file.
- Unknown key aliases or groups referenced from `[files]`.
- Files whose ciphertext header's recipient list doesn't match the
  current `recipients.toml` (needs `rekey`).

Exit non-zero on any finding. Suitable for CI.

## Threat model and what this doesn't protect against

The v1 limitations still apply verbatim (see `src/lib.rs` module
doc): plaintext in the Rimu evaluator is not zeroised, short secrets
skip redaction, UTF-8 plaintext only, no passphrase-protected keys.

New in v2:

- **Peer can decrypt every secret addressed to it.** A compromised
  rpi4b-1 reads everything on rpi4b-1's recipient list. Scope is per-
  file via `recipients.toml` — don't list a machine on a file it
  doesn't need.
- **Operator key compromise ≡ every secret is compromised.** Treat
  the operator identity file like an SSH private key. Consider age's
  passphrase-wrapped format once it's supported (see v2 non-goals).
- **`recipients.toml` is not signed.** An attacker with write access
  to the repo can add themselves as a recipient and wait for the
  next `rekey`. Out of scope here — this is the same trust boundary
  as editing the plan itself.

## Migration from agenix

Rough order:

1. Land SSH identity support in `lusid-secrets::Identity` (so lusid
   can decrypt the existing wormfarm `*.age` files without rekeying).
2. Land `recipients.toml` parser + `lusid secrets check` (read-only,
   lets us validate that the new config matches the old agenix
   config before cutting over).
3. Land `lusid secrets edit`, `rekey`, `keygen`.
4. In wormfarm-secrets: author `recipients.toml` by hand (mirroring
   the existing `secrets.nix`), run `lusid secrets check` until
   clean, delete `keys.nix` / `secrets.nix` / `justfile`.

Each step is independently useful and reversible. Step 1 unblocks
end-to-end verification of the wormfarm rpi4b-1 plan today (the
blocker in `wormfarm/TODO.md`); steps 2-4 are the replacement for
the agenix workflow.

## v2 non-goals (defer to v3+)

- Passphrase-protected identities (`age-keygen -o -`). Would need a
  passphrase-prompt UX inside `lusid-apply` — non-trivial and not
  urgent.
- age plugins (YubiKey, TPM, 1Password). Useful but orthogonal to
  getting off Nix.
- Per-target re-encryption at apply time (option 3 from the
  module-level `TODO(cc)` on remote/dev apply). This doc assumes the
  single-shared-recipients model because that's what the wormfarm
  project uses; per-target re-encryption is a separate project.
- Binary secrets. `DecryptError::NotUtf8` stays as-is.
- Secret rotation tooling beyond `rekey` — rotating plaintext (as
  opposed to re-encrypting unchanged plaintext) is `edit` territory.
