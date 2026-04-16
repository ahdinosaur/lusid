use std::{collections::BTreeSet, fmt::Display};

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_operation::{Operation, operations::group::GroupOperation};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use serde::Deserialize;
use thiserror::Error;

use crate::ResourceType;

/// Plan-level parameters for the `@core/group` resource.
///
/// Tagged by `state: "present" | "absent"`. Mirrors the shape used by Salt
/// (`group.present`) and Ansible (`ansible.builtin.group`), with an additional
/// `members` field to declare supplementary group membership declaratively.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum GroupParams {
    Present {
        name: String,
        gid: Option<u32>,
        system: Option<bool>,
        /// Exact list of supplementary members. `None` means "leave membership
        /// unmanaged"; `Some([])` means "ensure no supplementary members".
        /// Users whose *primary* group is this one are unaffected — primary
        /// membership is set on the `user` resource, not here.
        members: Option<Vec<String>>,
    },
    Absent {
        name: String,
    },
}

impl Display for GroupParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupParams::Present { name, .. } => write!(f, "Group::Present(name = {name})"),
            GroupParams::Absent { name } => write!(f, "Group::Absent(name = {name})"),
        }
    }
}

impl_display_render!(GroupParams);

#[derive(Debug, Clone)]
pub enum GroupResource {
    Present {
        name: String,
        gid: Option<u32>,
        system: bool,
        members: Option<Vec<String>>,
    },
    Absent {
        name: String,
    },
}

impl Display for GroupResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupResource::Present { name, .. } => write!(f, "Group::Present(name = {name})"),
            GroupResource::Absent { name } => write!(f, "Group::Absent(name = {name})"),
        }
    }
}

impl_display_render!(GroupResource);

#[derive(Debug, Clone)]
pub enum GroupState {
    Absent,
    Present { gid: u32, members: Vec<String> },
}

impl Display for GroupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupState::Absent => write!(f, "Group::Absent"),
            GroupState::Present { gid, members } => write!(
                f,
                "Group::Present(gid = {gid}, members = [{}])",
                members.join(", ")
            ),
        }
    }
}

impl_display_render!(GroupState);

#[derive(Error, Debug)]
pub enum GroupStateError {
    #[error(transparent)]
    Command(#[from] CommandError),

    #[error("failed to parse getent group output: {output}")]
    ParseGroup { output: String },

    #[error("failed to parse gid `{value}`: {source}")]
    ParseGid {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
}

#[derive(Debug, Clone)]
pub enum GroupChange {
    Create {
        name: String,
        gid: Option<u32>,
        system: bool,
        /// Supplementary members to install after the group is created. Empty
        /// means "no `gpasswd` step".
        members: Vec<String>,
    },
    Modify {
        name: String,
        gid: Option<u32>,
        /// `None` means membership is unmanaged or already matches; `Some`
        /// means "replace the current supplementary members with exactly this list".
        members: Option<Vec<String>>,
    },
    Delete {
        name: String,
    },
}

impl Display for GroupChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupChange::Create { name, .. } => write!(f, "Group::Create(name = {name})"),
            GroupChange::Modify { name, .. } => write!(f, "Group::Modify(name = {name})"),
            GroupChange::Delete { name } => write!(f, "Group::Delete(name = {name})"),
        }
    }
}

impl_display_render!(GroupChange);

#[derive(Debug, Clone)]
pub struct Group;

#[async_trait]
impl ResourceType for Group {
    const ID: &'static str = "group";

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
                    "gid".to_string() => field(ParamType::Number, false),
                    "system".to_string() => field(ParamType::Boolean, false),
                    "members".to_string() => field(string_list(), false),
                },
                indexmap! {
                    "state".to_string() => field(ParamType::Literal("absent".into()), true),
                    "name".to_string() => field(ParamType::String, true),
                },
            ]),
            span,
        ))
    }

    type Params = GroupParams;
    type Resource = GroupResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        let resource = match params {
            GroupParams::Present {
                name,
                gid,
                system,
                members,
            } => GroupResource::Present {
                name,
                gid,
                system: system.unwrap_or(false),
                members,
            },
            GroupParams::Absent { name } => GroupResource::Absent { name },
        };
        vec![CausalityTree::leaf(CausalityMeta::default(), resource)]
    }

    type State = GroupState;
    type StateError = GroupStateError;

    async fn state(
        _ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        let name = match resource {
            GroupResource::Present { name, .. } | GroupResource::Absent { name } => name,
        };

        let Some(entry) = get_group_entry(name).await? else {
            return Ok(GroupState::Absent);
        };

        Ok(GroupState::Present {
            gid: entry.gid,
            members: entry.members,
        })
    }

    type Change = GroupChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match (resource, state) {
            (GroupResource::Absent { .. }, GroupState::Absent) => None,

            (GroupResource::Absent { name }, GroupState::Present { .. }) => {
                Some(GroupChange::Delete { name: name.clone() })
            }

            (
                GroupResource::Present {
                    name,
                    gid,
                    system,
                    members,
                },
                GroupState::Absent,
            ) => Some(GroupChange::Create {
                name: name.clone(),
                gid: *gid,
                system: *system,
                members: members.clone().unwrap_or_default(),
            }),

            (
                GroupResource::Present {
                    name,
                    gid,
                    system: _,
                    members,
                },
                GroupState::Present {
                    gid: current_gid,
                    members: current_members,
                },
            ) => {
                let gid_change = gid.filter(|declared| *declared != *current_gid);

                let members_change = members.as_ref().and_then(|declared| {
                    let current: BTreeSet<&str> =
                        current_members.iter().map(String::as_str).collect();
                    let declared_set: BTreeSet<&str> =
                        declared.iter().map(String::as_str).collect();
                    if declared_set == current {
                        None
                    } else {
                        Some(declared.clone())
                    }
                });

                if gid_change.is_none() && members_change.is_none() {
                    None
                } else {
                    Some(GroupChange::Modify {
                        name: name.clone(),
                        gid: gid_change,
                        members: members_change,
                    })
                }
            }
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        match change {
            GroupChange::Create {
                name,
                gid,
                system,
                members,
            } => {
                let mut ops: Vec<CausalityTree<Operation>> = Vec::new();
                let needs_set_members = !members.is_empty();

                let add_meta = if needs_set_members {
                    CausalityMeta::id("add".into())
                } else {
                    CausalityMeta::default()
                };
                ops.push(CausalityTree::leaf(
                    add_meta,
                    Operation::Group(GroupOperation::Add {
                        name: name.clone(),
                        gid,
                        system,
                    }),
                ));

                if needs_set_members {
                    ops.push(CausalityTree::leaf(
                        CausalityMeta::requires(vec!["add".into()]),
                        Operation::Group(GroupOperation::SetMembers { name, members }),
                    ));
                }

                ops
            }
            GroupChange::Modify { name, gid, members } => {
                let mut ops: Vec<CausalityTree<Operation>> = Vec::new();
                if gid.is_some() {
                    ops.push(CausalityTree::leaf(
                        CausalityMeta::default(),
                        Operation::Group(GroupOperation::Modify {
                            name: name.clone(),
                            gid,
                        }),
                    ));
                }
                if let Some(members) = members {
                    ops.push(CausalityTree::leaf(
                        CausalityMeta::default(),
                        Operation::Group(GroupOperation::SetMembers { name, members }),
                    ));
                }
                ops
            }
            GroupChange::Delete { name } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                Operation::Group(GroupOperation::Delete { name }),
            )],
        }
    }
}

/// One entry from `/etc/group`, surfaced as typed fields.
struct GroupEntry {
    gid: u32,
    members: Vec<String>,
}

/// Read a single group from the NSS group database. `None` if the group doesn't
/// exist (which `getent` signals by exiting non-zero).
async fn get_group_entry(name: &str) -> Result<Option<GroupEntry>, GroupStateError> {
    let outcome = Command::new("getent")
        .args(["group", name])
        .outcome()
        .await?;
    if !outcome.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&outcome.stdout);

    // group format: name:password:gid:member1,member2
    // Note(cc): the member list after the third colon may contain commas but not
    // further colons, so a 4-field split is safe.
    let line = stdout.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 4 {
        return Err(GroupStateError::ParseGroup {
            output: stdout.into_owned(),
        });
    }

    let gid = parts[2]
        .parse::<u32>()
        .map_err(|source| GroupStateError::ParseGid {
            value: parts[2].to_string(),
            source,
        })?;

    let members: Vec<String> = parts[3]
        .trim_end()
        .split(',')
        .filter(|member| !member.is_empty())
        .map(str::to_string)
        .collect();

    Ok(Some(GroupEntry { gid, members }))
}
