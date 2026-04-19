use async_trait::async_trait;
use lusid_ctx::Context;
use lusid_fs::{self as fs, FsError};
use lusid_view::impl_display_render;
use std::{fmt::Display, pin::Pin};
use tokio::io::AsyncRead;
use tracing::info;

use crate::OperationType;
use crate::operations::file::{FileGroup, FileMode, FilePath, FileUser};

#[derive(Debug, Clone)]
pub enum DirectoryOperation {
    Create {
        path: FilePath,
    },
    Remove {
        path: FilePath,
    },
    ChangeMode {
        path: FilePath,
        mode: FileMode,
    },
    ChangeOwner {
        path: FilePath,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
}

impl Display for DirectoryOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectoryOperation::Create { path } => {
                write!(f, "Directory::Create(path = {path})")
            }
            DirectoryOperation::Remove { path } => {
                write!(f, "Directory::Remove(path = {path})")
            }
            DirectoryOperation::ChangeMode { path, mode } => {
                write!(f, "Directory::ChangeMode(path = {path}, mode = {mode})")
            }
            DirectoryOperation::ChangeOwner { path, user, group } => {
                write!(
                    f,
                    "Directory::ChangeOwner(path = {path}, user = {user:?}, group = {group:?})"
                )
            }
        }
    }
}

impl_display_render!(DirectoryOperation);

#[derive(Debug, Clone)]
pub struct Directory;

#[async_trait]
impl OperationType for Directory {
    type Operation = DirectoryOperation;

    fn merge(operations: Vec<Self::Operation>) -> Vec<Self::Operation> {
        operations
    }

    type ApplyOutput = Pin<Box<dyn Future<Output = Result<(), Self::ApplyError>> + Send + 'static>>;
    type ApplyError = FsError;

    type ApplyStdout = Pin<Box<dyn AsyncRead + Send + 'static>>;
    type ApplyStderr = Pin<Box<dyn AsyncRead + Send + 'static>>;

    async fn apply(
        _ctx: &mut Context,
        operation: &Self::Operation,
    ) -> Result<(Self::ApplyOutput, Self::ApplyStdout, Self::ApplyStderr), Self::ApplyError> {
        let stdout = Box::pin(tokio::io::empty());
        let stderr = Box::pin(tokio::io::empty());

        match operation.clone() {
            DirectoryOperation::Create { path } => {
                info!("[directory] create: {}", path);
                Ok((
                    Box::pin(async move { fs::create_dir(path.as_path()).await }),
                    stdout,
                    stderr,
                ))
            }
            DirectoryOperation::Remove { path } => {
                info!("[directory] remove: {}", path);
                Ok((
                    Box::pin(async move { fs::remove_dir(path.as_path()).await }),
                    stdout,
                    stderr,
                ))
            }
            DirectoryOperation::ChangeMode { path, mode } => {
                info!("[directory] change mode: {} -> {}", path, mode);
                Ok((
                    Box::pin(async move { fs::change_mode(path.as_path(), mode.as_u32()).await }),
                    stdout,
                    stderr,
                ))
            }
            DirectoryOperation::ChangeOwner { path, user, group } => {
                info!(
                    "[directory] change owner: {} -> user {:?} + group {:?}",
                    path, user, group
                );
                Ok((
                    Box::pin(async move {
                        fs::change_owner(
                            path.as_path(),
                            user.as_ref().map(|u| u.as_str()),
                            group.as_ref().map(|g| g.as_str()),
                        )
                        .await
                    }),
                    stdout,
                    stderr,
                ))
            }
        }
    }
}
