# lusid-vm-test

End-to-end testing of lusid plans against ephemeral QEMU VMs.

Inspired by [NixOS VM tests](https://wiki.nixos.org/wiki/NixOS_VM_tests):
declare a set of nodes (each one a [`Machine`](../machine/src/lib.rs)), drive
them via a typed Rust harness, and assert on post-conditions — files exist,
commands exit cleanly, services are active, the second `apply` is a no-op.

The tests are hermetic: each `#[lusid_vm_test]` body gets fresh per-instance
qcow2 overlays, freshly minted SSH keys, and unique forwarded ports. Nothing
on the host changes; nothing leaks between tests beyond the cached base image.

## Why

Today, validating a plan end-to-end means a human running `cargo run -p lusid
-- apply` against a hand-rolled VM and squinting at the TUI. That is fine for
exploratory work but not as a regression gate:

- New `@core/<id>` resources (apt-repo, git, file, …) ship without proof that
  they actually converge a target machine.
- Idempotency bugs (a `change()` that never returns `None`) only surface when
  someone re-runs a plan and notices unexpected churn.
- Cross-distro behaviour (Debian 12 vs Ubuntu 22.04, apt vs pacman) is
  asserted by inspection, not by CI.
- Refactors in `lusid-apply` / `lusid-operation` (epoch scheduling, merge
  logic, the NDJSON protocol) have no integration coverage — only crate-level
  unit tests.

`lusid-vm-test` closes that gap: it boots a VM, runs `lusid apply` against
it, parses the structured output, and lets a test author write assertions in
plain Rust.

## Design overview

### Layers

```
                   #[lusid_vm_test] async fn ...
                              │
                       ┌──────▼──────┐
                       │   Driver    │   per-test orchestrator
                       │ (this crate)│   • boots / shuts down nodes
                       └──┬───────┬──┘   • runs lusid-apply over SSH
                          │       │     • parses AppUpdate stream
                ┌─────────▼─┐ ┌───▼─────┐
                │  lusid-vm │ │ lusid-  │
                │ (QEMU)    │ │  ssh    │
                └───────────┘ └─────────┘
```

The driver is the only new piece. Everything underneath is already in the
workspace:

- [`lusid-vm`](../vm) boots the QEMU guest and returns a [`Vm`] with an SSH
  port and ed25519 keypair.
- [`lusid-ssh`](../ssh) opens an authenticated session, exec's commands with
  streaming stdout/stderr, and SFTPs files.
- [`lusid-apply`](../lusid-apply) is the binary we run on the guest. It
  already emits NDJSON [`AppUpdate`](../apply-stdio/src/lib.rs)s on stdout —
  the driver consumes that same stream as its assertion source of truth.

### Test shape

```rust
use lusid_machine::{Machine, MachineVmOptions};
use lusid_system::{Arch, Linux, Os, Hostname};
use lusid_vm_test::{vm_test, Driver};

fn debian_12() -> Machine {
    Machine {
        hostname: "test".parse().unwrap(),
        arch: Arch::X86_64,
        os: Os::Linux(Linux::Debian { version: 12 }),
        vm: Some(MachineVmOptions::default()),
    }
}

#[lusid_vm_test]
async fn apt_repo_installs_docker(driver: Driver) {
    let node = driver.node("debian", debian_12()).await;

    let result = node
        .apply_plan("examples/docker.lusid")
        .await
        .assert_succeeded();

    node.assert_file_exists("/etc/apt/sources.list.d/docker.sources").await;
    node.assert_file_mode("/etc/apt/keyrings/docker.asc", 0o644).await;
    node.assert_command_succeeds("apt-cache policy docker-ce").await;

    // Idempotency: re-applying the same plan should be a no-op.
    result.assert_idempotent().await;
}
```

`#[lusid_vm_test]` is a thin attribute macro that wraps the body in
`#[tokio::test]`, gates execution on `RUN_VM_TESTS=1` (skips with a logged
reason otherwise), and constructs the [`Driver`]. The driver owns the lifecycle
of every node spawned during the test and tears them all down on drop —
even if the test panics — by killing qemu and removing the per-instance dir.

### Driver API surface (sketch)

```rust
pub struct Driver { /* ... */ }

impl Driver {
    /// Boot a VM with this name. Idempotent within a single test —
    /// repeated calls return the same node. `name` namespaces all on-disk
    /// state (instance dir, ssh keypair, forwarded ports).
    pub async fn node(&self, name: &str, machine: Machine) -> Node;

    /// Boot multiple nodes in parallel. Names must be unique.
    pub async fn nodes(&self, specs: &[(&str, Machine)]) -> HashMap<String, Node>;
}

pub struct Node { /* holds Vm + an Ssh session pool */ }

impl Node {
    // ── plan execution ────────────────────────────────────────────
    pub async fn apply_plan(&self, plan: impl AsRef<Path>) -> ApplyRun;
    pub async fn apply_plan_with_params(
        &self,
        plan: impl AsRef<Path>,
        params: serde_json::Value,
    ) -> ApplyRun;

    // ── file assertions ───────────────────────────────────────────
    pub async fn read_file(&self, path: &str) -> Vec<u8>;
    pub async fn assert_file_exists(&self, path: &str);
    pub async fn assert_file_absent(&self, path: &str);
    pub async fn assert_file_contents(&self, path: &str, expected: &[u8]);
    pub async fn assert_file_mode(&self, path: &str, mode: u32);

    // ── command assertions ────────────────────────────────────────
    pub async fn run(&self, cmd: &str) -> RemoteOutput;       // exit code + stdout + stderr
    pub async fn assert_command_succeeds(&self, cmd: &str);
    pub async fn assert_command_fails(&self, cmd: &str);
    pub async fn wait_until_succeeds(&self, cmd: &str, timeout: Duration);

    // ── service assertions ────────────────────────────────────────
    pub async fn assert_unit_active(&self, unit: &str);       // systemctl is-active
    pub async fn assert_unit_failed(&self, unit: &str);

    // ── lifecycle ────────────────────────────────────────────────
    pub async fn reboot(&self);
    pub async fn shutdown(&self);
}

/// One run of `lusid apply` on a node, plus the parsed AppUpdate stream.
pub struct ApplyRun {
    pub node: Node,
    pub plan: PathBuf,
    pub params: Option<serde_json::Value>,
    pub view: AppView,                     // final folded state from apply-stdio
    pub updates: Vec<AppUpdate>,           // raw event log (for custom asserts)
    pub exit_code: i32,
}

impl ApplyRun {
    pub fn assert_succeeded(self) -> Self;             // exit 0 + no per-op error
    pub fn assert_failed_at(self, op_label: &str) -> Self;
    pub fn assert_no_changes(self) -> Self;            // ResourceChangesComplete { has_changes: false }
    pub async fn assert_idempotent(self) -> Self;      // re-apply, expect no_changes
}
```

The intent is the same fluent shape as NixOS VM tests' `machine.succeed(...)`
/ `machine.fail(...)` / `machine.wait_for_unit(...)`, but with the host-side
half kept Rust-typed so refactors in `Machine`, `AppUpdate`, etc. break tests
at compile time rather than at run time.

### How `apply_plan` actually runs

NixOS VM tests run their workload *inside* the guest because the guest is
the system under test. Same here:

1. The host workspace builds `lusid-apply` (release, target =
   guest's arch). For v1, that means the test machine's arch must match the
   guest arch (x86_64 → x86_64). Cross-arch is a v2 problem.
2. The driver SFTPs the binary to `/tmp/lusid-apply-<run-id>` and the plan
   directory to `/tmp/lusid-plan-<run-id>/`.
3. `Ssh::command` execs:
   ```
   sudo /tmp/lusid-apply-<run-id> \
     --root /tmp/lusid-plan-<run-id> \
     --plan /tmp/lusid-plan-<run-id>/<plan>.lusid \
     --log info \
     [--params '...']
   ```
4. The guest's stdout (NDJSON `AppUpdate`s) is read line-by-line into
   `AppView::update`; stderr (tracing logs) is captured for failure messages.
5. On exit, `ApplyRun` carries the final folded `AppView` plus the raw event
   log, both available for assertion.

This gives every assertion the same view the TUI has — same protocol, same
parser. `assert_idempotent` is the killer use case: re-running the plan and
checking `view.has_changes() == Some(false)` would have caught
`change()`-returns-`Some` bugs in `apt-repo` / `git` resources had they
existed.

### Lifecycle and isolation

- Each node uses an `instance_id` of `vm-test-<crate>-<test-fn>-<node-name>`.
  The `vm` crate is already idempotent on `instance_id`, so a re-run of the
  same test reuses the same overlay and the same forwarded port, which keeps
  the inner loop fast (~5s instead of ~60s for first boot).
- A `--clean` mode (`LUSID_VM_TEST_CLEAN=1`) removes every instance dir
  matching `vm-test-*` before the run. CI sets this; local dev does not.
- The `Driver` keeps a `Vec<Vm>` and on `Drop` issues `Vm::stop` on each.
  qcow2 overlays survive (cheap re-use). Nothing in `/`-land of the host or
  the guest leaks between tests.
- Per-test `tracing` spans wrap each `apply_plan` so failures point at the
  right node + plan in test output.

### Multi-node tests

For tests that need more than one VM (e.g. one node provisions an apt mirror,
another consumes it), `Driver::nodes` boots all of them in parallel. QEMU's
default user-mode networking already gives each guest outbound NAT; for
guest-to-guest networking a second pass would wire a shared `vlan` socket
between the qemu instances (out of scope for v1, but the API is shaped to
allow it without a breaking change).

### Snapshots

NixOS VM tests use QMP `savevm`/`loadvm` to snapshot a machine and roll back
between sub-tests. `lusid-vm` already exposes a QMP socket
([`vm/src/qemu/mod.rs`](../vm/src/qemu/mod.rs)) but doesn't speak it yet; v1
gets isolation purely from per-test instance dirs (slower but simpler), and
`Node::snapshot()` / `Node::restore()` slot in later behind that QMP socket
without changing the public test surface.

## Test discovery and execution

Tests are plain `#[lusid_vm_test]` functions in any crate's `tests/`
directory. They run under `cargo test`, gated by an env var so casual
contributors don't trip on missing QEMU/KVM/sudo:

```
RUN_VM_TESTS=1 cargo test -p lusid-vm-test --test apt_repo
```

Without `RUN_VM_TESTS`, the macro emits a `#[ignore]`-equivalent body that
logs `vm test skipped: set RUN_VM_TESTS=1 to enable`. CI sets the env var on
the QEMU-capable runner; PR checks on non-virt runners stay green.

A small `justfile` recipe (added later, not in v1) wraps the env var, image
prefetch, and a `--keep` flag (`LUSID_VM_TEST_KEEP=1`) that skips teardown
so a failing test leaves a poke-able VM behind.

## Failure ergonomics

- Every assertion captures the SSH command, exit code, last 200 lines of
  stdout, and last 200 lines of stderr into the panic message. Inspired by
  NixOS VM tests, where a failed `succeed` dumps the log immediately — no
  separate "go re-run with debug" step.
- On test failure the driver writes `~/.cache/lusid/vm/instances/<id>/last-run.log`
  containing the full NDJSON `AppUpdate` stream + per-command transcripts,
  so post-mortem doesn't require re-running the whole test.
- `Node::tail_journal(unit, lines)` is provided as a debug helper so an
  assertion's panic message can include systemd journal context.

## What this does **not** try to be (in v1)

- **A property/fuzz tester.** Tests are deterministic and named.
- **A multi-arch matrix runner.** Host arch == guest arch (x86_64). Adding
  aarch64 means cross-compiling `lusid-apply`, which is solvable but punted.
- **A snapshot diffing UI.** Failures dump text, not screenshots. The QMP
  socket is there if we want screenshots later (NixOS-style OCR is unlikely
  to pay off here — lusid is headless).
- **A replacement for unit tests.** Anything that doesn't actually need a
  VM (pure rendering, schema validation, change-computation) stays in the
  per-crate test modules. VM tests are reserved for genuine end-to-end
  behaviour.

## Concrete v1 milestones

1. **Crate scaffolding** — `Cargo.toml`, `src/lib.rs` with `Driver`/`Node`/
   `ApplyRun` skeletons, no networking yet.
2. **Single-node `apply_plan` + `assert_succeeded`** — boots one Debian 12
   VM, scp's the plan + binary, runs apply, parses NDJSON, asserts exit 0.
3. **File / command assertion helpers** — `assert_file_exists`, `read_file`,
   `assert_command_succeeds` etc. via `Ssh::command`.
4. **`#[lusid_vm_test]` proc macro** — separate `lusid-vm-test-macros` crate
   so the runtime crate stays compile-friendly. Handles `RUN_VM_TESTS` gate
   + `Driver` injection.
5. **`assert_idempotent`** — use case that motivated the crate; first
   regression test target is the new `@core/apt-repo`.
6. **CI wiring** — a single workflow job on a QEMU-capable runner with
   `RUN_VM_TESTS=1`. Cache the base qcow2 between runs.

Items 1–5 are the MVP; 6 unlocks the "stops regressions" payoff.

## References

- [NixOS VM tests](https://wiki.nixos.org/wiki/NixOS_VM_tests) — declarative
  multi-machine integration testing in the Nix ecosystem; the inspiration
  for the test shape and the driver vocabulary.
- [`testers.runNixOSTest`](https://nixos.org/manual/nixos/stable/index.html#sec-nixos-tests)
  — the underlying NixOS module; informative for how snapshots and
  multi-machine networking are exposed to the test author.
- [Ansible Molecule](https://ansible.readthedocs.io/projects/molecule/) and
  [Salt's kitchen-salt](https://github.com/saltstack/kitchen-salt) — adjacent
  prior art for "test a configuration tool against a real machine"; less
  declarative but worth borrowing the assertion-helper vocabulary from.
