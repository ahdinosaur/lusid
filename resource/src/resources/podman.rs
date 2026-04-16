use std::fmt::Display;

use async_trait::async_trait;
use indexmap::indexmap;
use lusid_causality::{CausalityMeta, CausalityTree};
use lusid_cmd::{Command, CommandError};
use lusid_ctx::Context;
use lusid_operation::{Operation, operations::podman::PodmanOperation};
use lusid_params::{ParamField, ParamType, ParamTypes};
use lusid_view::impl_display_render;
use rimu::{SourceId, Span, Spanned};
use serde::Deserialize;
use thiserror::Error;

use crate::ResourceType;

/// Plan-level parameters for the `@core/podman` resource.
///
/// Tagged by `state: "present" | "absent"`. Mirrors the shape of Ansible's
/// `containers.podman.podman_container` at a conservative subset — enough to
/// declare a long-running container without wrapping every podman flag.
// TODO(cc): spec-drift detection is limited to `image` today. Changes to
// `command`, `env`, `ports`, `volumes`, or `restart_policy` against an
// existing container will NOT trigger a recreate. Users who change those
// fields need to flip `state: absent` first, or rely on an image tag bump.
// Detecting drift requires normalising `podman inspect` output (which
// canonicalises ports/volumes/env in non-obvious ways) — worth deferring
// until we have a concrete user reporting the gap.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum PodmanParams {
    Present {
        name: String,
        image: String,
        command: Option<Vec<String>>,
        env: Option<Vec<String>>,
        ports: Option<Vec<String>>,
        volumes: Option<Vec<String>>,
        restart_policy: Option<String>,
        running: Option<bool>,
    },
    Absent {
        name: String,
    },
}

impl Display for PodmanParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PodmanParams::Present { name, image, .. } => {
                write!(f, "Podman::Present(name = {name}, image = {image})")
            }
            PodmanParams::Absent { name } => write!(f, "Podman::Absent(name = {name})"),
        }
    }
}

impl_display_render!(PodmanParams);

#[derive(Debug, Clone)]
pub enum PodmanResource {
    Present {
        name: String,
        image: String,
        command: Option<Vec<String>>,
        env: Vec<String>,
        ports: Vec<String>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
        running: bool,
    },
    Absent {
        name: String,
    },
}

impl Display for PodmanResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PodmanResource::Present {
                name,
                image,
                running,
                ..
            } => write!(
                f,
                "Podman::Present(name = {name}, image = {image}, running = {running})"
            ),
            PodmanResource::Absent { name } => write!(f, "Podman::Absent(name = {name})"),
        }
    }
}

impl_display_render!(PodmanResource);

#[derive(Debug, Clone)]
pub enum PodmanState {
    Absent,
    Present { image: String, running: bool },
}

impl Display for PodmanState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PodmanState::Absent => write!(f, "Podman::Absent"),
            PodmanState::Present { image, running } => {
                write!(f, "Podman::Present(image = {image}, running = {running})")
            }
        }
    }
}

impl_display_render!(PodmanState);

#[derive(Error, Debug)]
pub enum PodmanStateError {
    #[error(transparent)]
    Command(#[from] CommandError),

    #[error("failed to parse podman inspect output: {output}")]
    ParseInspect { output: String },
}

#[derive(Debug, Clone)]
pub enum PodmanChange {
    /// Container doesn't exist — create and optionally start.
    Create {
        name: String,
        image: String,
        command: Option<Vec<String>>,
        env: Vec<String>,
        ports: Vec<String>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
        start: bool,
    },
    /// Container exists with the right image, but needs to be started.
    Start { name: String },
    /// Container exists with the right image, but needs to be stopped.
    Stop { name: String },
    /// Container exists but its image no longer matches; remove and recreate.
    Recreate {
        name: String,
        image: String,
        command: Option<Vec<String>>,
        env: Vec<String>,
        ports: Vec<String>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
        start: bool,
    },
    /// Declared absent but the container exists; remove it.
    Remove { name: String },
}

impl Display for PodmanChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PodmanChange::Create { name, image, .. } => {
                write!(f, "Podman::Create(name = {name}, image = {image})")
            }
            PodmanChange::Start { name } => write!(f, "Podman::Start({name})"),
            PodmanChange::Stop { name } => write!(f, "Podman::Stop({name})"),
            PodmanChange::Recreate { name, image, .. } => {
                write!(f, "Podman::Recreate(name = {name}, image = {image})")
            }
            PodmanChange::Remove { name } => write!(f, "Podman::Remove({name})"),
        }
    }
}

impl_display_render!(PodmanChange);

#[derive(Debug, Clone)]
pub struct Podman;

#[async_trait]
impl ResourceType for Podman {
    const ID: &'static str = "podman";

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
                    "image".to_string() => field(ParamType::String, true),
                    "command".to_string() => field(string_list(), false),
                    "env".to_string() => field(string_list(), false),
                    "ports".to_string() => field(string_list(), false),
                    "volumes".to_string() => field(string_list(), false),
                    "restart_policy".to_string() => field(ParamType::String, false),
                    "running".to_string() => field(ParamType::Boolean, false),
                },
                indexmap! {
                    "state".to_string() => field(ParamType::Literal("absent".into()), true),
                    "name".to_string() => field(ParamType::String, true),
                },
            ]),
            span,
        ))
    }

    type Params = PodmanParams;
    type Resource = PodmanResource;

    fn resources(params: Self::Params) -> Vec<CausalityTree<Self::Resource>> {
        let resource = match params {
            PodmanParams::Present {
                name,
                image,
                command,
                env,
                ports,
                volumes,
                restart_policy,
                running,
            } => PodmanResource::Present {
                name,
                image,
                command,
                env: env.unwrap_or_default(),
                ports: ports.unwrap_or_default(),
                volumes: volumes.unwrap_or_default(),
                restart_policy,
                running: running.unwrap_or(true),
            },
            PodmanParams::Absent { name } => PodmanResource::Absent { name },
        };
        vec![CausalityTree::leaf(CausalityMeta::default(), resource)]
    }

    type State = PodmanState;
    type StateError = PodmanStateError;

    async fn state(
        _ctx: &mut Context,
        resource: &Self::Resource,
    ) -> Result<Self::State, Self::StateError> {
        let name = match resource {
            PodmanResource::Present { name, .. } | PodmanResource::Absent { name } => name,
        };

        // `podman container inspect` exits non-zero (125) when the container is
        // missing, which `outcome()` surfaces without raising. Distinguishing
        // "absent" from "podman itself failed" via stderr is unreliable across
        // versions, so we treat any non-success as Absent. A broken podman
        // install will then surface at apply-time on the first create.
        let outcome = Command::new("podman")
            .args([
                "container",
                "inspect",
                "--format",
                "{{.ImageName}}||{{.State.Running}}",
                name,
            ])
            .outcome()
            .await?;
        if !outcome.status.success() {
            return Ok(PodmanState::Absent);
        }

        let stdout = String::from_utf8_lossy(&outcome.stdout);
        let line = stdout.trim();
        let (image, running_raw) =
            line.split_once("||")
                .ok_or_else(|| PodmanStateError::ParseInspect {
                    output: stdout.to_string(),
                })?;
        let running = match running_raw.trim() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(PodmanStateError::ParseInspect {
                    output: stdout.to_string(),
                });
            }
        };
        Ok(PodmanState::Present {
            image: image.trim().to_string(),
            running,
        })
    }

    type Change = PodmanChange;

    fn change(resource: &Self::Resource, state: &Self::State) -> Option<Self::Change> {
        match (resource, state) {
            (PodmanResource::Absent { .. }, PodmanState::Absent) => None,

            (PodmanResource::Absent { name }, PodmanState::Present { .. }) => {
                Some(PodmanChange::Remove { name: name.clone() })
            }

            (
                PodmanResource::Present {
                    name,
                    image,
                    command,
                    env,
                    ports,
                    volumes,
                    restart_policy,
                    running,
                },
                PodmanState::Absent,
            ) => Some(PodmanChange::Create {
                name: name.clone(),
                image: image.clone(),
                command: command.clone(),
                env: env.clone(),
                ports: ports.clone(),
                volumes: volumes.clone(),
                restart_policy: restart_policy.clone(),
                start: *running,
            }),

            (
                PodmanResource::Present {
                    name,
                    image,
                    command,
                    env,
                    ports,
                    volumes,
                    restart_policy,
                    running,
                },
                PodmanState::Present {
                    image: current_image,
                    running: current_running,
                },
            ) => {
                // Note(cc): `podman inspect` returns the image as either the
                // user-facing reference (e.g. `docker.io/library/nginx:latest`)
                // or the sha256 digest, depending on how the container was
                // created. Raw string compare means a declared `nginx:latest`
                // against an inspect-reported `docker.io/library/nginx:latest`
                // will spuriously recreate. Left as-is for v1; revisit if it
                // bites in practice.
                if image != current_image {
                    Some(PodmanChange::Recreate {
                        name: name.clone(),
                        image: image.clone(),
                        command: command.clone(),
                        env: env.clone(),
                        ports: ports.clone(),
                        volumes: volumes.clone(),
                        restart_policy: restart_policy.clone(),
                        start: *running,
                    })
                } else if *running != *current_running {
                    if *running {
                        Some(PodmanChange::Start { name: name.clone() })
                    } else {
                        Some(PodmanChange::Stop { name: name.clone() })
                    }
                } else {
                    None
                }
            }
        }
    }

    fn operations(change: Self::Change) -> Vec<CausalityTree<Operation>> {
        match change {
            PodmanChange::Create {
                name,
                image,
                command,
                env,
                ports,
                volumes,
                restart_policy,
                start,
            } => create_ops(
                name,
                image,
                command,
                env,
                ports,
                volumes,
                restart_policy,
                start,
                None,
            ),
            PodmanChange::Start { name } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                Operation::Podman(PodmanOperation::Start { name }),
            )],
            PodmanChange::Stop { name } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                Operation::Podman(PodmanOperation::Stop { name }),
            )],
            PodmanChange::Recreate {
                name,
                image,
                command,
                env,
                ports,
                volumes,
                restart_policy,
                start,
            } => create_ops(
                name,
                image,
                command,
                env,
                ports,
                volumes,
                restart_policy,
                start,
                Some("remove"),
            ),
            PodmanChange::Remove { name } => vec![CausalityTree::leaf(
                CausalityMeta::default(),
                Operation::Podman(PodmanOperation::Remove { name }),
            )],
        }
    }
}

/// Build the Create (+ optional Start) operations, optionally preceded by a
/// Remove op when `remove_id` is `Some`. Used for both `Create` and `Recreate`
/// changes to keep the causality wiring in one place.
#[allow(clippy::too_many_arguments)]
fn create_ops(
    name: String,
    image: String,
    command: Option<Vec<String>>,
    env: Vec<String>,
    ports: Vec<String>,
    volumes: Vec<String>,
    restart_policy: Option<String>,
    start: bool,
    remove_id: Option<&'static str>,
) -> Vec<CausalityTree<Operation>> {
    let mut ops: Vec<CausalityTree<Operation>> = Vec::new();

    if let Some(id) = remove_id {
        ops.push(CausalityTree::leaf(
            CausalityMeta::id(id.into()),
            Operation::Podman(PodmanOperation::Remove { name: name.clone() }),
        ));
    }

    let create_meta = CausalityMeta {
        id: Some("create".into()),
        requires: remove_id.map(|id| vec![id.into()]).unwrap_or_default(),
        required_by: vec![],
    };
    ops.push(CausalityTree::leaf(
        create_meta,
        Operation::Podman(PodmanOperation::Create {
            name: name.clone(),
            image,
            command,
            env,
            ports,
            volumes,
            restart_policy,
        }),
    ));

    if start {
        ops.push(CausalityTree::leaf(
            CausalityMeta::requires(vec!["create".into()]),
            Operation::Podman(PodmanOperation::Start { name }),
        ));
    }

    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present_resource() -> PodmanResource {
        PodmanResource::Present {
            name: "web".into(),
            image: "docker.io/library/nginx:latest".into(),
            command: None,
            env: vec![],
            ports: vec!["8080:80".into()],
            volumes: vec![],
            restart_policy: Some("unless-stopped".into()),
            running: true,
        }
    }

    #[test]
    fn change_none_when_matches() {
        let resource = present_resource();
        let state = PodmanState::Present {
            image: "docker.io/library/nginx:latest".into(),
            running: true,
        };
        assert!(Podman::change(&resource, &state).is_none());
    }

    #[test]
    fn change_create_when_absent() {
        let resource = present_resource();
        let change = Podman::change(&resource, &PodmanState::Absent).expect("change");
        assert!(matches!(change, PodmanChange::Create { start: true, .. }));
    }

    #[test]
    fn change_recreate_when_image_differs() {
        let resource = present_resource();
        let state = PodmanState::Present {
            image: "docker.io/library/nginx:1.25".into(),
            running: true,
        };
        let change = Podman::change(&resource, &state).expect("change");
        assert!(matches!(change, PodmanChange::Recreate { .. }));
    }

    #[test]
    fn change_start_when_only_running_differs() {
        let resource = present_resource();
        let state = PodmanState::Present {
            image: "docker.io/library/nginx:latest".into(),
            running: false,
        };
        let change = Podman::change(&resource, &state).expect("change");
        assert!(matches!(change, PodmanChange::Start { .. }));
    }

    #[test]
    fn change_stop_when_declared_not_running() {
        let resource = PodmanResource::Present {
            name: "web".into(),
            image: "docker.io/library/nginx:latest".into(),
            command: None,
            env: vec![],
            ports: vec!["8080:80".into()],
            volumes: vec![],
            restart_policy: Some("unless-stopped".into()),
            running: false,
        };
        let state = PodmanState::Present {
            image: "docker.io/library/nginx:latest".into(),
            running: true,
        };
        let change = Podman::change(&resource, &state).expect("change");
        assert!(matches!(change, PodmanChange::Stop { .. }));
    }

    #[test]
    fn change_remove_when_declared_absent_but_present() {
        let resource = PodmanResource::Absent { name: "web".into() };
        let state = PodmanState::Present {
            image: "docker.io/library/nginx:latest".into(),
            running: true,
        };
        let change = Podman::change(&resource, &state).expect("change");
        assert!(matches!(change, PodmanChange::Remove { .. }));
    }

    #[test]
    fn change_none_when_absent_matches() {
        let resource = PodmanResource::Absent { name: "web".into() };
        assert!(Podman::change(&resource, &PodmanState::Absent).is_none());
    }
}
