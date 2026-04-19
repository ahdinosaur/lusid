use std::fmt::{self, Display};

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_ctx::Context;
use lusid_fs::{self as fs, FsError};
use lusid_operation::{
    Operation,
    operations::{
        directory::DirectoryOperation,
        file::{FileGroup, FileMode, FilePath, FileUser},
    },
};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use serde::Deserialize;
use thiserror::Error;

use crate::ResourceType;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DirectoryParams {
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

impl Display for DirectoryParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectoryParams::Present { path, .. } => write!(f, "Directory::Present(path = {path})"),
            DirectoryParams::Absent { path } => write!(f, "Directory::Absent(path = {path})"),
        }
    }
}

impl_display_render!(DirectoryParams);

#[derive(Debug, Clone)]
pub enum DirectoryResource {
    Present { path: FilePath },
    Absent { path: FilePath },
    Mode { path: FilePath, mode: FileMode },
    User { path: FilePath, user: FileUser },
    Group { path: FilePath, group: FileGroup },
}

impl Display for DirectoryResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectoryResource::Present { path } => write!(f, "DirectoryPresent({path})"),
            DirectoryResource::Absent { path } => write!(f, "DirectoryAbsent({path})"),
            DirectoryResource::Mode { path, mode } => {
                write!(f, "DirectoryMode({path}, mode = {mode})")
            }
            DirectoryResource::User { path, user } => {
                write!(f, "DirectoryUser({path}, user = {user})")
            }
            DirectoryResource::Group { path, group } => {
                write!(f, "DirectoryGroup({path}, group = {group})")
            }
        }
    }
}

impl_display_render!(DirectoryResource);

#[derive(Debug, Clone)]
pub enum DirectoryState {
    Present,
    Absent,
    ModeCorrect,
    ModeIncorrect,
    UserCorrect,
    UserIncorrect,
    GroupCorrect,
    GroupIncorrect,
}

impl Display for DirectoryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use DirectoryState::*;
        let text = match self {
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

impl_display_render!(DirectoryState);

#[derive(Error, Debug)]
pub enum DirectoryStateError {
    #[error(transparent)]
    Fs(#[from] FsError),
}

#[derive(Debug, Clone)]
pub enum DirectoryChange {
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

impl Display for DirectoryChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectoryChange::Create { path } => {
                write!(f, "Directory::Create(path = {path})")
            }
            DirectoryChange::Remove { path } => {
                write!(f, "Directory::Remove(path = {path})")
            }
            DirectoryChange::ChangeMode { path, mode } => {
                write!(f, "Directory::ChangeMode(path = {path}, mode = {mode})")
            }
            DirectoryChange::ChangeOwner { path, user, group } => {
                write!(
                    f,
                    "Directory::ChangeOwner(path = {path}, user = {user:?}, group = {group:?})"
                )
            }
        }
    }
}

impl_display_render!(DirectoryChange);

#[derive(Debug, Clone)]
pub struct Directory;

#[async_trait]
impl ResourceType for Directory {
    const ID: &'static str = "directory";

    fn param_types() -> Option<Spanned<ParamTypes>> {
        let span = Span::new(SourceId::empty(), 0, 0);
        let field = |ty, required: bool| {
            let mut param = ParamField::new(ty);
            if !required {
                param = param.with_optional();
            }
            Spanned::new(param, span.clone())
        };

        Some(Spanned::new(
            ParamTypes::Union(vec![
                indexmap! {
                  "state".to_string() => field(ParamType::Literal("present".into()), true),
                  "path".to_string() => field(ParamType::TargetPath, true),
                  "mode".to_string() => field(ParamType::Number, false),
                  "user".to_string() => field(ParamType::String, false),
                  "group".to_string() => field(ParamType::String, false),
                },
                indexmap! {
                  "state".to_string() => field(ParamType::Literal("absent".into()), true),
                  "path".to_string() => field(ParamType::TargetPath, true),
                },
            ]),
            span,
        ))
    }

    type Params = DirectoryParams;
    type Resource = DirectoryResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        match params {
            DirectoryParams::Present {
                path,
                mode,
                user,
                group,
            } => {
                let mut nodes = vec![CausalityTree::leaf(
                    CausalityMeta::id("directory".into()),
                    DirectoryResource::Present { path: path.clone() },
                )];

                if let Some(mode) = mode {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["directory".into()]),
                        DirectoryResource::Mode {
                            path: path.clone(),
                            mode,
                        },
                    ));
                }

                if let Some(user) = user {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["directory".into()]),
                        DirectoryResource::User {
                            path: path.clone(),
                            user,
                        },
                    ));
                }

                if let Some(group) = group {
                    nodes.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["directory".into()]),
                        DirectoryResource::Group { path, group },
                    ));
                }

                nodes
            }

            DirectoryParams::Absent { path } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                DirectoryResource::Absent { path },
            )],
        }
    }

    type State = DirectoryState;
    type StateError = DirectoryStateError;

    async fn state(
        _ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        let state = match resource {
            DirectoryResource::Present { path } | DirectoryResource::Absent { path } => {
                if fs::path_exists(path.as_path()).await? {
                    DirectoryState::Present
                } else {
                    DirectoryState::Absent
                }
            }

            DirectoryResource::Mode { path, mode } => {
                if !fs::path_exists(path.as_path()).await? {
                    DirectoryState::ModeIncorrect
                } else {
                    let actual_mode = fs::get_mode(path.as_path()).await?;
                    let actual_mode = actual_mode & 0o7777;
                    if actual_mode == mode.as_u32() {
                        DirectoryState::ModeCorrect
                    } else {
                        DirectoryState::ModeIncorrect
                    }
                }
            }

            DirectoryResource::User { path, user } => {
                if !fs::path_exists(path.as_path()).await? {
                    DirectoryState::UserIncorrect
                } else {
                    let actual_user = fs::get_owner_user(path.as_path()).await?;
                    let actual_user = actual_user.map(|u| u.name.to_string());
                    if actual_user.as_deref() == Some(user.as_str()) {
                        DirectoryState::UserCorrect
                    } else {
                        DirectoryState::UserIncorrect
                    }
                }
            }

            DirectoryResource::Group { path, group } => {
                if !fs::path_exists(path.as_path()).await? {
                    DirectoryState::GroupIncorrect
                } else {
                    let actual_group = fs::get_owner_group(path.as_path()).await?;
                    let actual_group = actual_group.map(|g| g.name.to_string());
                    if actual_group.as_deref() == Some(group.as_str()) {
                        DirectoryState::GroupCorrect
                    } else {
                        DirectoryState::GroupIncorrect
                    }
                }
            }
        };

        Ok(state)
    }

    type Change = DirectoryChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match (resource, state) {
            (DirectoryResource::Present { path }, DirectoryState::Absent) => {
                Some(DirectoryChange::Create { path: path.clone() })
            }

            (DirectoryResource::Present { .. }, DirectoryState::Present) => None,

            (DirectoryResource::Absent { path }, DirectoryState::Present) => {
                Some(DirectoryChange::Remove { path: path.clone() })
            }

            (DirectoryResource::Absent { .. }, DirectoryState::Absent) => None,

            (DirectoryResource::Mode { path, mode }, DirectoryState::ModeIncorrect) => {
                Some(DirectoryChange::ChangeMode {
                    path: path.clone(),
                    mode: *mode,
                })
            }

            (DirectoryResource::Mode { .. }, DirectoryState::ModeCorrect) => None,

            (DirectoryResource::User { path, user }, DirectoryState::UserIncorrect) => {
                Some(DirectoryChange::ChangeOwner {
                    path: path.clone(),
                    user: Some(user.clone()),
                    group: None,
                })
            }

            (DirectoryResource::User { .. }, DirectoryState::UserCorrect) => None,

            (DirectoryResource::Group { path, group }, DirectoryState::GroupIncorrect) => {
                Some(DirectoryChange::ChangeOwner {
                    path: path.clone(),
                    user: None,
                    group: Some(group.clone()),
                })
            }

            (DirectoryResource::Group { .. }, DirectoryState::GroupCorrect) => None,

            _ => {
                // TODO (mw): Return an error. Which means changing the trait's change method.
                // Or, alternatively, we have separate resources for each case, so there's no
                // possible mismatch.
                panic!("Unexpected case in change method for Directory resource.")
            }
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        let op = match change {
            DirectoryChange::Create { path } => {
                Operation::Directory(DirectoryOperation::Create { path })
            }
            DirectoryChange::Remove { path } => {
                Operation::Directory(DirectoryOperation::Remove { path })
            }
            DirectoryChange::ChangeMode { path, mode } => {
                Operation::Directory(DirectoryOperation::ChangeMode { path, mode })
            }
            DirectoryChange::ChangeOwner { path, user, group } => {
                Operation::Directory(DirectoryOperation::ChangeOwner { path, user, group })
            }
        };

        vec![CausalityTree::leaf(CausalityMeta::default(), op)]
    }
}
