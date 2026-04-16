//! Run `lusid-apply` on a guest and parse the structured output.
//!
//! The flow per [`run_apply`] call:
//!
//! 1. Resolve the host's `lusid-apply` binary via [`crate::binary`].
//! 2. Pick a unique remote run dir (`/tmp/lusid-vm-test-<run-id>/`).
//! 3. SFTP the binary, `chmod +x`, then SFTP the plan file in.
//! 4. SSH-exec `sudo -n /…/lusid-apply --root … --plan …` and read line-by-line:
//!    - stdout → `serde_json::from_str::<AppUpdate>` → fold into [`AppView`]
//!    - stderr → captured to a string (tracing logs from the guest binary).
//! 5. Wait for exit; assemble [`ApplyRun`].
//!
//! Per-run dir is left in place on exit so a failed run can be poked
//! manually; the next `run_apply` mints a new run-id, so nothing collides.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lusid_apply_stdio::{AppUpdate, AppView};
use lusid_ssh::{Ssh, SshCommandHandle, SshError, SshVolume};
use serde_json::Value as JsonValue;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing::{debug, info, warn};

use crate::binary::{BinaryError, locate_lusid_apply};
use crate::node::{Node, NodeError};

/// Knobs for [`crate::Node::apply_plan_with`]. `Default` matches
/// `lusid-apply`'s own defaults (`--log info`, no params).
#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    /// JSON value passed to `lusid-apply --params` (validated against the
    /// plan's params schema).
    pub params: Option<JsonValue>,

    /// `--log` argument, e.g. `"info"`, `"debug"`. `None` => omit (keeps
    /// `lusid-apply`'s built-in default of `info`).
    pub log: Option<String>,
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error(transparent)]
    Binary(#[from] BinaryError),

    #[error(transparent)]
    Node(#[from] NodeError),

    #[error(transparent)]
    Ssh(#[from] SshError),

    #[error("io error reading {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("plan file not found: {0}")]
    PlanNotFound(PathBuf),

    #[error("invalid AppView transition: {0}")]
    InvalidTransition(#[source] lusid_apply_stdio::AppViewError),

    #[error("invalid params JSON: {0}")]
    Params(#[source] serde_json::Error),
}

/// One run of `lusid apply` on a node. `view` is the final folded
/// [`AppView`], `updates` is the raw event log (useful for custom asserts),
/// and `exit_code` is the binary's exit status (0 on success).
///
/// Holds a clone of the [`Node`] it ran on so chained calls
/// (`assert_idempotent`) can re-run against the same VM without a separate
/// handle.
pub struct ApplyRun {
    pub node: Node,
    pub plan: PathBuf,
    pub options: ApplyOptions,
    pub view: AppView,
    pub updates: Vec<AppUpdate>,
    pub exit_code: i32,
    pub stderr: String,
}

impl ApplyRun {
    /// Whether the binary exited cleanly **and** every applied operation
    /// reported `error: None` in its `OperationApplyComplete`.
    pub fn succeeded(&self) -> bool {
        if self.exit_code != 0 {
            return false;
        }
        !self
            .updates
            .iter()
            .any(|u| matches!(u, AppUpdate::OperationApplyComplete { error: Some(_), .. }))
    }

    /// Whether `lusid-apply` reported "no changes needed" for this run, i.e.
    /// it emitted `ResourceChangesComplete { has_changes: false }`. This is
    /// the post-condition that [`Self::assert_idempotent`] checks for the
    /// second run.
    pub fn had_no_changes(&self) -> bool {
        self.updates
            .iter()
            .any(|u| matches!(u, AppUpdate::ResourceChangesComplete { has_changes: false }))
    }

    // ── assertions ───────────────────────────────────────────────────────

    /// Panic unless [`Self::succeeded`]. Failure message includes the per-op
    /// errors and the captured stderr (tracing logs from the guest binary).
    pub fn assert_succeeded(self) -> Self {
        if self.succeeded() {
            return self;
        }
        let op_errors: Vec<String> = self
            .updates
            .iter()
            .filter_map(|u| match u {
                AppUpdate::OperationApplyComplete {
                    index,
                    error: Some(e),
                } => Some(format!("  op {:?}: {}", index, e)),
                _ => None,
            })
            .collect();
        panic!(
            "lusid-apply on '{}' failed (exit {})\n--- per-op errors ---\n{}\n\
             --- guest stderr ---\n{}",
            self.node.name(),
            self.exit_code,
            if op_errors.is_empty() {
                "  (none)".into()
            } else {
                op_errors.join("\n")
            },
            self.stderr,
        );
    }

    /// Panic unless [`Self::had_no_changes`].
    pub fn assert_no_changes(self) -> Self {
        if self.had_no_changes() {
            return self;
        }
        panic!(
            "expected no changes, but lusid-apply reported has_changes=true \
             (or never emitted ResourceChangesComplete) on '{}'\n\
             --- guest stderr ---\n{}",
            self.node.name(),
            self.stderr,
        );
    }

    /// Re-apply the same plan with the same options on the same node and
    /// assert the second run was a no-op. Catches `change()`-returns-`Some`
    /// idempotency bugs in resources.
    pub async fn assert_idempotent(self) -> Self {
        let node = self.node.clone();
        let second = node.apply_plan_with(&self.plan, self.options.clone()).await;
        let second = second.assert_succeeded();
        if !second.had_no_changes() {
            panic!(
                "plan was not idempotent: second apply still had changes on '{}'\n\
                 --- guest stderr (2nd run) ---\n{}",
                second.node.name(),
                second.stderr,
            );
        }
        // Return the original run so the caller can keep chaining.
        self
    }
}

/// Per-process unique run-id source — used in remote temp dir names.
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_run_id() -> String {
    let n = RUN_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    format!("{ts}-{n}")
}

pub(crate) async fn run_apply(
    ssh: &mut Ssh,
    node: Node,
    plan: PathBuf,
    options: ApplyOptions,
) -> Result<ApplyRun, ApplyError> {
    if !plan.is_file() {
        return Err(ApplyError::PlanNotFound(plan));
    }

    let local_binary = locate_lusid_apply()?;

    let run_id = unique_run_id();
    let run_dir = format!("/tmp/lusid-vm-test-{run_id}");
    let remote_binary = format!("{run_dir}/lusid-apply");
    let plan_file_name = plan
        .file_name()
        .expect("plan path has a file name")
        .to_string_lossy()
        .into_owned();
    let remote_plan = format!("{run_dir}/{plan_file_name}");

    info!(
        node = node.name(),
        run_id,
        binary = %local_binary.display(),
        plan = %plan.display(),
        "preparing apply run on guest"
    );

    upload_local_file(ssh, &local_binary, &remote_binary).await?;
    chmod_exec(ssh, &remote_binary).await?;
    upload_local_file(ssh, &plan, &remote_plan).await?;

    let cmd = build_apply_command(&run_dir, &remote_binary, &remote_plan, &options)?;
    debug!(node = node.name(), cmd, "invoking lusid-apply");

    let handle = ssh.command(&cmd).await?;
    let (view, updates, stderr_buf, exit_code) = drive_apply_handle(handle).await?;

    info!(
        node = node.name(),
        run_id,
        exit_code,
        update_count = updates.len(),
        "apply run finished"
    );

    Ok(ApplyRun {
        node,
        plan,
        options,
        view,
        updates,
        exit_code,
        stderr: stderr_buf,
    })
}

/// Drive a streaming `lusid-apply` handle: own stdout/stderr in disjoint
/// tasks (avoids the SshCommandHandle borrow split problem), fold each
/// stdout line into an `AppView`, and capture stderr verbatim.
async fn drive_apply_handle(
    handle: SshCommandHandle,
) -> Result<(AppView, Vec<AppUpdate>, String, i32), ApplyError> {
    let SshCommandHandle {
        stdout,
        stderr,
        mut channel,
        command: _,
    } = handle;

    let stdout_task = async move {
        let mut updates: Vec<AppUpdate> = Vec::new();
        let mut view = AppView::default();
        let mut invalid_lines: Vec<String> = Vec::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(|source| ApplyError::Io {
            what: "stdout",
            source,
        })? {
            if line.trim().is_empty() {
                continue;
            }
            let update: AppUpdate = match serde_json::from_str(&line) {
                Ok(u) => u,
                Err(e) => {
                    warn!(
                        line = line.as_str(),
                        error = %e,
                        "stdout line was not AppUpdate JSON; capturing"
                    );
                    invalid_lines.push(line);
                    continue;
                }
            };
            view = view
                .update(update.clone())
                .map_err(ApplyError::InvalidTransition)?;
            updates.push(update);
        }
        Ok::<_, ApplyError>((view, updates, invalid_lines))
    };

    let stderr_task = async move {
        let mut bytes = Vec::new();
        BufReader::new(stderr)
            .read_to_end(&mut bytes)
            .await
            .map_err(|source| ApplyError::Io {
                what: "stderr",
                source,
            })?;
        Ok::<_, ApplyError>(String::from_utf8_lossy(&bytes).into_owned())
    };

    let ((view, updates, invalid_lines), mut stderr_buf) =
        tokio::try_join!(stdout_task, stderr_task)?;

    let exit_code = channel.wait().await?.map(|c| c as i32).unwrap_or(-1);

    if !invalid_lines.is_empty() {
        stderr_buf.push_str("\n--- non-JSON stdout lines ---\n");
        for l in &invalid_lines {
            stderr_buf.push_str(l);
            stderr_buf.push('\n');
        }
    }

    Ok((view, updates, stderr_buf, exit_code))
}

async fn upload_local_file(ssh: &mut Ssh, local: &Path, remote: &str) -> Result<(), ApplyError> {
    ssh.sync(SshVolume::FilePath {
        local: local.to_path_buf(),
        remote: remote.to_owned(),
    })
    .await?;
    Ok(())
}

async fn chmod_exec(ssh: &mut Ssh, remote: &str) -> Result<(), ApplyError> {
    let cmd = format!("chmod +x {}", shell_quote(remote));
    let handle = ssh.command(&cmd).await?;
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
            .map_err(|source| ApplyError::Io {
                what: "chmod stdout",
                source,
            })?;
        Ok::<_, ApplyError>(buf)
    };
    let stderr_task = async move {
        let mut buf = Vec::new();
        let mut s = stderr;
        s.read_to_end(&mut buf)
            .await
            .map_err(|source| ApplyError::Io {
                what: "chmod stderr",
                source,
            })?;
        Ok::<_, ApplyError>(buf)
    };
    let (_out, err) = tokio::try_join!(stdout_task, stderr_task)?;
    let exit = channel.wait().await?.unwrap_or(255);
    if exit != 0 {
        return Err(ApplyError::Io {
            what: "chmod",
            source: std::io::Error::other(format!(
                "chmod exit {exit}: {}",
                String::from_utf8_lossy(&err)
            )),
        });
    }
    Ok(())
}

fn build_apply_command(
    run_dir: &str,
    remote_binary: &str,
    remote_plan: &str,
    options: &ApplyOptions,
) -> Result<String, ApplyError> {
    let mut cmd = format!(
        "sudo -n {bin} --root {root} --plan {plan}",
        bin = shell_quote(remote_binary),
        root = shell_quote(run_dir),
        plan = shell_quote(remote_plan),
    );
    if let Some(level) = &options.log {
        cmd.push_str(" --log ");
        cmd.push_str(&shell_quote(level));
    }
    if let Some(params) = &options.params {
        let json = serde_json::to_string(params).map_err(ApplyError::Params)?;
        cmd.push_str(" --params ");
        cmd.push_str(&shell_quote(&json));
    }
    Ok(cmd)
}

/// POSIX single-quote escape for safe interpolation into a `sh -c` command.
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
