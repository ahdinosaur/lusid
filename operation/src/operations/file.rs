use async_trait::async_trait;
use lusid_ctx::Context;
use lusid_fs::{self as fs, FsError};
use lusid_view::impl_display_render;
use serde::{Deserialize, Serialize};
use std::{
    fmt::{Debug, Display},
    path::Path,
    pin::Pin,
};
use tokio::io::AsyncRead;
use tracing::info;

use crate::OperationType;

#[derive(Debug, Clone)]
pub enum FileSource {
    Contents(Vec<u8>),
    Path(FilePath),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FilePath(String);

impl FilePath {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl Display for FilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

impl Display for FileMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:o}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileUser(String);

impl FileUser {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for FileUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileGroup(String);

impl FileGroup {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for FileGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub enum FileOperation {
    Write {
        path: FilePath,
        source: FileSource,
    },
    Copy {
        source: FilePath,
        destination: FilePath,
    },
    Move {
        source: FilePath,
        destination: FilePath,
    },
    Remove {
        path: FilePath,
    },
    CreateSymlink {
        source: FilePath,
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

impl Display for FileOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileOperation::Write { path, source } => match source {
                FileSource::Contents(contents) => write!(
                    f,
                    "File::Write(path = {}, source = Contents({} bytes))",
                    path,
                    contents.len()
                ),
                FileSource::Path(source_path) => write!(
                    f,
                    "File::Write(path = {}, source = Path({}))",
                    path, source_path
                ),
            },
            FileOperation::Copy {
                source,
                destination,
            } => write!(
                f,
                "File::Copy(source = {}, destination = {})",
                source, destination
            ),
            FileOperation::Move {
                source,
                destination,
            } => write!(
                f,
                "File::Move(source = {}, destination = {})",
                source, destination
            ),
            FileOperation::Remove { path } => write!(f, "File::Remove(path = {})", path),
            FileOperation::CreateSymlink { source, path } => write!(
                f,
                "File::CreateSymlink(source = {}, path = {})",
                source, path
            ),
            FileOperation::ChangeMode { path, mode } => {
                write!(f, "File::ChangeMode(path = {}, mode = {})", path, mode)
            }
            FileOperation::ChangeOwner { path, user, group } => {
                write!(
                    f,
                    "File::ChangeOwner(path = {}, user = {:?}, group = {:?})",
                    path, user, group
                )
            }
        }
    }
}

impl_display_render!(FileOperation);

#[derive(Debug, Clone)]
pub struct File;

#[async_trait]
impl OperationType for File {
    type Operation = FileOperation;

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
            FileOperation::Write { path, source } => {
                info!("[file] write file: {}", path);
                Ok((
                    Box::pin(async move {
                        match source {
                            FileSource::Contents(contents) => {
                                fs::write_file_atomic(path.as_path(), &contents).await
                            }
                            FileSource::Path(source) => {
                                fs::copy_file_atomic(source.as_path(), path.as_path()).await
                            }
                        }
                    }),
                    stdout,
                    stderr,
                ))
            }
            FileOperation::Copy {
                source,
                destination,
            } => {
                info!("[file] copy file: {} -> {}", source, destination);
                Ok((
                    Box::pin(async move {
                        fs::copy_file_atomic(source.as_path(), destination.as_path()).await
                    }),
                    stdout,
                    stderr,
                ))
            }
            FileOperation::Move {
                source,
                destination,
            } => {
                info!("[file] move file: {} -> {}", source, destination);
                Ok((
                    Box::pin(async move {
                        fs::rename_file(source.as_path(), destination.as_path()).await
                    }),
                    stdout,
                    stderr,
                ))
            }
            FileOperation::Remove { path } => {
                info!("[file] remove file: {}", path);
                Ok((
                    Box::pin(async move { fs::remove_file(path.as_path()).await }),
                    stdout,
                    stderr,
                ))
            }
            FileOperation::CreateSymlink { source, path } => {
                info!("[file] create symlink: {} -> {}", path, source);
                Ok((
                    Box::pin(
                        async move { fs::create_symlink(source.as_path(), path.as_path()).await },
                    ),
                    stdout,
                    stderr,
                ))
            }
            FileOperation::ChangeMode { path, mode } => {
                info!("[file] change mode: {} -> {}", path, mode);
                Ok((
                    Box::pin(async move { fs::change_mode(path.as_path(), mode.as_u32()).await }),
                    stdout,
                    stderr,
                ))
            }
            FileOperation::ChangeOwner { path, user, group } => {
                info!(
                    "[file] change user: {} -> user {:?} + group {:?}",
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
