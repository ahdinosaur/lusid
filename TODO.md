## TODO

### Implement remote/dev apply secrets via per-target re-encryption

`lusid-apply` runs locally only today. `remote apply` and `dev apply` do
not forward secrets to the target — `cmd_dev_apply` errors with
`SecretsNotYetSupported` when the project has secrets configured, and
`cmd_remote_apply` is still `todo!()`. See the three candidate strategies
listed in `secrets/src/lib.rs`'s crate doc and the `TODO(cc)`s in
`lusid/src/lib.rs` (around `cmd_remote_apply` / `cmd_dev_apply`).

Pick **option 3** (per-target re-encryption): each target machine's SSH
host key is a recipient on exactly the secrets it needs, and the host
re-encrypts before shipping the ciphertext to the guest. The v2
`recipients.toml` already supports SSH peer keys, so the key plumbing is
mostly there — what's missing is the re-encryption step at apply time
and a clear answer to whose key is whose.

This likely means making the operator/machine distinction explicit in
the data model:

- **Operators** decrypt on the host (x25519 keys stored under
  `~/.config/lusid/identity`).
- **Machines** are targets that can also be recipients of the secrets
  they need (SSH host keys under `/etc/ssh/ssh_host_ed25519_key`).

Today `[keys]` in `recipients.toml` is a flat namespace with both kinds
mixed. Options to explore:

1. Keep the flat `[keys]` table and infer kind from the key type
   (`age1...` → operator, `ssh-...` → machine). Simplest; fragile if we
   ever want an x25519-keyed machine.
2. Split into `[operators]` and `[machines]` tables at the TOML level.
   Makes the model explicit; each machine's entry can then carry extra
   metadata (hostname, ssh port, default secrets list).
3. Tag each `[keys]` entry with a `kind = "operator" | "machine"` field.
   Less disruptive than (2) but more string-typed.

`lusid.toml`'s `[machines]` section already names machines — ideally
that's the same identifier space as the secrets recipient list, so
`lusid secrets rekey` / remote apply can look up a machine's SSH host
key without re-declaring it.

Rollout order: (a) settle on the data model, (b) wire `remote apply` /
`dev apply` to re-encrypt for the target machine's recipient before
shipping, (c) guest decrypts with its SSH host key via the existing
`Identity::from_file` path.

### Remove secret plaintext from Rimu; replace with `@core/secrets` module

Today `ctx.secrets.<name>` evaluates to a `Value::Tagged` whose inner is
a plain `Value::String` holding the plaintext. Rimu propagates the tag
through `+` concatenation so the typed identity survives, but every
intermediate copy Rimu makes (function args, object construction,
arithmetic temporaries) lives outside the `SecretBox` envelope and is
not zeroised on drop. See the `Note(cc)` block in
`params/src/lib.rs` (around `pub type Secret = ...`) and `plan/src/eval.rs`
(around `secrets_value`).

The agenix / sops-nix approach avoids this entirely: plans reference
secrets **by name**, and secret **contents** only materialise at apply
time, into a file on disk. The plan never sees plaintext.

Concrete shape:

- Drop `ParamType::Secret` as a string-valued type. Drop
  `TAG_SECRET` from params, drop the tagged-secret wrapping in
  `plan/src/eval.rs::secrets_value`, drop the
  `ValidateValueError::NullSecret` arm.
- `ctx.secrets.<name>` becomes a reference value — e.g. a tagged
  `Value::Tagged { tag: "secret-ref", inner: <name> }` or a dedicated
  Rimu value kind. Arithmetic on it is an error (no more "prefix " +
  secret); the *only* thing a plan can do with it is pass it to the
  secret-consuming module.
- Introduce `@core/secrets` as the single consumer of a secret
  reference. Its `apply` decrypts the named ciphertext into a file
  under `ctx.paths().runtime_dir()` (tmpfs, 0600, per-machine), and
  resources that need the plaintext take a `TargetPath` pointing at
  that file instead of a `Secret`.
- Redactor still gets the full plaintext table at apply start (no
  change to eager decryption). String scrubbing of process output
  remains a second line of defence in case a resource inlines the file
  contents somewhere.

This also resolves the `Note(cc)` about typo-on-lookup: a
`ctx.secrets.<typo>` that becomes `Null` currently silently propagates;
a strict secret-reference container errors at evaluation time.

### Read identity key material via `SecretBox`

`Identity::from_file` uses `fs::read_to_string` in
`secrets/src/identity.rs`, which loads the private key into a plain
`String`. The `age` crate wraps the parsed form in `SecretString`
internally, so the window is short — but the read buffer is still an
ordinary heap allocation that isn't zeroised on drop.

Switch the read path to land the bytes directly in a `SecretBox<String>`
(or `SecretBox<Vec<u8>>` and decode UTF-8 inside the secret envelope),
so the raw key material is zeroised as soon as parsing is done. The
in-memory `Identity` can keep its current shape (it already holds
`age::x25519::Identity` / `age::ssh::Identity`, both of which own their
own `SecretString`s); only the transient file-read buffer changes.

Small scope, but closes a gap that's visible in heap dumps and is
cheap to fix once we're done with the bigger items above.
