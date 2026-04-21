## TODO

### Wire remote apply to per-target re-encryption

`cmd_dev_apply` now forwards secrets to the VM via per-target
re-encryption: the host decrypts every `*.age` with the operator identity,
re-encrypts each plaintext to the VM's ephemeral SSH keypair, and ships
ciphertexts + that keypair (as the guest's age identity) over SFTP. See
`reencrypt_for_machine` in `secrets/src/lib.rs` and the `cmd_dev_apply`
wiring in `lusid/src/lib.rs`.

`cmd_remote_apply` is still `todo!()`. The expected shape mirrors
`cmd_dev_apply` — the only substantive differences are:

- Recipient key comes from `Recipients::get_machine(machine_id)`
  (looked up by `machine_id` in `recipients.toml`'s `[machines]` table),
  not an ephemeral VM auth key.
- Guest identity is the target's existing
  `/etc/ssh/ssh_host_ed25519_key`, so we don't SFTP an identity file —
  we just pass `--identity=/etc/ssh/ssh_host_ed25519_key` to the remote
  `lusid-apply`. (This does require lusid-apply to run as root on the
  target, which it likely does anyway.)
- The rest of remote apply (resolving the target address, SSH auth, plan
  upload, TUI streaming) still needs to land before the secrets step is
  relevant.

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
