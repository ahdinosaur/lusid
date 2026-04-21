# TODO: Secrets

Design for adding age-encrypted project secrets to lusid, starting from a
state without any secrets support (i.e. before
`e22f47ce5434c224a792d41d120237c6086026a0`). The current `secrets-2` branch
is thrown away; pieces of its code are good reference material and called
out below, but the branch itself isn't merged.

## Goal

Agenix-style secret flow:

- Project secrets live as `<root>/secrets/*.age` ciphertexts, plus a
  declarative config (`lusid-secrets.toml`) naming operators, machines,
  groups, and per-file recipients.
- Plans reference secrets **by name** via a dedicated `@core/secret`
  resource. The Rimu evaluator never sees plaintext.
- At apply time the machine's identity decrypts only the subset of files
  it's declared as a recipient for (in `lusid-secrets.toml`), into an
  in-memory `Secrets` bundle on `Context`. `@core/secret`'s operation
  resolves the name to plaintext just before the atomic write.
- Remote / dev apply forwards secrets to the guest via **per-target
  re-encryption**: the operator identity never leaves the host.

## Non-goals

- **No `ctx.secrets` in Rimu.** Plans reference secrets exclusively via
  `@core/secret`. Exposing decrypted paths through `ctx.secrets` collides
  with the planned rework of `HostPath` relative/absolute semantics;
  revisit only if that rework lands and we still want the indirection.
- **No tmpfs / runtime dir for decrypted plaintexts.** Plaintexts stay in
  memory (`SecretBox<String>`) for the duration of apply. The target
  filesystem is the only place plaintext ever lands on disk, via
  `@core/secret`'s atomic write.
- **No binary secrets in v1.** Decrypted payloads must be UTF-8;
  non-UTF-8 errors loudly at decrypt time. Binary support is a later diff
  (wrap `Vec<u8>` everywhere, teach `Redactor` to substring-match on
  bytes) — doable but not v1.

## Design at a glance

```
                              lusid-secrets.toml
                              [operators] [machines] [groups] [files]
                                     |
                                     v
 operator identity  -->  Recipients::files_for_alias(<this-alias>)
 (x25519 / ssh)                      |
                                     v
                 decrypt subset  -->  Secrets { HashMap<String, SecretBox<String>> }
                                              |
                                              v
                                     Context::set_secrets(...)
                                              |
                                              v
            plan refers to secret by name -->  @core/secret { name, path, mode?, ... }
                                              |
                                              v
                                     FileSource::Secret(name)
                                              |
                                              v
                             resolve against ctx.secrets(), atomic-write
```

Redactor hangs off the same `Secrets` bundle; every per-operation
stdout/stderr line is substring-scrubbed before emit.

## Phases

Each phase is a standalone PR. Cross-phase deps are called out. Suggested
landing order is at the bottom.

### Phase 1 — `secrets` crate: crypto + identity primitives

New crate. Deps: `age` (with `ssh` feature), `secrecy`, `tokio` (async file
I/O only), `thiserror`, `displaydoc`, `tracing`.

- `Identity` — parse an identity file containing **either** an age x25519
  secret key or an OpenSSH ed25519 / RSA private key. Reject
  passphrase-protected SSH keys up-front. Parsed bytes go behind
  `SecretBox` so drop is zeroising.
- `Key` enum (`X25519(age::x25519::Recipient)`, `Ssh(age::ssh::Recipient)`)
  — recipient-side; for parsing public keys declared in
  `lusid-secrets.toml`.
- `encrypt_bytes(recipients, path, plaintext) -> Vec<u8>`
- `decrypt_bytes(identity, path, ciphertext) -> Arc<SecretBox<String>>`
- `path` is used only for error labelling (never opened here).
- UTF-8 check on the decrypt output; `DecryptError::NotUtf8 { path }` on
  non-UTF-8 payloads.
- Unit tests: x25519 round-trip; SSH identity parsing (ed25519 + RSA);
  rejection of passphrase-protected SSH; bad-header error propagation.

**Reference to adapt from `secrets-2`**: `secrets/src/crypto.rs`,
`secrets/src/identity.rs`. Both are small and land mostly unchanged.

### Phase 2 — `lusid-secrets.toml` + `Recipients` model

Depends on phase 1.

- File path: `<secrets_dir>/lusid-secrets.toml`. Default `secrets_dir` is
  `<root>/secrets/`; overrideable via `lusid.toml` / CLI.
- Shape:
  ```toml
  [operators]
  mikey = "age1..."

  [machines]
  rpi4b-1 = "ssh-ed25519 AAAA..."

  [groups]
  operators = ["mikey"]

  [files]
  "api_token" = { recipients = ["@operators", "rpi4b-1"] }
  ```
- `Recipients::load(secrets_dir) -> Result<Recipients, RecipientsError>`
  with load-time validation:
  - `[operators]` and `[machines]` share a namespace; alias collision is
    an error (so bare references are unambiguous).
  - `@name` references in `[files]` resolve via `[groups]`. Expansion is
    **shallow** (groups can't reference groups) — keeps the model
    predictable with no meaningful limitation.
  - Unknown aliases / unknown groups / empty recipients list: hard errors.
- Lookups:
  - `resolve(file_stem) -> Vec<ResolvedRecipient>` — expanded recipient
    list for a single file; used by `rekey`, `edit`, and re-encryption.
  - `get_machine(machine_id) -> Option<&Key>` — for per-target
    re-encryption on the host side.
  - `files_for_alias(alias) -> Vec<&str>` — list of file stems a given
    alias can decrypt, direct or via any group containing that alias.
    This is the load-bearing lookup for "which secrets does this
    machine's identity have access to" at apply time.
- Unit tests: alias collisions; group expansion; unknown refs;
  `files_for_alias` correctness (direct listing, via one group, via
  multiple groups, excluded).

**Reference to adapt from `secrets-2`**: `secrets/src/recipients.rs`.
Rename constants and touch up doc comments; add `files_for_alias`.

### Phase 3 — `lusid secrets` CLI

Depends on phases 1 + 2. Runs independently of the apply pipeline; can
land in parallel with phase 4+.

Dispatched from the top-level `lusid` wrapper. All subcommands take a
`CliEnv { secrets_dir, identity_path }` resolved by the wrapper from
`lusid.toml` + CLI flags.

- `ls` — list every `*.age` file and its declared recipients (resolved
  aliases). No identity / decryption.
- `cat <name>` — decrypt the named file to stdout. Requires `--identity`.
- `edit <name>` — decrypt into a mode-0600 tempfile, open in `$EDITOR`,
  re-encrypt on save using current `[files]` recipients. Requires
  `--identity`. Scrub the tempfile even on editor failure.
- `rekey [name]` — re-encrypt to the current recipient list. Skip files
  whose ciphertext header already matches the declared recipient set.
  Without `<name>`, rekey every entry in `[files]`. Requires `--identity`.
- `keygen [-o <path>]` — generate a fresh x25519 identity. Default output
  `$XDG_CONFIG_HOME/lusid/identity` (or `$HOME/.config/lusid/identity`).
  Refuses to overwrite.
- `check` — audit `secrets/` against `lusid-secrets.toml`. Non-zero exit
  on drift: orphan ciphertexts, missing ciphertexts, recipient-set
  mismatch. No identity required; suitable for CI.

Integration test: round-trip through `keygen` → `edit` (scripted
`EDITOR=…`) → `cat` → `rekey` → `check`.

**Reference to adapt from `secrets-2`**: `secrets/src/cli.rs`,
`secrets/src/check.rs`. Near drop-in; rename `recipients.toml` →
`lusid-secrets.toml` throughout.

### Phase 4 — Decryption at apply + `Secrets` on `Context`

Depends on phases 1 + 2.

- `Secret = Arc<SecretBox<String>>`. `Arc` keeps clones into the redactor
  cheap; `SecretBox` gives redacted `Debug` and drop-zeroisation.
- `Secrets` wraps `HashMap<String, Secret>` keyed by file stem
  (`secrets/api_key.age` → `api_key`). Small API: `empty`, `get`, `iter`,
  `len`, `is_empty`, `redactor`.
- `Context` gains `secrets: Secrets`, with `secrets()` accessor and
  `set_secrets(secrets)` mutator. Initialised empty in `Context::create`.
- `lusid-apply` pipeline additions:
  1. `options.identity_path` → `Identity::from_file(...)`. No identity →
     skip secrets entirely (empty bundle).
  2. `Recipients::load(secrets_dir)`. Missing config while an identity is
     present → hard error (avoids silently-empty bundles when the user
     *did* supply credentials).
  3. Match the identity's public key against `[operators]` and
     `[machines]` to find its alias. No match → hard error.
  4. `decrypt_dir(&identity, &secrets_dir, &recipients.files_for_alias(alias))`
     → `Secrets`. Files not listed for this alias are ignored entirely
     (not even opened).
  5. `ctx.set_secrets(secrets)` before `plan(...)`.
- Error surface: `IdentityError`, `RecipientsError`,
  `DecryptDirError { ScanDir, ReadFile, Decrypt }`, `NoAliasForIdentity`.

**Reference to adapt from `secrets-2`**: `secrets/src/lib.rs` (`Secrets`,
`decrypt_dir`) and `lusid-apply/src/lib.rs`'s decrypt wiring. The
alias-matching step is new — current branch decrypts every file
regardless.

### Phase 5 — `@core/secret` resource

Depends on phase 4.

- New resource type in `resource/src/resources/secret.rs`:
  ```rimu
  - module: "@core/secret"
    params:
      name: "api_token"                     # -> secrets/api_token.age
      path: "/etc/myapp/api_token"
      mode: 384                              # optional; default 0o600
      user: "myapp"                          # optional
      group: "myapp"                          # optional
  ```
- `mode` defaults to `0o600` (owner read/write only) — this default is the
  main reason `@core/secret` exists as a distinct resource vs a
  naked `@core/file`.
- Delegates to `@core/file`'s state/change/operation machinery. Extend
  `@core/file`:
  - New `FileResource::SecretContents { name: String, path: FilePath }`
    atom.
  - New `FileSource::Secret(String)` variant, carried through
    `FileChange::Write { source }` into `FileOperation::Write`.
- `@core/secret` emits the same Mode / User / Group sibling atoms
  `@core/file` does, wired to the `"file"` causality id.
- `state()` for `SecretContents`: look up plaintext in `ctx.secrets()`;
  compare against on-disk bytes. Missing secret → `FileStateError::MissingSecret`
  (loud, not a silent `NotSourced`).
- `operation.apply()` for `FileOperation::Write` with
  `FileSource::Secret(name)`: resolve plaintext up-front (so the inner
  async block doesn't need to borrow `ctx`), then atomic-write.
  Plaintext copy lives only for the duration of the write.
- `Display` for `FileResource::SecretContents` and
  `FileOperation::Write { source: Secret(name) }` must print the *name*,
  never the plaintext.
- Add `examples/with-secrets.lusid` demonstrating the flow.

**Reference to adapt from `secrets-2`**: `resource/src/resources/secret.rs`
and the `SecretContents`/`FileSource::Secret` additions in
`resource/src/resources/file.rs` + `operation/src/operations/file.rs`
are the landing target.

### Phase 6 — Redactor over per-operation stdout/stderr

Depends on phase 4.

- `Secrets::redactor()` → `Redactor { secrets: Vec<Secret> }`. Filter by
  `REDACT_MIN_LEN = 8`; sort longest-first so outer matches consume inner
  substrings before the inner pattern gets a chance.
- `Redactor::redact(&line) -> String` substring-replaces every registered
  plaintext with `"<redacted>"`.
- `lusid-apply` builds the redactor right after `decrypt_dir`; before
  calling `ctx.set_secrets(secrets)` moves the bundle (redactor holds
  `Arc` clones so this is fine).
- Wrap every per-operation stdout/stderr line before emit. Clone the
  redactor once per operation spawn.
- Document limitations inline (copy from `secrets-2`'s `Redactor` doc):
  substring-only (base64 / JSON-escaped / chunked-across-reads secrets
  slip through); overlapping-adjacent secrets can leave one visible; short
  secrets skipped.

**Reference to adapt from `secrets-2`**: `secrets/src/lib.rs` (`Redactor`
type + tests). Near drop-in.

### Phase 7 — `dev apply` per-target re-encryption

Depends on phases 1 + 2 + 4 + 5 (i.e. local apply with secrets must work
first).

- `reencrypt_for_machine(host_identity, secrets_dir, machine_key) -> Vec<ReencryptedSecret>`:
  - Decrypt every `*.age` in `secrets_dir` with the host identity.
  - Re-encrypt each plaintext to `machine_key` **alone** (single-recipient
    output).
  - Return `[{ stem, ciphertext }]`. Plaintexts live only inside the
    intermediate `Secrets` and are zeroised at function return.
- `cmd_dev_apply` wiring:
  - VM already has an ephemeral SSH keypair for apply-time auth; reuse it
    as both the age recipient (host side) and the guest identity (guest
    side).
  - Before invoking `lusid-apply` on the guest: SFTP the re-encrypted
    ciphertexts into the guest's `<secrets-dir>`, and the VM keypair as
    the guest's age identity file.
  - Pass `--identity=<guest identity path>` and
    `--secrets-dir=<remote secrets dir>` to the guest `lusid-apply`.
- Operator identity never leaves the host. Guest has no knowledge of
  other machines' keys.
- Guest `lusid-apply` decrypts with the single identity it was given; on
  the guest side the alias-matching in phase 4 step 3 resolves through a
  minimal single-entry `lusid-secrets.toml` generated by the host, or
  (simpler) a dedicated code path that skips `Recipients` entirely on the
  guest and just decrypts every `*.age` it sees. Pick one in
  implementation — the simpler code path is probably worth the asymmetry.

**Reference to adapt from `secrets-2`**: `reencrypt_for_machine` in
`secrets/src/lib.rs`; the `cmd_dev_apply` wiring in `lusid/src/lib.rs`.

### Phase 8 — `remote apply` per-target re-encryption

Depends on phase 7 + the rest of remote apply (target resolution, SSH
auth, plan upload, TUI streaming).

Same shape as phase 7 with two substitutions:

- **Recipient key**: `Recipients::get_machine(machine_id)` rather than
  an ephemeral keypair.
- **Guest identity**: the target's existing
  `/etc/ssh/ssh_host_ed25519_key` — nothing is SFTP'd for the identity;
  just pass `--identity=/etc/ssh/ssh_host_ed25519_key`. Requires the
  guest `lusid-apply` to run as root, which it typically does already.

If the rest of remote apply isn't ready, `cmd_remote_apply` stays
`todo!()` and this phase waits.

## Suggested landing order

1. **Phase 1** (crypto primitives) — small, no dependents; unblocks
   everything else.
2. **Phase 2** (`lusid-secrets.toml`) — small, depends on 1.
3. **Phase 4** (decryption at apply + `Context`) — the core wiring.
4. **Phase 5** (`@core/secret`) — now local apply with secrets works
   end-to-end. This is the MVP landing point.
5. **Phase 6** (redactor) — belt-and-suspenders over the apply output
   stream.
6. **Phase 3** (CLI) — independent of 4–6; can be done any time after 2.
7. **Phase 7** (`dev apply` re-encryption).
8. **Phase 8** (`remote apply` re-encryption) — last, gated on the rest
   of remote apply.

Phases 1–5 can realistically land as a single PR if you want MVP in one
drop; the split above is for review ergonomics, not hard dependencies.

## Decisions already made (for reference)

- `@core/secret` takes a **name** (file stem), not a path or plaintext
  value.
- `ctx.secrets` is **not exposed** to Rimu. Plans cannot arithmetically
  combine or string-interpolate secret values.
- Plaintexts live **in memory only** during apply (`SecretBox<String>`).
  No tmpfs, no runtime dir.
- Decryption is **selective**: consult `lusid-secrets.toml` to find
  which files this machine's identity is a recipient for; decrypt only
  those. Files this machine has no access to are never opened.
- Missing-secret at apply time is a **hard error**
  (`FileStateError::MissingSecret`), not a silent empty file.
- UTF-8-only decryption; binary support deferred.
- Re-encryption (`dev apply` / `remote apply`) is **per-target single
  recipient**; operator identity never leaves the host.
