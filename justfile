export LUSID_APPLY_LINUX_X86_64 := "./target/x86_64-unknown-linux-gnu/release/lusid-apply"
export LUSID_APPLY_LINUX_AARCH64 := "./target/aarch64-unknown-linux-gnu/release/lusid-apply"

# Show available recipes.
default:
  @just --list

# Build the `lusid-apply` binary that the `lusid` CLI uploads into dev VMs.
build-lusid-apply:
  cargo build -p lusid-apply --target x86_64-unknown-linux-gnu --release
  # cargo build -p lusid-apply --target aarch64-unknown-linux-gnu --release

# -----------------------------------------------------------------------------
# Example: examples/nginx-cluster
#
# Two Debian 13 x86-64 servers, each running nginx with a per-machine greeting.

# List the machines defined in the nginx-cluster example.
nginx-cluster-list:
  cargo run -p lusid --release -- --config ./examples/nginx-cluster/lusid.toml machines list

# Boot the web-a VM (if not already running) and apply the plan to it.
nginx-cluster-apply-a: build-lusid-apply
  cargo run -p lusid --release -- --config ./examples/nginx-cluster/lusid.toml dev apply --machine web-a

# Boot the web-b VM (if not already running) and apply the plan to it.
nginx-cluster-apply-b: build-lusid-apply
  cargo run -p lusid --release -- --config ./examples/nginx-cluster/lusid.toml dev apply --machine web-b

# Open an SSH session to the web-a dev VM (e.g. to `curl localhost`).
nginx-cluster-ssh-a:
  cargo run -p lusid --release -- --config ./examples/nginx-cluster/lusid.toml dev ssh --machine web-a

# Open an SSH session to the web-b dev VM.
nginx-cluster-ssh-b:
  cargo run -p lusid --release -- --config ./examples/nginx-cluster/lusid.toml dev ssh --machine web-b

# -----------------------------------------------------------------------------
# Example: examples/arch-desktop
#
# One Arch Linux x86-64 machine with a minimal XFCE desktop + LightDM.

# List the machines defined in the arch-desktop example.
arch-desktop-list:
  cargo run -p lusid --release -- --config ./examples/arch-desktop/lusid.toml machines list

# Boot the desktop VM, apply the plan, and watch LightDM appear in the QEMU window.
arch-desktop-apply: build-lusid-apply
  cargo run -p lusid --release -- --config ./examples/arch-desktop/lusid.toml dev apply --machine desktop

# Open an SSH session to the desktop dev VM.
arch-desktop-ssh:
  cargo run -p lusid --release -- --config ./examples/arch-desktop/lusid.toml dev ssh --machine desktop
