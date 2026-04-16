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
pub enum SystemdServiceOperation {
    Enable { name: String },
    Disable { name: String },
    Start { name: String },
    Stop { name: String },
}

impl Display for SystemdServiceOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemdServiceOperation::Enable { name } => write!(f, "SystemdService::Enable({name})"),
            SystemdServiceOperation::Disable { name } => {
                write!(f, "SystemdService::Disable({name})")
            }
            SystemdServiceOperation::Start { name } => write!(f, "SystemdService::Start({name})"),
            SystemdServiceOperation::Stop { name } => write!(f, "SystemdService::Stop({name})"),
        }
    }
}

impl_display_render!(SystemdServiceOperation);

#[derive(Error, Debug)]
pub enum SystemdServiceApplyError {
    #[error(transparent)]
    Command(#[from] CommandError),
}

#[derive(Debug, Clone)]
pub struct SystemdService;

#[async_trait]
impl OperationType for SystemdService {
    type Operation = SystemdServiceOperation;

    // Note(cc): merge is a no-op. `systemctl enable|start` accepts multiple units but
    // the operations here are per-verb-per-unit; coalescing would save at most a fork
    // per service, which isn't worth the extra complexity while plans manage a handful
    // of services at a time. Revisit if plans start listing dozens of systemd units.
    fn merge(operations: Vec<Self::Operation>) -> Vec<Self::Operation> {
        operations
    }

    type ApplyOutput = Pin<Box<dyn Future<Output = Result<(), Self::ApplyError>> + Send + 'static>>;
    type ApplyError = SystemdServiceApplyError;
    type ApplyStdout = ChildStdout;
    type ApplyStderr = ChildStderr;

    async fn apply(
        _ctx: &mut Context,
        operation: &Self::Operation,
    ) -> Result<(Self::ApplyOutput, Self::ApplyStdout, Self::ApplyStderr), Self::ApplyError> {
        let (verb, name) = match operation {
            SystemdServiceOperation::Enable { name } => ("enable", name),
            SystemdServiceOperation::Disable { name } => ("disable", name),
            SystemdServiceOperation::Start { name } => ("start", name),
            SystemdServiceOperation::Stop { name } => ("stop", name),
        };
        info!("[systemd-service] {verb}: {name}");

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
