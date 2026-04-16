//! Runtime harness invoked by the `#[lusid_vm_test]` attribute macro.
//!
//! The macro expands to something like:
//! ```ignore
//! #[test]
//! fn my_test() {
//!     ::lusid_vm_test::__test_runner::run(
//!         env!("CARGO_PKG_NAME"),
//!         "my_test",
//!         |driver| async move { /* original body */ },
//!     );
//! }
//! ```
//!
//! This module owns:
//! - Gating on `RUN_VM_TESTS` so default `cargo test` runs stay QEMU-free.
//! - Initialising `tracing_subscriber` once per process so boot logs surface
//!   when a test is run under `cargo test -- --nocapture`.
//! - Constructing the `tokio` runtime (multi-thread, so concurrent nodes work).
//! - Building the [`Driver`] and handing it to the user's async body.
//!
//! Keeping this logic in a runtime crate rather than inside the macro keeps
//! the macro tiny (easier to read/maintain) and means we can change boot
//! behaviour without a macro-crate version bump.

use std::future::Future;
use std::sync::Once;

use tracing_subscriber::EnvFilter;

use crate::driver::Driver;

/// Env var that, when set to `1`, enables VM tests. Unset / anything else =>
/// tests log-and-skip. Matches the pattern used by `ignore`d tests elsewhere
/// but gives us a single switch so CI can opt in without editing sources.
const GATE_ENV: &str = "RUN_VM_TESTS";

static TRACING_INIT: Once = Once::new();

/// Install a default tracing subscriber (once per process) that honours
/// `RUST_LOG`. Default filter = `info` so boot/SSH/apply lifecycle shows up
/// without extra flags when running `cargo test -- --nocapture`.
fn init_tracing() {
    TRACING_INIT.call_once(|| {
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,russh=warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

/// Entry point called by the `#[lusid_vm_test]` macro. Blocks the current
/// thread until the test future completes. Panics from the body propagate
/// (which is what `cargo test` wants — a panic marks the test as failed).
///
/// `crate_name` and `test_name` are used to namespace the VM instance dir so
/// concurrent tests from different crates don't collide.
pub fn run<F, Fut>(crate_name: &str, test_name: &str, body: F)
where
    F: FnOnce(Driver) -> Fut,
    Fut: Future<Output = ()>,
{
    if !gate_enabled() {
        eprintln!("skipping VM test {crate_name}::{test_name}: set {GATE_ENV}=1 to run",);
        return;
    }

    init_tracing();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime for VM test");

    runtime.block_on(async move {
        let driver = Driver::new(crate_name, test_name)
            .await
            .expect("failed to construct Driver");
        body(driver).await;
    });
}

fn gate_enabled() -> bool {
    matches!(std::env::var(GATE_ENV).as_deref(), Ok("1"))
}
