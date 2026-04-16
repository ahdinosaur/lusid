//! Per-test orchestrator: owns the [`BaseContext`] every node shares and tracks
//! the [`Node`]s spawned during a single test. Nodes are not torn down on
//! drop — leaving qemu running between tests is the fast-iteration default
//! (the next call to [`Driver::node`] with the same name reuses the overlay
//! and forwarded port). Set `LUSID_VM_TEST_CLEAN=1` to wipe matching
//! `vm-test-*` instance directories before the test starts; this is what CI
//! sets to get fresh-VM-per-test isolation.

use std::path::PathBuf;

use lusid_ctx::{Context as BaseContext, ContextError};
use lusid_machine::Machine;
use lusid_vm::{VmError, VmOptions};
use thiserror::Error;
use tracing::{info, warn};

use crate::node::{Node, NodeError};

/// Env var that, when set to `1`, wipes any pre-existing `vm-test-*`
/// instance directories before the test begins.
const CLEAN_ENV: &str = "LUSID_VM_TEST_CLEAN";

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("failed to create base context: {0}")]
    Context(#[from] ContextError),

    #[error("failed to boot VM '{name}': {source}")]
    Boot {
        name: String,
        #[source]
        source: VmError,
    },

    #[error(transparent)]
    Node(#[from] NodeError),

    #[error("failed to clean instance dir {path}: {source}")]
    Clean {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("duplicate node name: {0}")]
    DuplicateNode(String),
}

/// Per-test orchestrator. Created by the `#[lusid_vm_test]` macro; tests
/// receive it as an argument and use it to spawn one or more [`Node`]s.
pub struct Driver {
    ctx: BaseContext,
    /// `vm-test-<crate>-<test>-` — prepended to each node's name to form the
    /// `instance_id` handed to `lusid-vm`.
    instance_prefix: String,
    nodes: Vec<String>,
}

impl Driver {
    /// Construct a driver scoped to a single test. The test's crate and
    /// function name namespace the instance ids so concurrent tests in the
    /// same workspace don't collide.
    pub async fn new(crate_name: &str, test_name: &str) -> Result<Self, DriverError> {
        let mut ctx = BaseContext::create(&workspace_root())?;
        let instance_prefix = format!("vm-test-{}-{}-", sanitize(crate_name), sanitize(test_name));

        if matches!(std::env::var(CLEAN_ENV).as_deref(), Ok("1")) {
            clean_matching_instances(&mut ctx, &instance_prefix).await?;
        }

        Ok(Self {
            ctx,
            instance_prefix,
            nodes: Vec::new(),
        })
    }

    /// Boot a VM with this name and return a connected [`Node`]. Names must be
    /// unique within a single test; repeated calls with the same name return
    /// an error rather than silently aliasing — even though `lusid-vm` itself
    /// is idempotent on `instance_id`, two `Node` handles to one VM would have
    /// independent SSH sessions and that's a footgun.
    pub async fn node(&mut self, name: &str, machine: Machine) -> Result<Node, DriverError> {
        if self.nodes.iter().any(|n| n == name) {
            return Err(DriverError::DuplicateNode(name.to_owned()));
        }

        let instance_id = format!("{}{}", self.instance_prefix, sanitize(name));
        info!(name, instance_id, "booting node");

        let vm = lusid_vm::Vm::run(
            &mut self.ctx,
            VmOptions {
                instance_id: &instance_id,
                machine: &machine,
                ports: Vec::new(),
            },
        )
        .await
        .map_err(|source| DriverError::Boot {
            name: name.to_owned(),
            source,
        })?;

        let node = Node::connect(name.to_owned(), vm).await?;
        self.nodes.push(name.to_owned());
        Ok(node)
    }
}

/// Sanitize a name for use in a file path / instance id: keep alphanumerics,
/// underscores, dashes; replace everything else with `-`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Workspace root, used as the [`BaseContext`] root. Mirrors how `lusid-apply`
/// constructs its context: the root is only relevant for resolving
/// `HostPath` params, which the VM-test driver itself doesn't deal in.
fn workspace_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vm-test crate has a parent")
        .to_path_buf()
}

/// Best-effort cleanup of `<data_dir>/vm/instances/<prefix>*`: kill qemu (if a
/// `qemu.pid` is present) and remove the dir. Errors during the kill are
/// logged but not fatal — the dir removal still proceeds.
async fn clean_matching_instances(_ctx: &mut BaseContext, prefix: &str) -> Result<(), DriverError> {
    let instances_dir = match instances_dir() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "could not resolve instances dir; skipping clean");
            return Ok(());
        }
    };
    if !instances_dir.exists() {
        return Ok(());
    }

    let mut entries = match tokio::fs::read_dir(&instances_dir).await {
        Ok(e) => e,
        Err(e) => {
            return Err(DriverError::Clean {
                path: instances_dir,
                source: e,
            });
        }
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| DriverError::Clean {
            path: instances_dir.clone(),
            source,
        })?
    {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(prefix) {
            continue;
        }
        let path = entry.path();
        kill_pid_in_dir(&path).await;
        info!(path = %path.display(), "removing stale instance dir");
        if let Err(source) = tokio::fs::remove_dir_all(&path).await {
            return Err(DriverError::Clean { path, source });
        }
    }
    Ok(())
}

async fn kill_pid_in_dir(instance_dir: &std::path::Path) {
    let pid_path = instance_dir.join("qemu.pid");
    let Ok(pid_str) = tokio::fs::read_to_string(&pid_path).await else {
        return;
    };
    let Ok(pid_int) = pid_str.trim().parse::<i32>() else {
        warn!(path = %pid_path.display(), "qemu.pid did not parse as i32");
        return;
    };
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    match kill(Pid::from_raw(pid_int), Some(Signal::SIGKILL)) {
        Ok(()) => info!(pid = pid_int, "killed stale qemu"),
        // ESRCH = no such process. Common case after a host reboot.
        Err(nix::errno::Errno::ESRCH) => {}
        Err(e) => warn!(pid = pid_int, error = %e, "failed to kill stale qemu"),
    }
}

/// Mirror of `lusid_vm::Paths::instances_dir` — kept duplicate because the
/// `Paths` type is private to the `lusid-vm` crate. If that gets exposed,
/// switch to the public API.
fn instances_dir() -> Result<PathBuf, lusid_ctx::PathsError> {
    let paths = lusid_ctx::Paths::create()?;
    Ok(paths.data_dir().join("vm/instances"))
}
