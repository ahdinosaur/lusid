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
