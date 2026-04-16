//! One running VM with an authenticated SSH session and helpers for the kind
//! of post-conditions a plan-test wants to assert: file presence/contents,
//! command exit codes, systemd unit state.
//!
//! Helpers split into two flavours:
//! - **assert_*** — panic on mismatch with a diagnostic message that includes
//!   stdout/stderr from the offending command. Failure messages aim to be
//!   self-contained so a CI log is enough to root-cause without re-running.
//! - **non-asserting** (`run`, `read_file`) — return the data and let the
//!   caller decide.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lusid_ssh::{Ssh, SshCommandHandle, SshConnectOptions, SshError, SshKeypair, SshVolume};
use lusid_vm::{Vm, VmError};
use russh::client::Config;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::apply::{ApplyOptions, ApplyRun, run_apply};

/// SSH connect retry/backoff budget. The VM has just booted; auth might race
/// the server's startup briefly. 30s is generous for a warm boot.
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum NodeError {
    #[error(transparent)]
    Vm(#[from] VmError),

    #[error(transparent)]
    Ssh(#[from] SshError),

    #[error("ssh command exited with {code}: {command}")]
    NonZeroExit {
        command: String,
        code: i32,
        stdout: String,
        stderr: String,
    },

    #[error("io error reading {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
}

/// Output of a one-shot remote command via [`Node::run`].
#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RemoteOutput {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }
}

/// One booted VM. Holds a single long-lived SSH session, lazily reconnected
/// if it drops. Cheap to clone via the underlying `Arc` if a test wants to
/// hand a node off to a helper.
#[derive(Clone)]
pub struct Node {
    inner: Arc<NodeInner>,
}

struct NodeInner {
    name: String,
    vm: Vm,
    ssh: Mutex<Ssh>,
}

impl Node {
    pub(crate) async fn connect(name: String, vm: Vm) -> Result<Self, NodeError> {
        let ssh = open_ssh(&vm).await?;
        Ok(Self {
            inner: Arc::new(NodeInner {
                name,
                vm,
                ssh: Mutex::new(ssh),
            }),
        })
    }

    /// Node name as passed to [`crate::Driver::node`].
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Underlying VM handle. Useful for tests that want to e.g. snapshot the
    /// SSH port or ask the VM to stop.
    pub fn vm(&self) -> &Vm {
        &self.inner.vm
    }

    // ── command execution ────────────────────────────────────────────────

    /// Run a shell command on the node. Captures stdout + stderr to memory and
    /// returns the exit code; **does not** assert success — use
    /// [`Self::assert_command_succeeds`] for that.
    pub async fn run(&self, command: &str) -> Result<RemoteOutput, NodeError> {
        let mut ssh = self.inner.ssh.lock().await;
        let handle = ssh.command(command).await?;
        collect_output(handle, command).await
    }

    /// Run and assert exit code 0. Panics with stdout + stderr on failure.
    pub async fn assert_command_succeeds(&self, command: &str) {
        let out = self.run(command).await.expect("ssh command failed");
        if !out.succeeded() {
            panic!("{}", format_failure("expected success", &out));
        }
    }

    /// Run and assert non-zero exit. Panics if the command unexpectedly
    /// succeeded.
    pub async fn assert_command_fails(&self, command: &str) {
        let out = self.run(command).await.expect("ssh command failed");
        if out.succeeded() {
            panic!("{}", format_failure("expected failure", &out));
        }
    }

    /// Re-run `command` until it exits 0 or `timeout` elapses. Panics on
    /// timeout. Useful for waiting on services to come up.
    pub async fn wait_until_succeeds(&self, command: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut last: Option<RemoteOutput> = None;
        loop {
            let out = self.run(command).await.expect("ssh command failed");
            if out.succeeded() {
                return;
            }
            if Instant::now() >= deadline {
                let last = last.unwrap_or(out);
                panic!(
                    "{}",
                    format_failure(&format!("timeout after {:?}", timeout), &last)
                );
            }
            last = Some(out);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // ── file assertions ──────────────────────────────────────────────────

    /// Read a file on the node. Uses `sudo cat` so root-owned files work too.
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, NodeError> {
        let cmd = format!("sudo -n cat {}", shell_quote(path));
        let mut ssh = self.inner.ssh.lock().await;
        let handle = ssh.command(&cmd).await?;
        let (stdout, stderr, exit_code) = drain_handle(handle).await?;

        if exit_code != 0 {
            return Err(NodeError::NonZeroExit {
                command: cmd,
                code: exit_code,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr,
            });
        }
        Ok(stdout)
    }

    /// Assert a file exists, regardless of contents.
    pub async fn assert_file_exists(&self, path: &str) {
        let cmd = format!("sudo -n test -e {}", shell_quote(path));
        let out = self.run(&cmd).await.expect("ssh command failed");
        if !out.succeeded() {
            panic!(
                "expected file to exist: {path}\n--- stderr ---\n{}",
                out.stderr
            );
        }
    }

    /// Assert a file does **not** exist.
    pub async fn assert_file_absent(&self, path: &str) {
        let cmd = format!("sudo -n test -e {}", shell_quote(path));
        let out = self.run(&cmd).await.expect("ssh command failed");
        if out.succeeded() {
            panic!("expected file to be absent: {path}");
        }
    }

    /// Assert a file's bytes match `expected` exactly.
    pub async fn assert_file_contents(&self, path: &str, expected: &[u8]) {
        let actual = self
            .read_file(path)
            .await
            .unwrap_or_else(|e| panic!("read_file({path}): {e}"));
        if actual != expected {
            panic!(
                "file contents mismatch: {path}\n--- expected ({} bytes) ---\n{}\n\
                 --- actual ({} bytes) ---\n{}",
                expected.len(),
                String::from_utf8_lossy(expected),
                actual.len(),
                String::from_utf8_lossy(&actual),
            );
        }
    }

    /// Assert a file's mode matches `mode` (low 12 bits of stat.st_mode).
    /// Compares `stat -c %a` so the mode is the same shape a user types.
    pub async fn assert_file_mode(&self, path: &str, mode: u32) {
        let cmd = format!("sudo -n stat -c %a {}", shell_quote(path));
        let out = self.run(&cmd).await.expect("ssh command failed");
        if !out.succeeded() {
            panic!("stat failed for {path}\n--- stderr ---\n{}", out.stderr);
        }
        let observed = u32::from_str_radix(out.stdout.trim(), 8).unwrap_or_else(|e| {
            panic!(
                "could not parse mode '{}' from stat: {e}",
                out.stdout.trim()
            )
        });
        if observed != mode {
            panic!(
                "file mode mismatch: {path} expected {:o}, got {:o}",
                mode, observed
            );
        }
    }

    // ── service assertions ───────────────────────────────────────────────

    /// Assert a systemd unit is `active`. Uses `systemctl is-active`, which
    /// returns 0 only for active units.
    pub async fn assert_unit_active(&self, unit: &str) {
        let cmd = format!("sudo -n systemctl is-active {}", shell_quote(unit));
        let out = self.run(&cmd).await.expect("ssh command failed");
        if !out.succeeded() {
            panic!(
                "expected unit to be active: {unit} (got '{}')",
                out.stdout.trim()
            );
        }
    }

    // ── plan execution ───────────────────────────────────────────────────

    /// Run `lusid-apply` against `plan` on this node. See [`ApplyRun`] for
    /// what the returned object exposes. Forwards to [`Self::apply_plan_with`]
    /// with default options.
    pub async fn apply_plan(&self, plan: impl AsRef<std::path::Path>) -> ApplyRun {
        self.apply_plan_with(plan, ApplyOptions::default()).await
    }

    /// Variant of [`Self::apply_plan`] with explicit [`ApplyOptions`] (params,
    /// log level, …).
    pub async fn apply_plan_with(
        &self,
        plan: impl AsRef<std::path::Path>,
        options: ApplyOptions,
    ) -> ApplyRun {
        let plan = plan.as_ref().to_path_buf();
        let mut ssh = self.inner.ssh.lock().await;
        run_apply(&mut ssh, self.clone(), plan, options)
            .await
            .expect("apply_plan failed")
    }

    // ── SFTP helpers (used by apply, exposed for tests that want them) ───

    /// Upload a local file to `remote_path` on the node, via SFTP.
    pub async fn upload_file(
        &self,
        local: impl AsRef<std::path::Path>,
        remote_path: &str,
    ) -> Result<(), NodeError> {
        let mut ssh = self.inner.ssh.lock().await;
        ssh.sync(SshVolume::FilePath {
            local: local.as_ref().to_path_buf(),
            remote: remote_path.to_owned(),
        })
        .await?;
        Ok(())
    }

    /// Upload raw bytes to `remote_path` with optional unix mode bits.
    pub async fn upload_bytes(
        &self,
        bytes: Vec<u8>,
        remote_path: &str,
        mode: Option<u32>,
    ) -> Result<(), NodeError> {
        let mut ssh = self.inner.ssh.lock().await;
        ssh.sync(SshVolume::FileBytes {
            local: bytes,
            permissions: mode,
            remote: remote_path.to_owned(),
        })
        .await?;
        Ok(())
    }
}

async fn open_ssh(vm: &Vm) -> Result<Ssh, NodeError> {
    let keypair: SshKeypair = vm.ssh_keypair().await?;
    let addr = format!("127.0.0.1:{}", vm.ssh_port);
    let options = SshConnectOptions {
        private_key: keypair.private_key,
        addrs: addr.clone(),
        username: vm.user.clone(),
        config: Arc::new(Config::default()),
        timeout: SSH_CONNECT_TIMEOUT,
    };
    info!(node = %vm.id, addr, user = %vm.user, "opening ssh");
    let ssh = Ssh::connect(options).await?;
    debug!(node = %vm.id, "ssh ready");
    Ok(ssh)
}

/// Drain stdout + stderr to in-memory buffers and wait for exit.
///
/// Takes the handle by value + destructures so each stream can be moved into
/// its own task (the fields are distinct allocations, but `&mut handle.stdout`
/// and `&mut handle.stderr` from the same `&mut handle` aliasing-conflict
/// across a `tokio::try_join!`).
async fn drain_handle(handle: SshCommandHandle) -> Result<(Vec<u8>, String, i32), NodeError> {
    let SshCommandHandle {
        stdout,
        stderr,
        mut channel,
        command: _,
    } = handle;

    let stdout_task = async move {
        let mut buf = Vec::new();
        let mut s = stdout;
        s.read_to_end(&mut buf)
            .await
            .map_err(|source| NodeError::Io {
                what: "stdout",
                source,
            })?;
        Ok::<Vec<u8>, NodeError>(buf)
    };
    let stderr_task = async move {
        let mut buf = Vec::new();
        let mut s = stderr;
        s.read_to_end(&mut buf)
            .await
            .map_err(|source| NodeError::Io {
                what: "stderr",
                source,
            })?;
        Ok::<String, NodeError>(String::from_utf8_lossy(&buf).into_owned())
    };
    let (stdout_buf, stderr_buf) = tokio::try_join!(stdout_task, stderr_task)?;
    let exit_code = channel.wait().await?.map(|c| c as i32).unwrap_or(-1);
    Ok((stdout_buf, stderr_buf, exit_code))
}

/// Like [`drain_handle`] but wraps the result as a [`RemoteOutput`] with the
/// originating command string.
async fn collect_output(
    handle: SshCommandHandle,
    command: &str,
) -> Result<RemoteOutput, NodeError> {
    let (stdout_buf, stderr_buf, exit_code) = drain_handle(handle).await?;
    Ok(RemoteOutput {
        command: command.to_owned(),
        exit_code,
        stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
        stderr: stderr_buf,
    })
}

/// Quote a string for safe interpolation into a `sh -c` command. Wraps in
/// single quotes and escapes any embedded single quotes the POSIX way:
/// `'…'\''…'`. Good enough for paths and unit names; not a general shell
/// quoter.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn format_failure(reason: &str, out: &RemoteOutput) -> String {
    format!(
        "command {reason}: `{}`\n  exit: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.command, out.exit_code, out.stdout, out.stderr,
    )
}
