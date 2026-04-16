//! Concrete mutations executed against a target machine.
//!
//! Operations are what actually *runs* — `apt install`, `write file`, `git clone`,
//! etc. They are produced by [`lusid_resource::ResourceChange::operations`] and are
//! the leaves of the causality tree handed to `lusid-apply` for per-epoch execution.
//!
//! Each operation family (apt, pacman, file, command, git) implements the
//! [`OperationType`] trait, which defines:
//!
//! - **`merge`** — coalesce same-type operations in one epoch (e.g. combine
//!   multiple `apt install` calls into one).
//! - **`apply`** — run the operation against the machine and return a future plus
//!   streaming stdout/stderr that the TUI can tail.
//!
//! The crate-level [`Operation`] / [`OperationApplyError`] / [`OperationApplyOutput`]
//! / [`OperationApplyStdout`] / [`OperationApplyStderr`] enums are thin dispatchers.
//! The three `ApplyXxx` enums use `pin_project` so they can forward `Future` /
//! `AsyncRead` polls to the per-type impls without boxing each call.

use async_trait::async_trait;
use core::task;
use lusid_ctx::Context;
use lusid_view::Render;
use pin_project::pin_project;
use std::{
    fmt::{Debug, Display},
    future::Future,
    pin::{Pin, pin},
    task::Poll,
};
use thiserror::Error;
use tokio::io::AsyncRead;

pub mod operations;

use crate::operations::{
    apt::{Apt, AptOperation},
    command::{Command, CommandOperation},
    file::{File, FileOperation},
    git::{Git, GitOperation},
    pacman::{Pacman, PacmanOperation},
};

/// One family of operations (apt, pacman, file, …). Implementors are zero-sized
/// markers; the real data lives in `Operation`.
#[async_trait]
pub trait OperationType {
    /// The concrete operation value (e.g. `AptOperation::Install { packages }`).
    type Operation: Render;

    /// Coalesce a batch of same-type operations scheduled in one epoch.
    ///
    /// For package managers this unions install sets. For side-effecting operations
    /// (file, command, git) the order matters, so `merge` is a no-op.
    fn merge(operations: Vec<Self::Operation>) -> Vec<Self::Operation>;

    /// Failure returned when `apply`'s future resolves.
    type ApplyError;

    /// Stdout stream of the running operation — polled by the TUI.
    type ApplyStdout: AsyncRead;

    /// Stderr stream of the running operation — polled by the TUI.
    type ApplyStderr: AsyncRead;

    /// Future that resolves when the operation finishes.
    type ApplyOutput: Future<Output = Result<(), Self::ApplyError>>;

    /// Kick off the operation and return its completion future plus live
    /// stdout/stderr streams. The caller drives all three concurrently so output
    /// is streamed in real time.
    async fn apply(
        ctx: &mut Context,
        operation: &Self::Operation,
    ) -> Result<(Self::ApplyOutput, Self::ApplyStdout, Self::ApplyStderr), Self::ApplyError>;
}

/// Dispatcher over every operation family. Every leaf of the per-epoch causality
/// tree is an `Operation`.
#[derive(Debug, Clone)]
pub enum Operation {
    Apt(AptOperation),
    Pacman(PacmanOperation),
    File(FileOperation),
    Command(CommandOperation),
    Git(GitOperation),
}

impl Operation {
    /// Partition `operations` by family, merge each family via its [`OperationType::merge`]
    /// impl, and re-wrap in family order (apt, pacman, file, command, git).
    ///
    /// Called once per epoch before `apply` — the whole point is to collapse e.g. 20
    /// separate `apt install` operations into one multi-package install.
    pub fn merge(operations: impl IntoIterator<Item = Operation>) -> Vec<Operation> {
        let OperationsByType {
            apt,
            pacman,
            file,
            command,
            git,
        } = partition_by_type(operations);

        std::iter::empty()
            .chain(Apt::merge(apt).into_iter().map(Operation::Apt))
            .chain(Pacman::merge(pacman).into_iter().map(Operation::Pacman))
            .chain(File::merge(file).into_iter().map(Operation::File))
            .chain(Command::merge(command).into_iter().map(Operation::Command))
            .chain(Git::merge(git).into_iter().map(Operation::Git))
            .collect()
    }
}

/// Dispatcher over any per-family `ApplyError`.
#[derive(Error, Debug)]
pub enum OperationApplyError {
    #[error("apt operation failed: {0:?}")]
    Apt(<Apt as OperationType>::ApplyError),

    #[error("pacman operation failed: {0:?}")]
    Pacman(<Pacman as OperationType>::ApplyError),

    #[error("file operation failed: {0:?}")]
    File(<File as OperationType>::ApplyError),

    #[error("command operation failed: {0:?}")]
    Command(<Command as OperationType>::ApplyError),

    #[error("git operation failed: {0:?}")]
    Git(<Git as OperationType>::ApplyError),
}

/// Unified completion future for any operation. `Future::poll` forwards to the active
/// variant via `pin_project`, avoiding a per-operation boxing allocation.
#[pin_project(project = OperationApplyOutputProject)]
pub enum OperationApplyOutput {
    Apt(#[pin] <Apt as OperationType>::ApplyOutput),
    Pacman(#[pin] <Pacman as OperationType>::ApplyOutput),
    File(#[pin] <File as OperationType>::ApplyOutput),
    Command(#[pin] <Command as OperationType>::ApplyOutput),
    Git(#[pin] <Git as OperationType>::ApplyOutput),
}

impl Future for OperationApplyOutput {
    type Output = Result<(), OperationApplyError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        use OperationApplyOutputProject::*;
        match self.project() {
            Apt(fut) => fut.poll(cx).map_err(OperationApplyError::Apt),
            Pacman(fut) => fut.poll(cx).map_err(OperationApplyError::Pacman),
            File(fut) => fut.poll(cx).map_err(OperationApplyError::File),
            Command(fut) => fut.poll(cx).map_err(OperationApplyError::Command),
            Git(fut) => fut.poll(cx).map_err(OperationApplyError::Git),
        }
    }
}

/// Unified stdout stream for any running operation. Implements [`AsyncRead`] by
/// forwarding to the active variant.
#[pin_project(project = OperationApplyStdoutProject)]
pub enum OperationApplyStdout {
    Apt(#[pin] <Apt as OperationType>::ApplyStdout),
    Pacman(#[pin] <Pacman as OperationType>::ApplyStdout),
    File(#[pin] <File as OperationType>::ApplyStdout),
    Command(#[pin] <Command as OperationType>::ApplyStdout),
    Git(#[pin] <Git as OperationType>::ApplyStdout),
}

impl AsyncRead for OperationApplyStdout {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        use OperationApplyStdoutProject::*;
        match self.project() {
            Apt(stream) => stream.poll_read(cx, buf),
            Pacman(stream) => stream.poll_read(cx, buf),
            File(stream) => stream.poll_read(cx, buf),
            Command(stream) => stream.poll_read(cx, buf),
            Git(stream) => stream.poll_read(cx, buf),
        }
    }
}

/// Unified stderr stream for any running operation. Implements [`AsyncRead`] by
/// forwarding to the active variant.
#[pin_project(project = OperationApplyStderrProject)]
pub enum OperationApplyStderr {
    Apt(#[pin] <Apt as OperationType>::ApplyStderr),
    Pacman(#[pin] <Pacman as OperationType>::ApplyStderr),
    File(#[pin] <File as OperationType>::ApplyStderr),
    Command(#[pin] <Command as OperationType>::ApplyStderr),
    Git(#[pin] <Git as OperationType>::ApplyStderr),
}

impl AsyncRead for OperationApplyStderr {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        use OperationApplyStderrProject::*;
        match self.project() {
            Apt(stream) => stream.poll_read(cx, buf),
            Pacman(stream) => stream.poll_read(cx, buf),
            File(stream) => stream.poll_read(cx, buf),
            Command(stream) => stream.poll_read(cx, buf),
            Git(stream) => stream.poll_read(cx, buf),
        }
    }
}

impl Operation {
    /// Start the operation on the target machine. Returns a completion future plus
    /// streaming stdout/stderr. The caller (typically `lusid-apply`) should drive the
    /// future and both streams concurrently so output is surfaced in real time.
    pub async fn apply(
        &self,
        ctx: &mut Context,
    ) -> Result<
        (
            OperationApplyOutput,
            OperationApplyStdout,
            OperationApplyStderr,
        ),
        OperationApplyError,
    > {
        match self {
            Operation::Apt(op) => {
                let (output, stdout, stderr) = Apt::apply(ctx, op)
                    .await
                    .map_err(OperationApplyError::Apt)?;
                Ok((
                    OperationApplyOutput::Apt(output),
                    OperationApplyStdout::Apt(stdout),
                    OperationApplyStderr::Apt(stderr),
                ))
            }
            Operation::Pacman(op) => {
                let (output, stdout, stderr) = Pacman::apply(ctx, op)
                    .await
                    .map_err(OperationApplyError::Pacman)?;
                Ok((
                    OperationApplyOutput::Pacman(output),
                    OperationApplyStdout::Pacman(stdout),
                    OperationApplyStderr::Pacman(stderr),
                ))
            }
            Operation::File(op) => {
                let (output, stdout, stderr) = File::apply(ctx, op)
                    .await
                    .map_err(OperationApplyError::File)?;
                Ok((
                    OperationApplyOutput::File(output),
                    OperationApplyStdout::File(stdout),
                    OperationApplyStderr::File(stderr),
                ))
            }
            Operation::Command(op) => {
                let (output, stdout, stderr) = Command::apply(ctx, op)
                    .await
                    .map_err(OperationApplyError::Command)?;
                Ok((
                    OperationApplyOutput::Command(output),
                    OperationApplyStdout::Command(stdout),
                    OperationApplyStderr::Command(stderr),
                ))
            }
            Operation::Git(op) => {
                let (output, stdout, stderr) = Git::apply(ctx, op)
                    .await
                    .map_err(OperationApplyError::Git)?;
                Ok((
                    OperationApplyOutput::Git(output),
                    OperationApplyStdout::Git(stdout),
                    OperationApplyStderr::Git(stderr),
                ))
            }
        }
    }
}

impl Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Operation::*;
        match self {
            Apt(op) => Display::fmt(op, f),
            Pacman(op) => Display::fmt(op, f),
            File(op) => Display::fmt(op, f),
            Command(op) => Display::fmt(op, f),
            Git(op) => Display::fmt(op, f),
        }
    }
}

impl Render for Operation {
    fn render(&self) -> lusid_view::View {
        use Operation::*;
        match self {
            Apt(params) => params.render(),
            File(params) => params.render(),
            Pacman(params) => params.render(),
            Command(params) => params.render(),
            Git(params) => params.render(),
        }
    }
}

/// Operations grouped by family, ready to be fed to each family's `merge`.
#[derive(Debug, Clone)]
pub struct OperationsByType {
    apt: Vec<AptOperation>,
    pacman: Vec<PacmanOperation>,
    file: Vec<FileOperation>,
    command: Vec<CommandOperation>,
    git: Vec<GitOperation>,
}

/// Bucket a mixed iterator of operations into per-family vectors.
fn partition_by_type(operations: impl IntoIterator<Item = Operation>) -> OperationsByType {
    let mut apt: Vec<AptOperation> = Vec::new();
    let mut pacman: Vec<PacmanOperation> = Vec::new();
    let mut file: Vec<FileOperation> = Vec::new();
    let mut command: Vec<CommandOperation> = Vec::new();
    let mut git: Vec<GitOperation> = Vec::new();
    for operation in operations.into_iter() {
        match operation {
            Operation::Apt(op) => apt.push(op),
            Operation::Pacman(op) => pacman.push(op),
            Operation::File(op) => file.push(op),
            Operation::Command(op) => command.push(op),
            Operation::Git(op) => git.push(op),
        }
    }
    OperationsByType {
        apt,
        pacman,
        file,
        command,
        git,
    }
}
