//! Locates the host-built `lusid-apply` binary that gets SCP'd into the guest.
//!
//! Resolution order:
//! 1. `LUSID_APPLY_BIN` env var — explicit override, used by CI.
//! 2. `<workspace_root>/target/release/lusid-apply` — the conventional path
//!    from `cargo build --release -p lusid-apply`.
//!
//! No automatic build: a recursive `cargo build` from inside `cargo test` is a
//! lock-contention foot-gun. Tests fail fast with a clear message instead.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum BinaryError {
    #[error(
        "lusid-apply binary not found at {path} (set LUSID_APPLY_BIN, or run `cargo build --release -p lusid-apply`)"
    )]
    NotFound { path: PathBuf },
}

/// Locate the `lusid-apply` binary on the host. See module docs for resolution
/// order.
pub fn locate_lusid_apply() -> Result<PathBuf, BinaryError> {
    if let Some(path) = std::env::var_os("LUSID_APPLY_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            debug!(path = %path.display(), "using LUSID_APPLY_BIN");
            return Ok(path);
        }
        return Err(BinaryError::NotFound { path });
    }

    let path = workspace_target_dir().join("release").join("lusid-apply");
    if path.is_file() {
        debug!(path = %path.display(), "using release lusid-apply");
        return Ok(path);
    }
    Err(BinaryError::NotFound { path })
}

/// `<workspace>/target`, honouring `CARGO_TARGET_DIR` if set. Falls back to
/// `vm-test`'s parent (the workspace root) which is fine for the in-tree case.
fn workspace_target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    workspace_root().join("target")
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at vm-test/. Workspace root is its parent.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vm-test crate has a parent (the workspace root)")
        .to_path_buf()
}
