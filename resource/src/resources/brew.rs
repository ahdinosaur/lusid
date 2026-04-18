use std::fmt::Display;

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_operation::{Operation, operations::brew::BrewOperation};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_system::OsKind;
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use serde::Deserialize;
use thiserror::Error;

use crate::ResourceType;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BrewParams {
    Package { package: String },
    Packages { packages: Vec<String> },
}

impl Display for BrewParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrewParams::Package { package } => write!(f, "Brew(package = {package})"),
            BrewParams::Packages { packages } => {
                write!(f, "Brew(packages = [{}])", packages.join(", "))
            }
        }
    }
}

impl_display_render!(BrewParams);

#[derive(Debug, Clone)]
pub struct BrewResource {
    pub package: String,
}

impl Display for BrewResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { package } = self;
        write!(f, "Brew({package})")
    }
}

impl_display_render!(BrewResource);

#[derive(Debug, Clone)]
pub enum BrewState {
    NotInstalled,
    Installed,
}

impl Display for BrewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrewState::NotInstalled => write!(f, "Brew::NotInstalled"),
            BrewState::Installed => write!(f, "Brew::Installed"),
        }
    }
}

impl_display_render!(BrewState);

#[derive(Error, Debug)]
pub enum BrewStateError {
    #[error(transparent)]
    Command(#[from] CommandError),
}

// TODO(cc): add an `Uninstall` variant — mirror image of the apt/pacman resources.
// A declared package cannot currently be retracted.
#[derive(Debug, Clone)]
pub enum BrewChange {
    Install { package: String },
}

impl Display for BrewChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrewChange::Install { package } => write!(f, "Brew::Install({package})"),
        }
    }
}

impl_display_render!(BrewChange);

#[derive(Debug, Clone)]
pub struct Brew;

#[async_trait]
impl ResourceType for Brew {
    const ID: &'static str = "brew";

    fn supported_on(os: OsKind) -> bool {
        matches!(os, OsKind::MacOS)
    }

    fn param_types() -> Option<Spanned<ParamTypes>> {
        let span = Span::new(SourceId::empty(), 0, 0);
        Some(Spanned::new(
            ParamTypes::Union(vec![
                indexmap! {
                    "package".to_string() =>
                        Spanned::new(ParamField::new(ParamType::String), span.clone()),
                },
                indexmap! {
                    "packages".to_string() => Spanned::new(
                        ParamField::new(ParamType::List {
                            item: Box::new(Spanned::new(ParamType::String, span.clone())),
                        }),
                        span.clone(),
                    ),
                },
            ]),
            span,
        ))
    }

    type Params = BrewParams;
    type Resource = BrewResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        match params {
            BrewParams::Package { package } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                BrewResource { package },
            )],
            BrewParams::Packages { packages } => packages
                .into_iter()
                .map(|package| {
                    CausalityTree::leaf(CausalityMeta::default(), BrewResource { package })
                })
                .collect(),
        }
    }

    type State = BrewState;
    type StateError = BrewStateError;
    async fn state(
        _ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        // `brew list --versions --formula <pkg>` exits 0 when the formula is
        // installed and non-zero otherwise. Preferring the exit code over parsing
        // stderr: Homebrew's "not installed" message text has changed across
        // major releases ("No such keg" vs. "No available formula with the
        // name...") while the exit status has stayed stable.
        let outcome = Command::new("brew")
            .args(["list", "--versions", "--formula", &resource.package])
            .outcome()
            .await?;
        if outcome.status.success() {
            Ok(BrewState::Installed)
        } else {
            Ok(BrewState::NotInstalled)
        }
    }

    type Change = BrewChange;
    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match state {
            BrewState::Installed => None,
            BrewState::NotInstalled => Some(BrewChange::Install {
                package: resource.package.clone(),
            }),
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        match change {
            BrewChange::Install { package } => {
                vec![
                    CausalityTree::Leaf {
                        node: Operation::Brew(BrewOperation::Update),
                        meta: CausalityMeta::id("update".into()),
                    },
                    CausalityTree::Leaf {
                        node: Operation::Brew(BrewOperation::Install {
                            packages: vec![package],
                        }),
                        meta: CausalityMeta::requires(vec!["update".into()]),
                    },
                ]
            }
        }
    }
}
