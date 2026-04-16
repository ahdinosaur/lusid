//! End-to-end VM test for the `@core/apt-repo` resource.
//!
//! Boots a Debian 13 guest, applies a plan that installs Docker's apt repo,
//! and asserts the two expected files land on disk with the right mode.
//! A second apply asserts idempotency — no further changes should be emitted.
//!
//! Gated on `RUN_VM_TESTS=1` (see `crate::runner`) so default `cargo test`
//! runs don't boot qemu.

use std::path::PathBuf;

use lusid_vm_test::{Driver, lusid_vm_test, presets};

fn example_plan() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vm-test crate has a parent (workspace root)")
        .join("examples")
        .join("apt-repo-docker.lusid")
}

#[lusid_vm_test]
async fn apt_repo_installs_docker_sources(mut driver: Driver) {
    let node = driver
        .node("host", presets::debian_13("apt-repo-host"))
        .await
        .expect("failed to boot host node");

    let plan = example_plan();

    // First apply — should create both files from scratch.
    node.apply_plan(&plan)
        .await
        .assert_succeeded()
        .assert_idempotent()
        .await;

    node.assert_file_exists("/etc/apt/sources.list.d/docker.sources")
        .await;
    node.assert_file_exists("/etc/apt/keyrings/docker.asc")
        .await;
    node.assert_file_mode("/etc/apt/keyrings/docker.asc", 0o644)
        .await;
    node.assert_file_mode("/etc/apt/sources.list.d/docker.sources", 0o644)
        .await;

    // Sanity: the `Signed-By:` directive points at the keyring we wrote.
    let sources = node
        .read_file("/etc/apt/sources.list.d/docker.sources")
        .await
        .expect("read sources file");
    let sources = String::from_utf8(sources).expect("sources file is utf-8");
    assert!(
        sources.contains("Signed-By: /etc/apt/keyrings/docker.asc"),
        "sources file missing Signed-By directive:\n{sources}",
    );
}
