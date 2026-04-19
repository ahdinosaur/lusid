# lusid-system

Runtime detection of the host machine.

`System` bundles hostname, CPU arch, OS (with distro + version for Linux), and
current user — the same struct plans see as `ctx.system` inside their
`setup(params, ctx)` function (serialized through `rimu-interop`).

Detection covers:

- **Arch**: `cfg(target_arch)` → `X86_64` / `Aarch64`.
- **OS**: On Linux, parses `/etc/os-release` via the `etc-os-release` crate and
  recognises Ubuntu / Debian / Arch. Unknown distros return an error rather than
  a silent default.
- **Hostname**: via the `hostname` crate.
- **User**: `$USER` / `$HOME` on Unix, `$USERNAME` / `$USERPROFILE` on Windows.

Types are `#[non_exhaustive]` where variant growth is expected, so adding a new
OS variant is non-breaking.
