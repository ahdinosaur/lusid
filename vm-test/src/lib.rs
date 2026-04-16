//! End-to-end testing of lusid plans against ephemeral QEMU VMs.
//!
//! See [`README.md`](https://github.com/ahdinosaur/lusid-2/blob/main/vm-test/README.md)
//! for the full design. The runtime entry points are:
//!
//! - [`Driver`] — boots and tracks per-test VMs.
//! - [`Node`] — one running VM with a connected SSH session, plus assertion
//!   helpers (`assert_file_exists`, `assert_command_succeeds`, …).
//! - [`ApplyRun`] — the result of running `lusid apply` on a node, with
//!   `assert_succeeded` / `assert_idempotent` for plan-level assertions.
//!
//! Tests are written as `#[lusid_vm_test]` async functions and gated on
//! `RUN_VM_TESTS=1` so default `cargo test` runs stay QEMU-free.

mod apply;
mod binary;
mod driver;
mod machines;
mod node;
mod runner;

pub use crate::apply::{ApplyOptions, ApplyRun};
pub use crate::driver::{Driver, DriverError};
pub use crate::node::{Node, NodeError, RemoteOutput};

pub use lusid_machine::{Machine, MachineVmOptions};

/// Re-export of the attribute macro from `lusid-vm-test-macros`.
pub use lusid_vm_test_macros::lusid_vm_test;

/// Pre-built [`Machine`] specs for common targets. Each function takes the
/// hostname to set on the guest.
pub mod presets {
    pub use crate::machines::*;
}

/// Internals invoked by the `#[lusid_vm_test]` macro. Not part of the
/// public API; do not call directly.
#[doc(hidden)]
pub mod __test_runner {
    pub use crate::runner::run;
}
