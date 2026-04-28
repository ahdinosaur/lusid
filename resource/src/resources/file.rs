use std::fmt::{self, Display};

use async_trait::async_trait;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_ctx::Context;
use lusid_fs::{self as fs, FsError};
use lusid_operation::{
    Operation,
    operations::file::{FileGroup, FileMode, FileOperation, FilePath, FileSource, FileUser},
};
use lusid_params::{FromRimu, ParseError, StructFields};
use lusid_view::impl_display_render;
use rimu::{Spanned, Value};
use secrecy::ExposeSecret;
use thiserror::Error;

use crate::ResourceType;

#[derive(Debug, Clone)]
pub enum FileParams {
    Sourced {
        source: FilePath,
        path: FilePath,
        mode: Option<FileMode>,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
    Present {
        path: FilePath,
        mode: Option<FileMode>,
        user: Option<FileUser>,
        group: Option<FileGroup>,
    },
    Absent {
        path: FilePath,
    },
}

impl FromRimu for FileParams {
    fn from_rimu(value: Spanned<Value>) -> Result<Self, Spanned<ParseError>> {
        let mut fields = StructFields::new(value)?;
        let state = fields.take_discriminator("state", &["sourced", "present", "absent"])?;
        let out = match state {
            "sourced" => FileParams::Sourced {
                // `source` is a `host-path`; the parser resolves a relative
                // string against the plan's source dir (or accepts a typed
                // `Value::HostPath` from a plan that uses `host_path("./...")`).
                // Either way we lower to a `FilePath` string for the operation
                // layer.
                source: FilePath::new(
                    fields
                        .required_host_path("source")?
                        .to_string_lossy()
                        .into_owned(),
                ),
                path: FilePath::new(fields.required_target_path("path")?),
                mode: fields.optional_u32("mode")?.map(FileMode::new),
                user: fields.optional_string("user")?.map(FileUser::new),
                group: fields.optional_string("group")?.map(FileGroup::new),
            },
            "present" => FileParams::Present {
                path: FilePath::new(fields.required_target_path("path")?),
                mode: fields.optional_u32("mode")?.map(FileMode::new),
                user: fields.optional_string("user")?.map(FileUser::new),
                group: fields.optional_string("group")?.map(FileGroup::new),
            },
            "absent" => FileParams::Absent {
                path: FilePath::new(fields.required_target_path("path")?),
            },
            _ => unreachable!(),
        };
        fields.finish()?;
        Ok(out)
    }
}

impl Display for FileParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileParams::Sourced { source, path, .. } => {
                write!(f, "File::Sourced(source = {source}, path = {path})")
            }
            FileParams::Present { path, .. } => write!(f, "File::Present(path = {path})"),
            FileParams::Absent { path } => write!(f, "File::Absent(path = {path})"),
        }
    }
}

impl_display_render!(FileParams);

#[derive(Debug, Clone)]
pub enum FileResource {
    Sourced {
        source: FilePath,
        path: FilePath,
    },
    /// Contents sourced from a decrypted secret by name; resolved against
    /// [`Context::secrets`] at state/apply time so plaintext never travels
    /// through the resource/change tree. See `@core/secret`.
    Secret {
        name: String,
        path: FilePath,
    },
    Present {
        path: FilePath,
    },
    Absent {
        path: FilePath,
    },
    Mode {
        path: FilePath,
        mode: FileMode,
    },
    User {
        path: FilePath,
        user: FileUser,
    },
    Group {
        path: FilePath,
        group: FileGroup,
    },
}

impl Display for FileResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileResource::Sourced { source, path } => {
                write!(f, "FileSourced({source} -> {path})")
            }
            FileResource::Secret { name, path } => {
                write!(f, "FileSecret(secret = {name} -> {path})")
            }
            FileResource::Present { path } => write!(f, "FilePresent({path})"),
            FileResource::Absent { path } => write!(f, "FileAbsent({path})"),
            FileResource::Mode { path, mode } => write!(f, "FileMode({path}, mode = {mode})"),
            FileResource::User { path, user } => write!(f, "FileUser({path}, user = {user})"),
            FileResource::Group { path, group } => write!(f, "FileGroup({path}, group = {group})"),
        }
    }
}

impl_display_render!(FileResource);

#[derive(Debug, Clone)]
pub enum FileState {
    Sourced,
    NotSourced,
    Present,
    Absent,
    ModeCorrect,
    ModeIncorrect,
    UserCorrect,
    UserIncorrect,
    GroupCorrect,
    GroupIncorrect,
}

impl Display for FileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use FileState::*;
        let text = match self {
            Sourced => "Sourced",
            NotSourced => "NotSourced",
            Present => "Present",
            Absent => "Absent",
            ModeCorrect => "ModeCorrect",
            ModeIncorrect => "ModeIncorrect",
            UserCorrect => "UserCorrect",
            UserIncorrect => "UserIncorrect",
            GroupCorrect => "GroupCorrect",
            GroupIncorrect => "GroupIncorrect",
        };
        write!(f, "{text}")
    }
}

impl_display_render!(FileState);

#[derive(Error, Debug)]
pub enum FileStateError {
    #[error(transparent)]
    Fs(#[from] FsError),

    /// Fires at state probe time when diffing on-disk contents against a
    /// declared secret. Apply-side twin:
    /// [`FileApplyError::MissingSecret`](lusid_operation::operations::file::FileApplyError::MissingSecret).
    #[error(
        "secret {name:?} referenced by file resource was not found in decrypted secrets bundle"
    )]
    MissingSecret { name: String },
}

#[derive(Debug, Clone)]
pub enum FileChange {
    Write {
        path: FilePath,
        source: FileSource,
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

impl Display for FileChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileChange::Write { path, source } => match source {
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
                FileSource::Secret(name) => {
                    write!(f, "File::Write(path = {}, source = Secret({}))", path, name)
                }
            },
            FileChange::Remove { path } => write!(f, "File::Remove(path = {path})"),
            FileChange::ChangeMode { path, mode } => {
                write!(f, "File::ChangeMode(path = {path}, mode = {mode})")
            }
            FileChange::ChangeOwner { path, user, group } => write!(
                f,
                "File::ChangeOwner(path = {path}, user = {user:?}, group = {group:?})"
            ),
        }
    }
}

impl_display_render!(FileChange);

#[derive(Debug, Clone)]
pub struct File;

#[async_trait]
impl ResourceType for File {
    const ID: &'static str = "file";

    type Params = FileParams;
    type Resource = FileResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        match params {
            FileParams::Sourced {
                source,
                path,
                mode,
                user,
                group,
            } => {
                let mut nodes = vec![CausalityTree::leaf(
                    CausalityMeta::id("file".into()),
                    FileResource::Sourced {
                        source,
                        path: path.clone(),
                    },
                )];

                if let Some(mode) = mode {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Mode {
                            path: path.clone(),
                            mode,
                        },
                    ));
                }

                if let Some(user) = user {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::User {
                            path: path.clone(),
                            user,
                        },
                    ))
                }

                if let Some(group) = group {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Group { path, group },
                    ));
                }

                nodes
            }

            FileParams::Present {
                path,
                mode,
                user,
                group,
            } => {
                let mut nodes = vec![CausalityTree::leaf(
                    CausalityMeta::id("file".into()),
                    FileResource::Present { path: path.clone() },
                )];

                if let Some(mode) = mode {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Mode {
                            path: path.clone(),
                            mode,
                        },
                    ));
                }

                if let Some(user) = user {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::User {
                            path: path.clone(),
                            user,
                        },
                    ));
                }

                if let Some(group) = group {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["file".into()]),
                        FileResource::Group { path, group },
                    ));
                }

                nodes
            }

            FileParams::Absent { path } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                FileResource::Absent { path },
            )],
        }
    }

    type State = FileState;
    type StateError = FileStateError;

    async fn state(
        ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        let state = match resource {
            FileResource::Sourced { source, path } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::NotSourced
                } else {
                    let source_contents = fs::read_file_to_bytes(source.as_path()).await?;
                    let path_contents = fs::read_file_to_bytes(path.as_path()).await?;
                    if source_contents == path_contents {
                        FileState::Sourced
                    } else {
                        FileState::NotSourced
                    }
                }
            }

            FileResource::Secret { name, path } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::NotSourced
                } else {
                    // Compare the file's current contents against the
                    // decrypted secret plaintext. A missing secret here
                    // (e.g. typo in the plan's `name` field) surfaces as
                    // `MissingSecret` rather than a silent NotSourced.
                    let secret = ctx
                        .secrets()
                        .get(name)
                        .ok_or_else(|| FileStateError::MissingSecret { name: name.clone() })?;
                    let path_contents = fs::read_file_to_bytes(path.as_path()).await?;
                    if path_contents.as_slice() == secret.expose_secret().as_bytes() {
                        FileState::Sourced
                    } else {
                        FileState::NotSourced
                    }
                }
            }

            FileResource::Present { path } | FileResource::Absent { path } => {
                if fs::path_exists(path.as_path()).await? {
                    FileState::Present
                } else {
                    FileState::Absent
                }
            }

            FileResource::Mode { path, mode } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::ModeIncorrect
                } else {
                    let actual_mode = fs::get_mode(path.as_path()).await?;
                    let actual_mode = actual_mode & 0o7777;
                    if actual_mode == mode.as_u32() {
                        FileState::ModeCorrect
                    } else {
                        FileState::ModeIncorrect
                    }
                }
            }

            FileResource::User { path, user } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::UserIncorrect
                } else {
                    let actual_user = fs::get_owner_user(path.as_path()).await?;
                    let actual_user = actual_user.map(|u| u.name.to_string());
                    if actual_user.as_deref() == Some(user.as_str()) {
                        FileState::UserCorrect
                    } else {
                        FileState::UserIncorrect
                    }
                }
            }

            FileResource::Group { path, group } => {
                if !fs::path_exists(path.as_path()).await? {
                    FileState::GroupIncorrect
                } else {
                    let actual_group = fs::get_owner_group(path.as_path()).await?;
                    let actual_group = actual_group.map(|g| g.name.to_string());
                    if actual_group.as_deref() == Some(group.as_str()) {
                        FileState::GroupCorrect
                    } else {
                        FileState::GroupIncorrect
                    }
                }
            }
        };

        Ok(state)
    }

    type Change = FileChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match (resource, state) {
            (FileResource::Sourced { source, path }, FileState::NotSourced) => {
                Some(FileChange::Write {
                    path: path.clone(),
                    source: FileSource::Path(source.clone()),
                })
            }

            (FileResource::Sourced { .. }, FileState::Sourced) => None,

            (FileResource::Secret { name, path }, FileState::NotSourced) => {
                Some(FileChange::Write {
                    path: path.clone(),
                    source: FileSource::Secret(name.clone()),
                })
            }

            (FileResource::Secret { .. }, FileState::Sourced) => None,

            (FileResource::Present { path }, FileState::Absent) => Some(FileChange::Write {
                path: path.clone(),
                source: FileSource::Contents(Vec::new()),
            }),

            (FileResource::Present { .. }, FileState::Present) => None,

            (FileResource::Absent { path }, FileState::Present) => {
                Some(FileChange::Remove { path: path.clone() })
            }

            (FileResource::Absent { .. }, FileState::Absent) => None,

            (FileResource::Mode { path, mode }, FileState::ModeIncorrect) => {
                Some(FileChange::ChangeMode {
                    path: path.clone(),
                    mode: *mode,
                })
            }

            (FileResource::Mode { .. }, FileState::ModeCorrect) => None,

            (FileResource::User { path, user }, FileState::UserIncorrect) => {
                Some(FileChange::ChangeOwner {
                    path: path.clone(),
                    user: Some(user.clone()),
                    group: None,
                })
            }

            (FileResource::User { .. }, FileState::UserCorrect) => None,

            (FileResource::Group { path, group }, FileState::GroupIncorrect) => {
                Some(FileChange::ChangeOwner {
                    path: path.clone(),
                    user: None,
                    group: Some(group.clone()),
                })
            }

            (FileResource::Group { .. }, FileState::GroupCorrect) => None,

            _ => {
                // TODO (mw): Return an error. Which means changing the trait's change method.
                // Or, alternatively, we have separate resources for each case, so there's no
                // possible mismatch.
                panic!("Unexpected case in change method for File resource.")
            }
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        let op = match change {
            FileChange::Write { path, source } => {
                Operation::File(FileOperation::Write { path, source })
            }
            FileChange::Remove { path } => Operation::File(FileOperation::Remove { path }),
            FileChange::ChangeMode { path, mode } => {
                Operation::File(FileOperation::ChangeMode { path, mode })
            }
            FileChange::ChangeOwner { path, user, group } => {
                Operation::File(FileOperation::ChangeOwner { path, user, group })
            }
        };

        vec![CausalityTree::leaf(CausalityMeta::default(), op)]
    }
}
