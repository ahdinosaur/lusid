use async_trait::async_trait;
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_view::impl_display_render;
use std::{fmt::Display, pin::Pin};
use thiserror::Error;
use tokio::process::{ChildStderr, ChildStdout};
use tracing::info;

use crate::OperationType;

#[derive(Debug, Clone)]
pub enum SystemdOperation {
    Enable { name: String },
    Disable { name: String },
    Start { name: String },
    Stop { name: String },
}

impl Display for SystemdOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemdOperation::Enable { name } => write!(f, "Systemd::Enable({name})"),
            SystemdOperation::Disable { name } => {
                write!(f, "Systemd::Disable({name})")
            }
            SystemdOperation::Start { name } => write!(f, "Systemd::Start({name})"),
            SystemdOperation::Stop { name } => write!(f, "Systemd::Stop({name})"),
        }
    }
}

impl_display_render!(SystemdOperation);

#[derive(Error, Debug)]
pub enum SystemdApplyError {
    #[error(transparent)]
    Command(#[from] CommandError),
}

#[derive(Debug, Clone)]
pub struct Systemd;

#[async_trait]
impl OperationType for Systemd {
    type Operation = SystemdOperation;

    // Note(cc): merge is a no-op. `systemctl enable|start` accepts multiple units but
    // the operations here are per-verb-per-unit; coalescing would save at most a fork
    // per unit, which isn't worth the extra complexity while plans manage a handful
    // of units at a time. Revisit if plans start listing dozens of systemd units.
    fn merge(operations: Vec<Self::Operation>) -> Vec<Self::Operation> {
        operations
    }

    type ApplyOutput = Pin<Box<dyn Future<Output = Result<(), Self::ApplyError>> + Send + 'static>>;
    type ApplyError = SystemdApplyError;
    type ApplyStdout = ChildStdout;
    type ApplyStderr = ChildStderr;

    async fn apply(
        _ctx: &mut Context,
        operation: &Self::Operation,
    ) -> Result<(Self::ApplyOutput, Self::ApplyStdout, Self::ApplyStderr), Self::ApplyError> {
        let (verb, name) = match operation {
            SystemdOperation::Enable { name } => ("enable", name),
            SystemdOperation::Disable { name } => ("disable", name),
            SystemdOperation::Start { name } => ("start", name),
            SystemdOperation::Stop { name } => ("stop", name),
        };
        info!("[systemd] {verb}: {name}");

        let mut cmd = Command::new("systemctl");
        cmd.arg("--no-ask-password").arg(verb).arg(name);
        let output = cmd.sudo().output().await?;
        Ok((
            Box::pin(async move {
                output.status.await?;
                Ok(())
            }),
            output.stdout,
            output.stderr,
        ))
    }
}
