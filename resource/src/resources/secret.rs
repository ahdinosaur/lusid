//! `@core/secret`: materialise an age-decrypted plaintext onto the target
//! filesystem, referenced by name (agenix-style — the plan names the secret,
//! the plaintext is resolved at apply time against the decrypted secrets
//! bundle on [`Context`]).
//!
//! Differences from `@core/file` + `type: "contents"`:
//!
//! - `name` names a `*.age` secret by its file stem (e.g. `api_key` →
//!   `secrets/api_key.age`). Plaintext never flows through the plan.
//! - `mode` defaults to `0o600` (owner read/write, nothing for group/world)
//!   when omitted. `@core/file` leaves mode to the umask.
//!
//! Under the hood this delegates to `@core/file`'s state/change/operation
//! machinery — the atoms produced are ordinary [`FileResource::SecretContents`]
//! variants, so downstream scheduling and application are identical. Only
//! the default permissions and the intent expressed by the plan author differ.
//!
//! Note(cc): not as strict as agenix's model (which decrypts onto a tmpfs
//! mount, forces `0400`, root-owned). Those are bigger moves — tmpfs needs
//! an operation that can mount/unmount, and root-owned doesn't work for the
//! current `lusid local apply` running under the logged-in user. Revisit
//! when remote/dev apply lands.

use std::fmt::{self, Display};

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_ctx::Context;
use lusid_operation::{
    Operation,
    operations::file::{FileGroup, FileMode, FilePath, FileUser},
};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use serde::Deserialize;

use crate::ResourceType;
use crate::resources::file::{File, FileChange, FileResource, FileState, FileStateError};

/// Default mode applied when the plan omits `mode`. `0o600` = read/write
/// for the owner only. Overridable by the plan (e.g. a secret that is
/// deliberately group-readable for a multi-user service).
pub const DEFAULT_MODE: u32 = 0o600;

#[derive(Debug, Clone, Deserialize)]
pub struct SecretParams {
    pub name: String,
    pub path: FilePath,
    pub mode: Option<FileMode>,
    pub user: Option<FileUser>,
    pub group: Option<FileGroup>,
}

impl Display for SecretParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(name={}, path={})", self.name, self.path)
    }
}

impl_display_render!(SecretParams);

#[derive(Debug, Clone)]
pub struct Secret;

#[async_trait]
impl ResourceType for Secret {
    const ID: &'static str = "secret";

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
            ParamTypes::Struct(indexmap! {
                "name".to_string() => field(ParamType::String, true),
                "path".to_string() => field(ParamType::TargetPath, true),
                "mode".to_string() => field(ParamType::Number, false),
                "user".to_string() => field(ParamType::String, false),
                "group".to_string() => field(ParamType::String, false),
            }),
            span,
        ))
    }

    type Params = SecretParams;
    type Resource = FileResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        let SecretParams {
            name,
            path,
            mode,
            user,
            group,
        } = params;
        let mode = mode.unwrap_or_else(|| FileMode::new(DEFAULT_MODE));

        let mut nodes = vec![
            CausalityTree::leaf(
                CausalityMeta::id("file".into()),
                FileResource::SecretContents {
                    name,
                    path: path.clone(),
                },
            ),
            // Always emit a Mode atom: the default mode is a guarantee of this
            // module, not a suggestion. A no-op (already-correct mode) collapses
            // to no change at the change() layer.
            CausalityTree::leaf(
                CausalityMeta::requires(vec!["file".into()]),
                FileResource::Mode {
                    path: path.clone(),
                    mode,
                },
            ),
        ];

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

    type State = FileState;
    type StateError = FileStateError;

    async fn state(
        ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        <File as ResourceType>::state(ctx, resource).await
    }

    type Change = FileChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        <File as ResourceType>::change(resource, state)
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        <File as ResourceType>::operations(change)
    }
}
