# lusid-ssh

Async SSH client for lusid's VM provisioning pipeline.

Built on [`russh`]. The public surface is [`Ssh`]:

- `Ssh::connect` — retry/backoff on transient IO errors, then ed25519 public-key auth.
- `Ssh::command` — start a remote command; returns an `SshCommandHandle` exposing
  stdout/stderr as [`tokio::io::AsyncRead`] streams, stdin as `AsyncWrite`, and
  the exit code as an `async_promise::Promise`.
- `Ssh::sync` — SFTP upload: directory, single file, or raw bytes.
- `Ssh::terminal` — forward the current TTY (including `SIGWINCH` for window
  resize) to a remote interactive shell.
- `Ssh::disconnect` — clean channel teardown.

[`SshKeypair`] manages a local ed25519 keypair (`id_ed25519[.pub]`) — either
load an existing one from disk or create + save a new one.

## Host key verification

`NoCheckHandler` skips host key verification. This is intentional for the
current use case: lusid connects only to VMs it has just booted. When SSHing
into arbitrary remote machines becomes a use case, this must be revisited.

## Internals

- `session.rs` — thin wrappers around `russh::client::Handle` (as
  `AsyncSession`) and `russh::Channel` (as `AsyncChannel`). `AsyncChannel`
  runs a reader task that demuxes `ChannelMsg`s into per-stream
  subscribers and event promises (success/failure, EOF, exit status).
- `stream.rs` — `ReadStream` adapts an mpsc channel of `CryptoVec`s into an
  `AsyncRead` / `AsyncBufRead` / `Read` / `BufRead` source.
- `sync.rs` — SFTP upload loops. Symlinks are currently skipped with a
  `warn!` rather than propagated as errors.

## References

- [`hydro-project/async-ssh2-russh`](https://github.com/hydro-project/async-ssh2-russh), Apache-2.0 — original inspiration for the `AsyncSession` / `AsyncChannel` shape.
