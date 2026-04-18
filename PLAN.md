# macOS support plan

Adding macOS as both **host** (where lusid runs) and **target** (what lusid
configures), plus a sketch of VM options. Written after a codebase walkthrough;
cite files by path when continuing the work.

## Current state (what already exists)

- `lusid_system::Os` is `#[non_exhaustive]` with only an `Os::Linux(Linux)`
  variant (`system/src/os.rs:19-25`); `Os::get()` is gated on
  `#[cfg(target_os = "linux")]`.
- `lusid_ctx::Paths::create()` already has a `#[cfg(target_os = "macos")]`
  branch using `~/Library` (`ctx/src/paths.rs:63-71`). ✅
- `lusid_system::User`/`Arch`/`Hostname` are already Unix-portable.
- `fs::change_owner*` is `#[cfg(unix)]` — works on macOS.
- `fs::copy_dir` (`fs/src/lib.rs:238`) shells out to `cp --recursive`, which
  is GNU-only. Note(cc) on the function already flags this.
- `lusid` CLI wires two env vars, `LUSID_APPLY_LINUX_X86_64` and
  `LUSID_APPLY_LINUX_AARCH64` (`lusid/src/lib.rs:57-61`), and uses them
  unconditionally (`:215`, `:285`) — the Note(cc) there foresees a map.
- Resources with strong Linux assumptions: `apt`, `apt_repo`, `pacman`,
  `systemd`, `user` (uses `useradd`/`usermod`/`userdel`), `group` (uses
  `groupadd`/`groupmod`/`gpasswd`).
- Cross-platform resources today: `file`, `directory`, `git`, `command`.
- `lusid-vm` is QEMU + KVM + libguestfs (`virt-get-kernel`). Linux-host only
  in practice.

---

## Phase 1 — Host: lusid runs on macOS

Smallest valuable slice: `lusid local apply` builds and runs on macOS against
the local machine for cross-platform resources.

- [x] `system/src/os.rs`: add `Os::MacOS { version: String }` variant
      (serde shape mirroring Linux: `{ type: "macos", macos: "15.3.1" }`);
      add `#[cfg(target_os = "macos")] Os::get()` that parses
      `sw_vers -productVersion` or the SystemVersion plist. Update `Display`.
      Decide version validation policy (free-form vs. `X.Y[.Z]`).
- [x] `fs/src/lib.rs` (`copy_dir`): replace `cp --recursive` with a portable
      async walker (`tokio::fs::read_dir` + `fs::copy` + symlink handling) so
      it works on BSD cp / non-Linux.
- [x] `lusid/src/lib.rs`: generalise apply-binary lookup from Linux-only env
      vars (`LUSID_APPLY_LINUX_{X86_64,AARCH64}`) to `(OsKind, Arch)`-keyed —
      `HashMap<(OsKind, Arch), String>` in `Config`, populated from CLI
      flags / env vars / `lusid.toml` / defaults. `cmd_local_apply` picks
      via `System::get()`; `cmd_dev_apply` picks via the machine spec.
      Added `Os::kind()` / `OsKind` to `lusid-system`.
- [x] Resource gate-keeping: added `ResourceType::supported_on(OsKind) -> bool`
      with default `true` and `OsKind::Linux`-only overrides on `apt`,
      `apt_repo`, `pacman`, `systemd`, `user`, `group`. Planner emits a
      span-aware `CoreModuleNotSupportedOnOs` error pointing at the offending
      `@core/<id>` reference before any apply.
- [x] Sanity pass: target triples are `x86_64-apple-darwin` and
      `aarch64-apple-darwin`. Verified `cargo check -p lusid-fs
      -p lusid-system --target {aarch64,x86_64}-apple-darwin` on Linux.
      Full-workspace cross-check fails on the `psm` build script (rimu →
      chumsky → stacker → psm uses Apple-flavored `cc` flags that a Linux
      host's `cc` rejects) — not a lusid issue. Full sanity pass needs
      either a macOS CI runner or an `osxcross` toolchain; the Phase 1
      changes themselves are portable.

Cross-platform resources usable on macOS immediately after Phase 1:
`@core/file`, `@core/directory`, `@core/git`, `@core/command`.

## Phase 2 — Target: macOS-native resources

Makes lusid actually useful for provisioning a Mac.

- [x] `@core/brew`: mirror of `@core/apt`. Params `package` / `packages`.
      State via `brew list --versions --formula <pkg>` exit code (0 =
      installed); not parsing stderr since Homebrew's "not installed"
      message text changes across releases. Operations: `Update` (id
      `"update"`) followed by `Install { packages }` (requires `"update"`).
      Uninstall variant left as `TODO(cc)` to mirror apt/pacman later.
      Runs without `sudo` (brew refuses root); sets
      `HOMEBREW_NO_AUTO_UPDATE=1` on `install` so the explicit `Update`
      operation stays the only update point.
- [ ] `@core/brew_cask` (or `cask: bool` on `@core/brew`) for GUI apps.
- [ ] `@core/launchd`: equivalent of `@core/systemd`. Params `name` (label),
      `enabled`, `active`, `scope` (`system` → `/Library/LaunchDaemons`,
      `user` → `~/Library/LaunchAgents`). Operations `Bootstrap`/`Bootout`
      for enable/disable, `Kickstart`/`Stop` for active/inactive — the
      modern `launchctl` verbs, not the deprecated `load`/`unload`.
- [ ] User/group on macOS. `useradd` family doesn't exist; use `dscl` /
      `sysadminctl` / `dseditgroup`. Recommended shape: keep the `User` /
      `Group` resource params identical, split the `OperationType::apply`
      impl to pick a macOS branch based on `System::get().os`. If the
      param surface diverges enough (system users, uid ranges), split
      into sibling resources and keep each OS honest.
- [ ] Nice-to-have, later: `@core/defaults` (for `defaults write`),
      `@core/mas` (Mac App Store), `@core/xcode_cli_tools`.

## Phase 3 — VM support

Three distinct sub-questions, different answers.

### 3a. macOS host running Linux VMs — worth doing

Blocker: QEMU + KVM + libguestfs. KVM doesn't exist on macOS; libguestfs is
Linux-only.

Options:
1. **Keep QEMU, swap accelerator + image prep.** QEMU-on-macOS uses
   `-accel hvf`. Drop `virt-get-kernel`; boot directly from the cloud image
   via `-hda <qcow2>`. `mkisofs` available via `brew install cdrtools`.
   Ship/download OVMF firmware instead of hardcoding
   `/usr/share/OVMF/OVMF_{CODE,VARS}_4M.fd` (`vm/README.md:42`).
2. **Add a Virtualization.framework backend** via a Rust binding. Apple
   Silicon only. Faster than QEMU/HVF, tighter integration.
3. **Shell out to Tart or Lima.** Less work, but gives up single-crate
   lifecycle ownership.

Suggested approach: **(1) first, (2) later if perf matters.** Introduce a
`VmBackend` trait (prepare, start, stop, remove, ssh_port); implement
`QemuLinuxKvm` and `QemuMacHvf` variants; pick by host `System::get().os`.
The existing `Vm` becomes the QEMU-specific impl.

`instance_start.rs:58` already has a TODO about hard-coded `kvm = true`;
becomes `accel: Accel::Kvm | Accel::Hvf | Accel::None`.

### 3b. macOS host running macOS VMs

Only via Virtualization.framework. Apple Silicon only. Guest images via
Apple RestoreImage (no cloud-init). QEMU cannot reliably boot modern macOS.
Substantial parallel pipeline; **defer** until 3a ships and there's demand.

### 3c. Linux host running macOS VMs

Blocked by Apple's EULA (macOS licensed only on Apple hardware). **Skip.**

---

## Sequencing

1. Phase 1 as one PR — type foundation + portability fixes.
2. Phase 2a: `@core/brew`.
3. Phase 2b: `@core/launchd`, then User/Group macOS backend.
4. Phase 3a: `VmBackend` trait + QEMU/HVF backend. Punt 3b.
