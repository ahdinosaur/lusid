use std::{collections::BTreeSet, fmt::Display};

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_operation::{
    Operation,
    operations::{file::FilePath, user::UserOperation},
};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use serde::Deserialize;
use thiserror::Error;

use crate::ResourceType;

/// Plan-level parameters for the `@core/user` resource.
///
/// Tagged by `state: "present" | "absent"`. Mirrors the shape used by Salt (`user.present`)
/// and Ansible (`ansible.builtin.user`), adapted to lusid's typed params schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum UserParams {
    Present {
        name: String,
        uid: Option<u32>,
        group: Option<String>,
        groups: Option<Vec<String>>,
        append_groups: Option<bool>,
        comment: Option<String>,
        home: Option<FilePath>,
        shell: Option<String>,
        system: Option<bool>,
        create_home: Option<bool>,
    },
    Absent {
        name: String,
        remove_home: Option<bool>,
    },
}

impl Display for UserParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserParams::Present { name, .. } => write!(f, "User::Present(name = {name})"),
            UserParams::Absent { name, remove_home } => {
                write!(f, "User::Absent(name = {name}, remove_home = {remove_home:?})")
            }
        }
    }
}

impl_display_render!(UserParams);

#[derive(Debug, Clone)]
pub enum UserResource {
    Present {
        name: String,
        uid: Option<u32>,
        group: Option<String>,
        groups: Option<Vec<String>>,
        append_groups: bool,
        comment: Option<String>,
        home: Option<FilePath>,
        shell: Option<String>,
        system: bool,
        create_home: bool,
    },
    Absent {
        name: String,
        remove_home: bool,
    },
}

impl Display for UserResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserResource::Present { name, .. } => write!(f, "User::Present(name = {name})"),
            UserResource::Absent { name, remove_home } => {
                write!(f, "User::Absent(name = {name}, remove_home = {remove_home})")
            }
        }
    }
}

impl_display_render!(UserResource);

#[derive(Debug, Clone)]
pub enum UserState {
    Absent,
    Present {
        uid: u32,
        primary_group: String,
        groups: Vec<String>,
        comment: String,
        home: String,
        shell: String,
    },
}

impl Display for UserState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserState::Absent => write!(f, "User::Absent"),
            UserState::Present {
                uid,
                primary_group,
                home,
                shell,
                ..
            } => write!(
                f,
                "User::Present(uid = {uid}, group = {primary_group}, home = {home}, shell = {shell})"
            ),
        }
    }
}

impl_display_render!(UserState);

#[derive(Error, Debug)]
pub enum UserStateError {
    #[error(transparent)]
    Command(#[from] CommandError),

    #[error("failed to parse getent passwd output: {output}")]
    ParsePasswd { output: String },

    #[error("failed to parse getent group output: {output}")]
    ParseGroup { output: String },

    #[error("failed to parse uid `{value}`: {source}")]
    ParseUid {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },

    #[error("failed to parse gid `{value}`: {source}")]
    ParseGid {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
}

#[derive(Debug, Clone)]
pub enum UserChange {
    Create {
        name: String,
        uid: Option<u32>,
        primary_group: Option<String>,
        supplementary_groups: Vec<String>,
        comment: Option<String>,
        home: Option<FilePath>,
        shell: Option<String>,
        system: bool,
        create_home: bool,
    },
    Modify {
        name: String,
        uid: Option<u32>,
        primary_group: Option<String>,
        supplementary_groups: Option<Vec<String>>,
        comment: Option<String>,
        home: Option<FilePath>,
        shell: Option<String>,
    },
    Delete {
        name: String,
        remove_home: bool,
    },
}

impl Display for UserChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserChange::Create { name, .. } => write!(f, "User::Create(name = {name})"),
            UserChange::Modify { name, .. } => write!(f, "User::Modify(name = {name})"),
            UserChange::Delete { name, remove_home } => {
                write!(f, "User::Delete(name = {name}, remove_home = {remove_home})")
            }
        }
    }
}

impl_display_render!(UserChange);

#[derive(Debug, Clone)]
pub struct User;

#[async_trait]
impl ResourceType for User {
    const ID: &'static str = "user";

    fn param_types() -> Option<Spanned<ParamTypes>> {
        let span = Span::new(SourceId::empty(), 0, 0);
        let field = |typ, required: bool| {
            let mut param = ParamField::new(typ);
            if !required {
                param = param.with_optional();
            }
            Spanned::new(param, span.clone())
        };
        let string_list = || ParamType::List {
            item: Box::new(Spanned::new(ParamType::String, span.clone())),
        };

        Some(Spanned::new(
            ParamTypes::Union(vec![
                indexmap! {
                    "state".to_string() => field(ParamType::Literal("present".into()), true),
                    "name".to_string() => field(ParamType::String, true),
                    "uid".to_string() => field(ParamType::Number, false),
                    "group".to_string() => field(ParamType::String, false),
                    "groups".to_string() => field(string_list(), false),
                    "append_groups".to_string() => field(ParamType::Boolean, false),
                    "comment".to_string() => field(ParamType::String, false),
                    "home".to_string() => field(ParamType::TargetPath, false),
                    "shell".to_string() => field(ParamType::String, false),
                    "system".to_string() => field(ParamType::Boolean, false),
                    "create_home".to_string() => field(ParamType::Boolean, false),
                },
                indexmap! {
                    "state".to_string() => field(ParamType::Literal("absent".into()), true),
                    "name".to_string() => field(ParamType::String, true),
                    "remove_home".to_string() => field(ParamType::Boolean, false),
                },
            ]),
            span,
        ))
    }

    type Params = UserParams;
    type Resource = UserResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        let resource = match params {
            UserParams::Present {
                name,
                uid,
                group,
                groups,
                append_groups,
                comment,
                home,
                shell,
                system,
                create_home,
            } => UserResource::Present {
                name,
                uid,
                group,
                groups,
                append_groups: append_groups.unwrap_or(false),
                comment,
                home,
                shell,
                system: system.unwrap_or(false),
                create_home: create_home.unwrap_or(true),
            },
            UserParams::Absent { name, remove_home } => UserResource::Absent {
                name,
                remove_home: remove_home.unwrap_or(false),
            },
        };
        vec![CausalityTree::leaf(CausalityMeta::default(), resource)]
    }

    type State = UserState;
    type StateError = UserStateError;

    async fn state(
        _ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        let name = match resource {
            UserResource::Present { name, .. } | UserResource::Absent { name, .. } => name,
        };

        let Some(passwd_entry) = get_passwd_entry(name).await? else {
            return Ok(UserState::Absent);
        };

        // Note(cc): A user without a resolvable primary group shouldn't happen on a
        // well-formed system, but we don't want to block on it — return an empty group
        // name and let the change() logic treat it as a mismatch against whatever the
        // plan declares.
        let primary_group = get_group_name_for_gid(passwd_entry.gid)
            .await?
            .unwrap_or_default();

        let groups = get_supplementary_groups(name, &primary_group).await?;

        Ok(UserState::Present {
            uid: passwd_entry.uid,
            primary_group,
            groups,
            comment: passwd_entry.comment,
            home: passwd_entry.home,
            shell: passwd_entry.shell,
        })
    }

    type Change = UserChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match (resource, state) {
            (UserResource::Absent { .. }, UserState::Absent) => None,

            (
                UserResource::Absent { name, remove_home },
                UserState::Present { .. },
            ) => Some(UserChange::Delete {
                name: name.clone(),
                remove_home: *remove_home,
            }),

            (
                UserResource::Present {
                    name,
                    uid,
                    group,
                    groups,
                    // Note(cc): `append_groups` only matters when reconciling against an
                    // existing user — a fresh account gets exactly the declared groups.
                    append_groups: _,
                    comment,
                    home,
                    shell,
                    system,
                    create_home,
                },
                UserState::Absent,
            ) => Some(UserChange::Create {
                name: name.clone(),
                uid: *uid,
                primary_group: group.clone(),
                supplementary_groups: groups.clone().unwrap_or_default(),
                comment: comment.clone(),
                home: home.clone(),
                shell: shell.clone(),
                system: *system,
                create_home: *create_home,
            }),

            (
                UserResource::Present {
                    name,
                    uid,
                    group,
                    groups,
                    append_groups,
                    comment,
                    home,
                    shell,
                    system: _,
                    create_home: _,
                },
                UserState::Present {
                    uid: current_uid,
                    primary_group: current_primary_group,
                    groups: current_groups,
                    comment: current_comment,
                    home: current_home,
                    shell: current_shell,
                },
            ) => {
                let uid_change = uid.filter(|declared| *declared != *current_uid);

                let group_change = group
                    .as_ref()
                    .filter(|declared| declared.as_str() != current_primary_group.as_str())
                    .cloned();

                let groups_change = groups.as_ref().and_then(|declared| {
                    let current: BTreeSet<&str> =
                        current_groups.iter().map(String::as_str).collect();
                    let declared_set: BTreeSet<&str> =
                        declared.iter().map(String::as_str).collect();

                    if *append_groups {
                        if declared_set.is_subset(&current) {
                            None
                        } else {
                            let merged: BTreeSet<&str> =
                                current.union(&declared_set).copied().collect();
                            Some(merged.into_iter().map(str::to_string).collect())
                        }
                    } else if declared_set == current {
                        None
                    } else {
                        Some(declared.clone())
                    }
                });

                let comment_change = comment
                    .as_ref()
                    .filter(|declared| declared.as_str() != current_comment.as_str())
                    .cloned();

                let home_change = home
                    .as_ref()
                    .filter(|declared| declared.as_path() != std::path::Path::new(current_home))
                    .cloned();

                let shell_change = shell
                    .as_ref()
                    .filter(|declared| declared.as_str() != current_shell.as_str())
                    .cloned();

                if uid_change.is_none()
                    && group_change.is_none()
                    && groups_change.is_none()
                    && comment_change.is_none()
                    && home_change.is_none()
                    && shell_change.is_none()
                {
                    None
                } else {
                    Some(UserChange::Modify {
                        name: name.clone(),
                        uid: uid_change,
                        primary_group: group_change,
                        supplementary_groups: groups_change,
                        comment: comment_change,
                        home: home_change,
                        shell: shell_change,
                    })
                }
            }
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        let op = match change {
            UserChange::Create {
                name,
                uid,
                primary_group,
                supplementary_groups,
                comment,
                home,
                shell,
                system,
                create_home,
            } => Operation::User(UserOperation::Add {
                name,
                uid,
                primary_group,
                supplementary_groups,
                comment,
                home,
                shell,
                system,
                create_home,
            }),
            UserChange::Modify {
                name,
                uid,
                primary_group,
                supplementary_groups,
                comment,
                home,
                shell,
            } => Operation::User(UserOperation::Modify {
                name,
                uid,
                primary_group,
                supplementary_groups,
                comment,
                home,
                shell,
            }),
            UserChange::Delete { name, remove_home } => {
                Operation::User(UserOperation::Delete { name, remove_home })
            }
        };
        vec![CausalityTree::leaf(CausalityMeta::default(), op)]
    }
}

/// One entry from `/etc/passwd`, surfaced as typed fields.
struct PasswdEntry {
    uid: u32,
    gid: u32,
    comment: String,
    home: String,
    shell: String,
}

/// Read a single user from the NSS passwd database. `None` if the user doesn't exist
/// (which `getent` signals by exiting non-zero with empty stderr).
async fn get_passwd_entry(name: &str) -> Result<Option<PasswdEntry>, UserStateError> {
    let output = Command::new("getent")
        .args(["passwd", name])
        .handle(
            |stdout| -> Result<Option<String>, UserStateError> {
                Ok(Some(String::from_utf8_lossy(stdout).to_string()))
            },
            |_stderr| -> Result<Option<Option<String>>, UserStateError> {
                // getent exits non-zero with no output when the entry is missing.
                Ok(Some(None))
            },
        )
        .await??;

    let Some(stdout) = output else {
        return Ok(None);
    };

    // passwd format: name:password:uid:gid:gecos:home:shell
    // Note(cc): the GECOS field itself can contain commas (full name, office, phone, etc.)
    // but not colons, so a 7-field split is always safe.
    let line = stdout.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.splitn(7, ':').collect();
    if parts.len() < 7 {
        return Err(UserStateError::ParsePasswd {
            output: stdout,
        });
    }

    let uid = parts[2]
        .parse::<u32>()
        .map_err(|source| UserStateError::ParseUid {
            value: parts[2].to_string(),
            source,
        })?;
    let gid = parts[3]
        .parse::<u32>()
        .map_err(|source| UserStateError::ParseGid {
            value: parts[3].to_string(),
            source,
        })?;

    Ok(Some(PasswdEntry {
        uid,
        gid,
        comment: parts[4].to_string(),
        home: parts[5].to_string(),
        shell: parts[6].trim_end().to_string(),
    }))
}

/// Resolve a numeric gid to its group name via `getent group <gid>`.
async fn get_group_name_for_gid(gid: u32) -> Result<Option<String>, UserStateError> {
    let output = Command::new("getent")
        .args(["group", &gid.to_string()])
        .handle(
            |stdout| -> Result<Option<String>, UserStateError> {
                Ok(Some(String::from_utf8_lossy(stdout).to_string()))
            },
            |_stderr| -> Result<Option<Option<String>>, UserStateError> { Ok(Some(None)) },
        )
        .await??;

    let Some(stdout) = output else {
        return Ok(None);
    };

    // group format: name:password:gid:members
    let line = stdout.lines().next().unwrap_or("");
    let name = line.split(':').next().ok_or_else(|| UserStateError::ParseGroup {
        output: stdout.clone(),
    })?;
    if name.is_empty() {
        return Err(UserStateError::ParseGroup { output: stdout });
    }
    Ok(Some(name.to_string()))
}

/// All supplementary groups for `name`, i.e. every group the user is a member of
/// except the primary one. Uses `id -Gn` for the full list, then filters.
async fn get_supplementary_groups(
    name: &str,
    primary_group: &str,
) -> Result<Vec<String>, UserStateError> {
    let stdout = Command::new("id").args(["-Gn", name]).run().await?;
    let stdout = String::from_utf8_lossy(&stdout);
    let groups: Vec<String> = stdout
        .split_whitespace()
        .filter(|group| *group != primary_group)
        .map(str::to_string)
        .collect();
    Ok(groups)
}
