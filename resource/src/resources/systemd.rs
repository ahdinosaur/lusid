use std::fmt::Display;

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_operation::{Operation, operations::systemd::SystemdOperation};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use rimu_interop::FromRimu;
use thiserror::Error;

use crate::ResourceType;

#[derive(Debug, Clone, FromRimu)]
pub struct SystemdParams {
    pub name: String,
    pub enabled: Option<bool>,
    pub active: Option<bool>,
}

impl Display for SystemdParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            name,
            enabled,
            active,
        } = self;
        write!(
            f,
            "Systemd(name = {name}, enabled = {enabled:?}, active = {active:?})"
        )
    }
}

impl_display_render!(SystemdParams);

#[derive(Debug, Clone)]
pub struct SystemdResource {
    pub name: String,
    pub enabled: bool,
    pub active: bool,
}

impl Display for SystemdResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            name,
            enabled,
            active,
        } = self;
        write!(
            f,
            "Systemd(name = {name}, enabled = {enabled}, active = {active})"
        )
    }
}

impl_display_render!(SystemdResource);

#[derive(Debug, Clone)]
pub struct SystemdState {
    pub enabled: bool,
    pub active: bool,
}

impl Display for SystemdState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { enabled, active } = self;
        write!(f, "Systemd(enabled = {enabled}, active = {active})")
    }
}

impl_display_render!(SystemdState);

#[derive(Error, Debug)]
pub enum SystemdStateError {
    #[error(transparent)]
    Command(#[from] CommandError),

    #[error("systemd unit not found: {name}")]
    UnitNotFound { name: String },

    #[error("failed to parse systemctl show output: missing {field}")]
    MissingField { field: &'static str },

    #[error("unknown systemd ActiveState: {state}")]
    UnknownActiveState { state: String },

    #[error("unknown systemd UnitFileState: {state}")]
    UnknownUnitFileState { state: String },
}

/// Desired-state delta for a systemd unit. `enable` / `active` are `Some(desired)` if a
/// transition is needed on that dimension, `None` if the current state already matches.
/// At least one field is `Some` — otherwise [`Systemd::change`] returns `None`.
#[derive(Debug, Clone)]
pub struct SystemdChange {
    pub name: String,
    pub enable: Option<bool>,
    pub active: Option<bool>,
}

impl Display for SystemdChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            name,
            enable,
            active,
        } = self;
        let mut verbs: Vec<&'static str> = Vec::new();
        if let Some(enable) = enable {
            verbs.push(if *enable { "enable" } else { "disable" });
        }
        if let Some(active) = active {
            verbs.push(if *active { "start" } else { "stop" });
        }
        write!(f, "Systemd::{}({})", verbs.join("+"), name)
    }
}

impl_display_render!(SystemdChange);

#[derive(Debug, Clone)]
pub struct Systemd;

#[async_trait]
impl ResourceType for Systemd {
    const ID: &'static str = "systemd";

    fn param_types() -> Option<Spanned<ParamTypes>> {
        let span = Span::new(SourceId::empty(), 0, 0);
        let field = |typ, required: bool| {
            let mut param = ParamField::new(typ);
            if !required {
                param = param.with_optional();
            }
            Spanned::new(param, span.clone())
        };

        Some(Spanned::new(
            ParamTypes::Struct(indexmap! {
                "name".to_string() => field(ParamType::String, true),
                "enabled".to_string() => field(ParamType::Boolean, false),
                "active".to_string() => field(ParamType::Boolean, false),
            }),
            span,
        ))
    }

    type Params = SystemdParams;
    type Resource = SystemdResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        vec![CausalityTree::leaf(
            CausalityMeta::default(),
            SystemdResource {
                name: params.name,
                enabled: params.enabled.unwrap_or(true),
                active: params.active.unwrap_or(true),
            },
        )]
    }

    type State = SystemdState;
    type StateError = SystemdStateError;

    async fn state(
        _ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        // `systemctl show` is the stable, scriptable interface: it always exits 0 and
        // emits `Key=Value` lines. For a missing unit it still exits 0 with
        // `LoadState=not-found`, so we detect missing units from the output rather than
        // from exit status.
        let output = Command::new("systemctl")
            .args([
                "show",
                "--property=LoadState,ActiveState,UnitFileState",
                &resource.name,
            ])
            .run()
            .await?;
        let output = String::from_utf8_lossy(&output);

        let mut load_state: Option<&str> = None;
        let mut active_state: Option<&str> = None;
        let mut unit_file_state: Option<&str> = None;
        for line in output.lines() {
            if let Some(v) = line.strip_prefix("LoadState=") {
                load_state = Some(v);
            } else if let Some(v) = line.strip_prefix("ActiveState=") {
                active_state = Some(v);
            } else if let Some(v) = line.strip_prefix("UnitFileState=") {
                unit_file_state = Some(v);
            }
        }

        let load_state =
            load_state.ok_or(SystemdStateError::MissingField { field: "LoadState" })?;
        if load_state == "not-found" {
            return Err(SystemdStateError::UnitNotFound {
                name: resource.name.clone(),
            });
        }

        let active_state = active_state.ok_or(SystemdStateError::MissingField {
            field: "ActiveState",
        })?;
        let active = parse_active_state(active_state)?;

        let unit_file_state = unit_file_state.ok_or(SystemdStateError::MissingField {
            field: "UnitFileState",
        })?;
        let enabled = parse_unit_file_state(unit_file_state)?;

        Ok(SystemdState { enabled, active })
    }

    type Change = SystemdChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        let enable = (resource.enabled != state.enabled).then_some(resource.enabled);
        let active = (resource.active != state.active).then_some(resource.active);
        if enable.is_none() && active.is_none() {
            return None;
        }
        Some(SystemdChange {
            name: resource.name.clone(),
            enable,
            active,
        })
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        let SystemdChange {
            name,
            enable,
            active,
        } = change;
        let mut ops: Vec<CausalityTree<Operation>> = Vec::new();
        if let Some(enable) = enable {
            let op = if enable {
                SystemdOperation::Enable { name: name.clone() }
            } else {
                SystemdOperation::Disable { name: name.clone() }
            };
            ops.push(CausalityTree::leaf(
                CausalityMeta::default(),
                Operation::Systemd(op),
            ));
        }
        if let Some(active) = active {
            let op = if active {
                SystemdOperation::Start { name }
            } else {
                SystemdOperation::Stop { name }
            };
            ops.push(CausalityTree::leaf(
                CausalityMeta::default(),
                Operation::Systemd(op),
            ));
        }
        ops
    }
}

/// Map systemd's `ActiveState` string to a boolean "is running" view.
///
/// Transitional states (`activating`, `reloading`) are treated as `active` to avoid
/// thrashing a service mid-transition. `failed` maps to `inactive` so that a user who
/// declared `active: true` still gets a `start` attempt — if the unit keeps failing,
/// the apply surfaces systemctl's stderr.
fn parse_active_state(state: &str) -> Result<bool, SystemdStateError> {
    match state {
        "active" | "reloading" | "activating" => Ok(true),
        "inactive" | "deactivating" | "failed" => Ok(false),
        other => Err(SystemdStateError::UnknownActiveState {
            state: other.to_string(),
        }),
    }
}

/// Map systemd's `UnitFileState` string to a boolean "is enabled at boot" view.
///
/// `static`, `alias`, `indirect`, `linked*`, `generated`, and `transient` all report
/// as enabled because their presence is authoritative — `systemctl enable` is a no-op
/// on these and `disable` refuses. `masked` reports as disabled (masking blocks
/// activation entirely, which is stricter than disable). Empty `UnitFileState` is
/// common for runtime-only units that have no install hook; treat as disabled.
fn parse_unit_file_state(state: &str) -> Result<bool, SystemdStateError> {
    match state {
        "enabled" | "enabled-runtime" | "static" | "alias" | "indirect" | "linked"
        | "linked-runtime" | "generated" | "transient" => Ok(true),
        "disabled" | "masked" | "masked-runtime" | "" => Ok(false),
        other => Err(SystemdStateError::UnknownUnitFileState {
            state: other.to_string(),
        }),
    }
}
