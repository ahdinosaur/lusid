use async_trait::async_trait;
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_view::impl_display_render;
use std::{collections::BTreeSet, fmt::Display, pin::Pin};
use thiserror::Error;
use tokio::process::{ChildStderr, ChildStdout};
use tracing::info;

use crate::OperationType;

#[derive(Debug, Clone)]
pub enum BrewOperation {
    Update,
    Install { packages: Vec<String> },
}

impl Display for BrewOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrewOperation::Update => write!(f, "Brew::Update"),
            BrewOperation::Install { packages } => {
                write!(f, "Brew::Install(packages = [{}])", packages.join(", "))
            }
        }
    }
}

impl_display_render!(BrewOperation);

#[derive(Error, Debug)]
pub enum BrewApplyError {
    #[error(transparent)]
    Command(#[from] CommandError),
}

#[derive(Debug, Clone)]
pub struct Brew;

#[async_trait]
impl OperationType for Brew {
    type Operation = BrewOperation;

    fn merge(operations: Vec<Self::Operation>) -> Vec<Self::Operation> {
        let mut update = false;
        let mut install: BTreeSet<String> = BTreeSet::new();

        for operation in operations {
            match operation {
                BrewOperation::Update => {
                    update = true;
                }
                BrewOperation::Install { packages } => {
                    for package in packages {
                        install.insert(package);
                    }
                }
            }
        }

        let mut operations = Vec::new();
        if update {
            operations.push(BrewOperation::Update);
        }
        if !install.is_empty() {
            operations.push(BrewOperation::Install {
                packages: install.into_iter().collect(),
            })
        }
        operations
    }

    type ApplyOutput = Pin<Box<dyn Future<Output = Result<(), Self::ApplyError>> + Send + 'static>>;
    type ApplyError = BrewApplyError;
    type ApplyStdout = ChildStdout;
    type ApplyStderr = ChildStderr;

    async fn apply(
        _ctx: &mut Context,
        operation: &Self::Operation,
    ) -> Result<(Self::ApplyOutput, Self::ApplyStdout, Self::ApplyStderr), Self::ApplyError> {
        match operation {
            BrewOperation::Update => {
                info!("[brew] update");
                // Homebrew refuses to run as root, so intentionally *not* wrapped in
                // `sudo()` — the invoking user must own the Homebrew prefix.
                let mut cmd = Command::new("brew");
                cmd.arg("update");
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
            BrewOperation::Install { packages } => {
                info!("[brew] install: {}", packages.join(", "));
                // `HOMEBREW_NO_AUTO_UPDATE=1`: the Update operation is already wired
                // as a causality prerequisite of every Install, so auto-update on
                // install would duplicate that work (and it's slow over a cold
                // network).
                let mut cmd = Command::new("brew");
                cmd.env("HOMEBREW_NO_AUTO_UPDATE", "1")
                    .arg("install")
                    .arg("--formula")
                    .arg("--")
                    .args(packages);
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
    }
}
