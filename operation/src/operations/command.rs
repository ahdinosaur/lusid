use async_trait::async_trait;
use lusid_cmd::{Command as RunCommand, CommandError as RunCommandError};
use lusid_ctx::Context;
use lusid_view::impl_display_render;
use std::{fmt::Display, pin::Pin, str::FromStr};
use thiserror::Error;
use tokio::process::{ChildStderr, ChildStdout};
use tracing::info;

use crate::OperationType;

#[derive(Debug, Clone)]
pub struct CommandOperation {
    pub command: String,
}

impl Display for CommandOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let CommandOperation { command } = self;
        write!(f, "Command({command})")
    }
}

impl_display_render!(CommandOperation);

#[derive(Error, Debug)]
pub enum CommandApplyError {
    #[error("failed to parse command: {0}")]
    ParseCommand(#[source] <RunCommand as FromStr>::Err),

    #[error(transparent)]
    RunCommand(#[from] RunCommandError),
}

#[derive(Debug, Clone)]
pub struct Command;

#[async_trait]
impl OperationType for Command {
    type Operation = CommandOperation;

    fn merge(operations: Vec<Self::Operation>) -> Vec<Self::Operation> {
        operations
    }

    type ApplyOutput = Pin<Box<dyn Future<Output = Result<(), Self::ApplyError>> + Send + 'static>>;
    type ApplyError = CommandApplyError;
    type ApplyStdout = ChildStdout;
    type ApplyStderr = ChildStderr;

    async fn apply(
        _ctx: &mut Context,
        operation: &Self::Operation,
    ) -> Result<(Self::ApplyOutput, Self::ApplyStdout, Self::ApplyStderr), Self::ApplyError> {
        let CommandOperation { command } = operation;
        info!("[command] run: {command}");

        let mut cmd = RunCommand::from_str(command).map_err(CommandApplyError::ParseCommand)?;
        let output = cmd.output().await?;
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
