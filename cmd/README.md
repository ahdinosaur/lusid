# lusid-cmd

Thin wrapper around `tokio::process::Command` used by lusid operations.

On top of the tokio API it adds:

- **Stdio routing.** Boolean `stdout(bool)` / `stderr(bool)` flip between piped
  (captured) and inherited (streamed to parent) — we need both depending on
  whether the output is for program logic or for the user to see.
- **`sudo()`.** Rewraps as `sudo -n <cmd>`, forwarding explicitly-set env vars
  and the working dir. The `-n` ensures non-interactive failure rather than a
  blocked password prompt.
- **`handle()`.** For commands where a non-zero exit is meaningful (e.g. an
  `is_installed` probe) rather than an error.
- **`FromStr` via `shell-words`.** Plan authors can write command strings; lusid
  parses them into program + args.
- **`new_sh()`.** Shortcut for `sh -c "..."` when shell features are needed.

All long-running operations prefer `output()`/`spawn()` so stdout and stderr
stream rather than being buffered; non-interactive execution is mandatory for
anything wrapped in `sudo()`.
