# Example: arch desktop

A single Arch Linux machine provisioned as a minimal graphical workstation:
X.org, XFCE, and LightDM. You log in at the LightDM greeter and land in an
XFCE session.

The point of this example is to show:

- How to apply a plan to a **graphical VM** (XFCE is visible in the QEMU
  window opened by `dev apply`).
- How to install a **group of packages** in one go with `@core/pacman`.
- How to sequence a **package install** with a **service enable** so the
  display manager is ready to start as soon as its packages are on disk.

## Files

- [`lusid.toml`](./lusid.toml) — declares one machine (`desktop`) targeting
  Arch Linux x86-64. No VM overrides, so graphics is on and memory/CPU
  defaults apply.
- [`desktop.lusid`](./desktop.lusid) — the plan. Installs X + XFCE + LightDM,
  then enables and starts `lightdm.service`.

## What the plan does

```text
pacman install xorg-server xorg-xinit xfce4 lightdm lightdm-gtk-greeter
        │
        └─► systemd enable + start lightdm
```

1. `@core/pacman` installs the five packages in a single transaction:
   - `xorg-server`, `xorg-xinit` — the X11 display server.
   - `xfce4` — the XFCE meta-package (window manager, panel, session,
     Thunar file manager, terminal).
   - `lightdm`, `lightdm-gtk-greeter` — the login display manager and its
     greeter UI.
2. `@core/systemd` enables `lightdm.service` (so it starts on every boot)
   and activates it immediately. A QEMU window will show the LightDM
   greeter a few seconds after the apply finishes.

## Try it (local dev VM)

From the repo root. The first run downloads the Arch cloud image (~700 MB)
and takes a few minutes; later runs reuse it.

```sh
# Apply the plan. A QEMU window opens during boot; the apply itself streams
# in your terminal.
just arch-desktop-apply

# After apply, the QEMU window will show the LightDM login greeter.
# Log in as user `arch` (the cloud image's default account).
# If you need a shell instead:
just arch-desktop-ssh
```

## Try it (on a real Arch machine)

Same plan, no VM. Copy `desktop.lusid` and `lusid.toml` onto the target
Arch host (making sure the `hostname` in `lusid.toml` matches the host's
own hostname) and run:

```sh
lusid local apply --config ./lusid.toml
```

You'll need `sudo` on the target — `pacman -S` and `systemctl enable/start`
both use it. Once the plan completes, reboot (or just log out of the
console) to reach the LightDM greeter.

## Things to try next

- Swap XFCE for LXQt: change the packages to
  `["xorg-server", "xorg-xinit", "lxqt", "sddm"]` and the systemd unit to
  `sddm` (LXQt's typical greeter is SDDM, not LightDM). Re-apply and you
  get a different desktop with the same plan shape.
- Add a user resource to the plan that creates a non-default account for
  logging in at the greeter.
- Add your dotfiles (vimrc, gitconfig, etc.) via `@core/file` items, each
  sourced relative to the plan directory.
