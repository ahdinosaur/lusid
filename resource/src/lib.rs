//! Resource types — the user-facing "thing I want on my machine" layer.
//!
//! Each resource (apt, file, pacman, command, git) describes one kind of managed
//! system entity. The pipeline for every resource is the same five-step shape,
//! captured by the [`ResourceType`] trait:
//!
//! 1. **Params** — friendly user-facing struct, deserialised from the plan's Rimu
//!    value via the declared [`ParamTypes`] schema.
//! 2. **Resource** — one or more "atoms" produced from Params. One apt
//!    `packages: [a, b]` param expands to two atoms (one per package). Atoms are
//!    arranged in a [`CausalityTree`] so resource-internal ordering can be declared.
//! 3. **State** — current observed state for an atom (e.g. Installed/NotInstalled).
//! 4. **Change** — the delta from State to the desired Resource. `None` means
//!    "already matches".
//! 5. **Operations** — the concrete actions (apt install, write file, etc.) derived
//!    from the Change. Lives in the `lusid-operation` crate.
//!
//! The crate-level [`Resource`] / [`ResourceState`] / [`ResourceChange`] /
//! [`ResourceParams`] enums are plain dispatchers — each variant boxes the per-type
//! data and delegates through the trait. Adding a new resource means: writing a
//! `ResourceType` impl, adding a variant to each of these enums, and threading it
//! through the match arms.

use std::fmt::Display;

use crate::resources::file::FileParams;
pub use crate::resources::*;

use async_trait::async_trait;
use lusid_causality::CausalityTree;
use lusid_ctx::Context;
use lusid_operation::Operation;
use lusid_params::ParamTypes;
use lusid_view::Render;
use rimu::Spanned;
use serde::de::DeserializeOwned;
use thiserror::Error;

mod resources;

use crate::resources::apt::{Apt, AptChange, AptParams, AptResource, AptState};
use crate::resources::apt_repo::{
    AptRepo, AptRepoChange, AptRepoParams, AptRepoResource, AptRepoState,
};
use crate::resources::command::{
    Command, CommandChange, CommandParams, CommandResource, CommandState,
};
use crate::resources::file::{File, FileChange, FileResource, FileState};
use crate::resources::git::{Git, GitChange, GitParams, GitResource, GitState};
use crate::resources::group::{Group, GroupChange, GroupParams, GroupResource, GroupState};
use crate::resources::pacman::{Pacman, PacmanChange, PacmanParams, PacmanResource, PacmanState};
use crate::resources::systemd::{
    Systemd, SystemdChange, SystemdParams, SystemdResource, SystemdState,
};
use crate::resources::user::{User, UserChange, UserParams, UserResource, UserState};

/// The full pipeline for a single resource type.
///
/// Implementors are zero-sized marker types (e.g. `Apt`, `File`); all the real data lives
/// in the associated types. The flow for one plan item is:
///
/// `Params -> resources() -> State (via state()) -> change() -> operations()`
#[async_trait]
pub trait ResourceType {
    /// Stable identifier used as the `@core/<ID>` module name in plans.
    const ID: &'static str;

    /// Rimu schema used to validate this resource's params. `None` means "no fields".
    fn param_types() -> Option<Spanned<ParamTypes>>;

    /// User-facing params struct (deserialised from the plan's Rimu value).
    type Params: Render + DeserializeOwned;

    /// Indivisible unit of managed state. One `Params` may produce many atoms (e.g. one
    /// per package in a packages list).
    type Resource: Render;

    /// Expand params into one or more resource atoms, organised as a causality tree so
    /// intra-resource ordering (e.g. "chmod after write") can be declared via meta ids.
    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>>;

    /// Observed state of a single atom on the target machine.
    type State: Render;

    /// Failures that can occur while observing state (command exec, parse errors, etc.).
    type StateError;

    /// Observe the current state of `resource` on the target machine.
    async fn state(
        ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError>;

    /// The delta from `State` to the desired `Resource`.
    type Change: Render;

    /// Compute the change needed to reach `resource` from `state`. `None` means no-op.
    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change>;

    /// Lower a change into concrete operations (apt install, write file, …) to execute.
    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>>;
}

/// Dispatcher over every resource's `Params` variant. Produced by the planner from the
/// `@core/<id>` module a plan item refers to.
#[derive(Debug, Clone)]
pub enum ResourceParams {
    Apt(AptParams),
    AptRepo(AptRepoParams),
    File(FileParams),
    Pacman(PacmanParams),
    Command(CommandParams),
    Git(GitParams),
    Systemd(SystemdParams),
    User(UserParams),
    Group(GroupParams),
}

impl Display for ResourceParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ResourceParams::*;
        match self {
            Apt(params) => params.fmt(f),
            AptRepo(params) => params.fmt(f),
            File(params) => params.fmt(f),
            Pacman(params) => params.fmt(f),
            Command(params) => params.fmt(f),
            Git(params) => params.fmt(f),
            Systemd(params) => params.fmt(f),
            User(params) => params.fmt(f),
            Group(params) => params.fmt(f),
        }
    }
}

impl Render for ResourceParams {
    fn render(&self) -> lusid_view::View {
        use ResourceParams::*;
        match self {
            Apt(params) => params.render(),
            AptRepo(params) => params.render(),
            File(params) => params.render(),
            Pacman(params) => params.render(),
            Command(params) => params.render(),
            Git(params) => params.render(),
            Systemd(params) => params.render(),
            User(params) => params.render(),
            Group(params) => params.render(),
        }
    }
}

/// Dispatcher over every resource's `Resource` atom.
#[derive(Debug, Clone)]
pub enum Resource {
    Apt(AptResource),
    AptRepo(AptRepoResource),
    File(FileResource),
    Pacman(PacmanResource),
    Command(CommandResource),
    Git(GitResource),
    Systemd(SystemdResource),
    User(UserResource),
    Group(GroupResource),
}

impl Display for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Resource::*;
        match self {
            Apt(apt) => apt.fmt(f),
            AptRepo(apt_repo) => apt_repo.fmt(f),
            File(file) => file.fmt(f),
            Pacman(pacman) => pacman.fmt(f),
            Command(command) => command.fmt(f),
            Git(git) => git.fmt(f),
            Systemd(systemd) => systemd.fmt(f),
            User(user) => user.fmt(f),
            Group(group) => group.fmt(f),
        }
    }
}

impl Render for Resource {
    fn render(&self) -> lusid_view::View {
        use Resource::*;
        match self {
            Apt(params) => params.render(),
            AptRepo(params) => params.render(),
            File(params) => params.render(),
            Pacman(params) => params.render(),
            Command(params) => params.render(),
            Git(params) => params.render(),
            Systemd(params) => params.render(),
            User(params) => params.render(),
            Group(params) => params.render(),
        }
    }
}

/// Dispatcher over every resource's observed `State`.
///
/// Invariant: the variant always matches the originating `Resource` variant — see
/// [`Resource::change`] for the enforcement point.
#[derive(Debug, Clone)]
pub enum ResourceState {
    Apt(AptState),
    AptRepo(AptRepoState),
    File(FileState),
    Pacman(PacmanState),
    Command(CommandState),
    Git(GitState),
    Systemd(SystemdState),
    User(UserState),
    Group(GroupState),
}

impl Display for ResourceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ResourceState::*;
        match self {
            Apt(apt) => apt.fmt(f),
            AptRepo(apt_repo) => apt_repo.fmt(f),
            File(file) => file.fmt(f),
            Pacman(pacman) => pacman.fmt(f),
            Command(command) => command.fmt(f),
            Git(git) => git.fmt(f),
            Systemd(systemd) => systemd.fmt(f),
            User(user) => user.fmt(f),
            Group(group) => group.fmt(f),
        }
    }
}

impl Render for ResourceState {
    fn render(&self) -> lusid_view::View {
        use ResourceState::*;
        match self {
            Apt(params) => params.render(),
            AptRepo(params) => params.render(),
            File(params) => params.render(),
            Pacman(params) => params.render(),
            Command(params) => params.render(),
            Git(params) => params.render(),
            Systemd(params) => params.render(),
            User(params) => params.render(),
            Group(params) => params.render(),
        }
    }
}

/// Dispatcher over any per-resource `StateError`. The wrapped error carries the original
/// span/context; the variant just tells you which resource family failed.
#[derive(Error, Debug)]
pub enum ResourceStateError {
    #[error("apt state error: {0}")]
    Apt(#[from] <Apt as ResourceType>::StateError),

    #[error("apt-repo state error: {0}")]
    AptRepo(#[from] <AptRepo as ResourceType>::StateError),

    #[error("file state error: {0}")]
    File(#[from] <File as ResourceType>::StateError),
    #[error("pacman state error: {0}")]
    Pacman(#[from] <Pacman as ResourceType>::StateError),
    #[error("command state error: {0}")]
    Command(#[from] <Command as ResourceType>::StateError),

    #[error("git state error: {0}")]
    Git(#[from] <Git as ResourceType>::StateError),

    #[error("systemd state error: {0}")]
    Systemd(#[from] <Systemd as ResourceType>::StateError),

    #[error("user state error: {0}")]
    User(#[from] <User as ResourceType>::StateError),

    #[error("group state error: {0}")]
    Group(#[from] <Group as ResourceType>::StateError),
}

/// Dispatcher over every resource's `Change`.
#[derive(Debug, Clone)]
pub enum ResourceChange {
    Apt(AptChange),
    AptRepo(AptRepoChange),
    File(FileChange),
    Pacman(PacmanChange),
    Command(CommandChange),
    Git(GitChange),
    Systemd(SystemdChange),
    User(UserChange),
    Group(GroupChange),
}

impl Display for ResourceChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ResourceChange::*;
        match self {
            Apt(apt) => apt.fmt(f),
            AptRepo(apt_repo) => apt_repo.fmt(f),
            File(file) => file.fmt(f),
            Pacman(pacman) => pacman.fmt(f),
            Command(command) => command.fmt(f),
            Git(git) => git.fmt(f),
            Systemd(systemd) => systemd.fmt(f),
            User(user) => user.fmt(f),
            Group(group) => group.fmt(f),
        }
    }
}

impl Render for ResourceChange {
    fn render(&self) -> lusid_view::View {
        use ResourceChange::*;
        match self {
            Apt(params) => params.render(),
            AptRepo(params) => params.render(),
            File(params) => params.render(),
            Pacman(params) => params.render(),
            Command(params) => params.render(),
            Git(params) => params.render(),
            Systemd(params) => params.render(),
            User(params) => params.render(),
            Group(params) => params.render(),
        }
    }
}

impl ResourceParams {
    /// Expand params into resource atoms and lift each per-type tree into the
    /// top-level [`Resource`] dispatcher.
    pub fn resources(self) -> Vec<CausalityTree<Resource>> {
        fn typed<R: ResourceType>(
            params: R::Params,
            map: impl Fn(R::Resource) -> Resource + Copy,
        ) -> Vec<CausalityTree<Resource>> {
            R::resources(params)
                .into_iter()
                .map(|tree| tree.map(map))
                .collect()
        }

        match self {
            ResourceParams::Apt(params) => typed::<Apt>(params, Resource::Apt),
            ResourceParams::AptRepo(params) => typed::<AptRepo>(params, Resource::AptRepo),
            ResourceParams::File(params) => typed::<File>(params, Resource::File),
            ResourceParams::Pacman(params) => typed::<Pacman>(params, Resource::Pacman),
            ResourceParams::Command(params) => typed::<Command>(params, Resource::Command),
            ResourceParams::Git(params) => typed::<Git>(params, Resource::Git),
            ResourceParams::Systemd(params) => typed::<Systemd>(params, Resource::Systemd),
            ResourceParams::User(params) => typed::<User>(params, Resource::User),
            ResourceParams::Group(params) => typed::<Group>(params, Resource::Group),
        }
    }
}

impl Resource {
    /// Observe this atom on the target machine and return a [`ResourceState`] in the
    /// matching variant.
    pub async fn state(&self, ctx: &mut Context) -> Result<ResourceState, ResourceStateError> {
        async fn typed<R: ResourceType>(
            ctx: &mut Context,
            resource: &R::Resource,
            map: impl Fn(R::State) -> ResourceState,
            map_err: impl Fn(R::StateError) -> ResourceStateError,
        ) -> Result<ResourceState, ResourceStateError> {
            R::state(ctx, resource).await.map(map).map_err(map_err)
        }

        match self {
            Resource::Apt(resource) => {
                typed::<Apt>(ctx, resource, ResourceState::Apt, ResourceStateError::Apt).await
            }
            Resource::AptRepo(resource) => {
                typed::<AptRepo>(
                    ctx,
                    resource,
                    ResourceState::AptRepo,
                    ResourceStateError::AptRepo,
                )
                .await
            }
            Resource::File(resource) => {
                typed::<File>(ctx, resource, ResourceState::File, ResourceStateError::File).await
            }
            Resource::Pacman(resource) => {
                typed::<Pacman>(
                    ctx,
                    resource,
                    ResourceState::Pacman,
                    ResourceStateError::Pacman,
                )
                .await
            }
            Resource::Command(resource) => {
                typed::<Command>(
                    ctx,
                    resource,
                    ResourceState::Command,
                    ResourceStateError::Command,
                )
                .await
            }
            Resource::Git(resource) => {
                typed::<Git>(ctx, resource, ResourceState::Git, ResourceStateError::Git).await
            }
            Resource::Systemd(resource) => {
                typed::<Systemd>(
                    ctx,
                    resource,
                    ResourceState::Systemd,
                    ResourceStateError::Systemd,
                )
                .await
            }
            Resource::User(resource) => {
                typed::<User>(
                    ctx,
                    resource,
                    ResourceState::User,
                    ResourceStateError::User,
                )
                .await
            }
            Resource::Group(resource) => {
                typed::<Group>(
                    ctx,
                    resource,
                    ResourceState::Group,
                    ResourceStateError::Group,
                )
                .await
            }
        }
    }

    /// Diff this atom against its observed state. `None` means "already correct".
    ///
    /// Panics if the state variant does not match the resource variant — this is a
    /// programmer error since [`Self::state`] always returns the matching variant.
    pub fn change(&self, state: &ResourceState) -> Option<ResourceChange> {
        fn typed<R: ResourceType>(
            resource: &R::Resource,
            state: &R::State,
            map: impl Fn(R::Change) -> ResourceChange,
        ) -> Option<ResourceChange> {
            R::change(resource, state).map(map)
        }

        // Note(cc): the `#[allow(unreachable_patterns)]` dates from when only one
        // resource existed and the `_` arm really was unreachable. With five variants
        // the `_` arm is reachable (e.g. `(Resource::Apt, ResourceState::File)`) and
        // the allow is likely stale — leaving it for now to avoid churn, but it can
        // probably be removed.
        #[allow(unreachable_patterns)]
        match (self, state) {
            (Resource::Apt(resource), ResourceState::Apt(state)) => {
                typed::<Apt>(resource, state, ResourceChange::Apt)
            }
            (Resource::AptRepo(resource), ResourceState::AptRepo(state)) => {
                typed::<AptRepo>(resource, state, ResourceChange::AptRepo)
            }
            (Resource::File(resource), ResourceState::File(state)) => {
                typed::<File>(resource, state, ResourceChange::File)
            }
            (Resource::Pacman(resource), ResourceState::Pacman(state)) => {
                typed::<Pacman>(resource, state, ResourceChange::Pacman)
            }
            (Resource::Command(resource), ResourceState::Command(state)) => {
                typed::<Command>(resource, state, ResourceChange::Command)
            }
            (Resource::Git(resource), ResourceState::Git(state)) => {
                typed::<Git>(resource, state, ResourceChange::Git)
            }
            (Resource::Systemd(resource), ResourceState::Systemd(state)) => {
                typed::<Systemd>(resource, state, ResourceChange::Systemd)
            }
            (Resource::User(resource), ResourceState::User(state)) => {
                typed::<User>(resource, state, ResourceChange::User)
            }
            (Resource::Group(resource), ResourceState::Group(state)) => {
                typed::<Group>(resource, state, ResourceChange::Group)
            }
            _ => {
                // Programmer error, should never happen, or if it does should be immediately obvious.
                panic!("Unmatched resource and state")
            }
        }
    }
}

impl ResourceChange {
    /// Lower a change into the concrete operations that execute it, preserving any
    /// intra-change ordering (e.g. `apt update` before `apt install`).
    pub fn operations(self) -> Vec<CausalityTree<Operation>> {
        match self {
            ResourceChange::Apt(change) => Apt::operations(change),
            ResourceChange::AptRepo(change) => AptRepo::operations(change),
            ResourceChange::File(change) => File::operations(change),
            ResourceChange::Pacman(change) => Pacman::operations(change),
            ResourceChange::Command(change) => Command::operations(change),
            ResourceChange::Git(change) => Git::operations(change),
            ResourceChange::Systemd(change) => Systemd::operations(change),
            ResourceChange::User(change) => User::operations(change),
            ResourceChange::Group(change) => Group::operations(change),
        }
    }
}
