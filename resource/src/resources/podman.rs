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
// TODO(cc): spec-drift detection covers `image` (canonicalised), `command`,
// `volumes`, and `restart_policy`. Changes to `env` or `ports` will NOT yet
// trigger a recreate — `.Config.Env` mixes user values with image defaults
// (requires a subset check or a label written at create time), and
// `.HostConfig.PortBindings` is a map that doesn't round-trip cleanly from
// the user's "HOST:CONTAINER[/proto]" strings. See `change()` for detail.
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
    Present {
        /// Canonicalized image reference (e.g. `docker.io/library/nginx:latest`).
        image: String,
        running: bool,
        command: Option<Vec<String>>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
    },
}

impl Display for PodmanState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PodmanState::Absent => write!(f, "Podman::Absent"),
            PodmanState::Present { image, running, .. } => {
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

    #[error("failed to parse podman inspect output: {source}\noutput: {output}")]
    ParseInspect {
        #[source]
        source: serde_json::Error,
        output: String,
    },

    #[error("podman inspect returned empty array for container")]
    InspectEmpty,
}

/// Subset of `podman container inspect` JSON we care about for drift detection.
/// Fields we don't compare today (Env, PortBindings) are omitted — see
/// `change()` for the rationale.
#[derive(Debug, Clone, Deserialize)]
struct InspectContainer {
    #[serde(rename = "ImageName", default)]
    image_name: String,

    #[serde(rename = "Config", default)]
    config: InspectConfig,

    #[serde(rename = "HostConfig", default)]
    host_config: InspectHostConfig,

    #[serde(rename = "State", default)]
    state: InspectState,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InspectConfig {
    #[serde(rename = "Cmd", default)]
    cmd: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InspectHostConfig {
    #[serde(rename = "Binds", default)]
    binds: Vec<String>,

    #[serde(rename = "RestartPolicy", default)]
    restart_policy: InspectRestartPolicy,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InspectRestartPolicy {
    #[serde(rename = "Name", default)]
    name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InspectState {
    #[serde(rename = "Running", default)]
    running: bool,
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
            .args(["container", "inspect", name])
            .outcome()
            .await?;
        if !outcome.status.success() {
            return Ok(PodmanState::Absent);
        }

        let containers: Vec<InspectContainer> =
            serde_json::from_slice(&outcome.stdout).map_err(|source| {
                PodmanStateError::ParseInspect {
                    source,
                    output: String::from_utf8_lossy(&outcome.stdout).into_owned(),
                }
            })?;
        let container = containers
            .into_iter()
            .next()
            .ok_or(PodmanStateError::InspectEmpty)?;

        let restart_policy = {
            let name = container.host_config.restart_policy.name;
            // podman reports "" or "no" when no policy is set; normalise both to None
            // so drift detection doesn't flap on the default.
            if name.is_empty() || name == "no" {
                None
            } else {
                Some(name)
            }
        };

        Ok(PodmanState::Present {
            image: canonicalize_image(&container.image_name),
            running: container.state.running,
            command: container.config.cmd,
            volumes: container.host_config.binds,
            restart_policy,
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
                    command: current_command,
                    volumes: current_volumes,
                    restart_policy: current_restart_policy,
                },
            ) => {
                // Note(cc): env and ports are deliberately not compared here.
                // `.Config.Env` includes image defaults plus things like PATH,
                // so a raw compare against the user's declared env would always
                // drift; detecting it properly needs either a subset check or
                // a label written at create time. `.HostConfig.PortBindings`
                // is a map<container/proto, [{HostIp, HostPort}]>, and
                // round-tripping the user's "HOST:CONTAINER[/proto]" strings
                // through that shape isn't trivial. Left as follow-ups.
                let declared_image = canonicalize_image(image);
                let command_drift = command
                    .as_ref()
                    .is_some_and(|declared| Some(declared) != current_command.as_ref());
                let restart_drift = restart_policy
                    .as_ref()
                    .is_some_and(|declared| Some(declared) != current_restart_policy.as_ref());
                let volumes_drift = volumes != current_volumes;

                if declared_image != *current_image
                    || command_drift
                    || restart_drift
                    || volumes_drift
                {
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

/// Best-effort canonicalisation of a container image reference to the form
/// that `podman inspect` typically reports (`<registry>/<repo>:<tag>`), so that
/// `nginx:latest` and `docker.io/library/nginx:latest` compare equal when
/// deciding if the current container matches the declared spec.
///
/// This intentionally does **not** handle digest references (`name@sha256:…`);
/// those are returned unchanged so a declared digest still matches itself.
fn canonicalize_image(reference: &str) -> String {
    // Digest references are already unambiguous — leave them alone.
    if reference.contains('@') {
        return reference.to_string();
    }

    // Split off the tag, if any. The tag delimiter is the *last* `:`, but only
    // when it's after the final `/` (otherwise it's a registry port like
    // `localhost:5000/foo`).
    let (name, tag) = match reference.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name.to_string(), tag.to_string()),
        _ => (reference.to_string(), "latest".to_string()),
    };

    // Does `name` start with a registry host? The OCI rule: if the first
    // path segment contains a `.` or `:`, or is exactly `localhost`, it's
    // treated as a registry host; otherwise it defaults to `docker.io`.
    let name = match name.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => {
            name
        }
        Some(_) => format!("docker.io/{name}"),
        None => format!("docker.io/library/{name}"),
    };

    format!("{name}:{tag}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ResourceSpec {
        image: String,
        command: Option<Vec<String>>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
        running: bool,
    }

    impl Default for ResourceSpec {
        fn default() -> Self {
            Self {
                image: "docker.io/library/nginx:latest".into(),
                command: None,
                volumes: vec![],
                restart_policy: Some("unless-stopped".into()),
                running: true,
            }
        }
    }

    fn resource(spec: ResourceSpec) -> PodmanResource {
        PodmanResource::Present {
            name: "web".into(),
            image: spec.image,
            command: spec.command,
            env: vec![],
            ports: vec!["8080:80".into()],
            volumes: spec.volumes,
            restart_policy: spec.restart_policy,
            running: spec.running,
        }
    }

    struct StateSpec {
        image: String,
        running: bool,
        command: Option<Vec<String>>,
        volumes: Vec<String>,
        restart_policy: Option<String>,
    }

    impl Default for StateSpec {
        fn default() -> Self {
            Self {
                image: "docker.io/library/nginx:latest".into(),
                running: true,
                command: None,
                volumes: vec![],
                restart_policy: Some("unless-stopped".into()),
            }
        }
    }

    fn state(spec: StateSpec) -> PodmanState {
        PodmanState::Present {
            image: spec.image,
            running: spec.running,
            command: spec.command,
            volumes: spec.volumes,
            restart_policy: spec.restart_policy,
        }
    }

    #[test]
    fn change_none_when_matches() {
        assert!(
            Podman::change(
                &resource(ResourceSpec::default()),
                &state(StateSpec::default())
            )
            .is_none()
        );
    }

    #[test]
    fn change_create_when_absent() {
        let change = Podman::change(&resource(ResourceSpec::default()), &PodmanState::Absent)
            .expect("change");
        assert!(matches!(change, PodmanChange::Create { start: true, .. }));
    }

    #[test]
    fn change_recreate_when_image_differs() {
        let current = state(StateSpec {
            image: "docker.io/library/nginx:1.25".into(),
            ..StateSpec::default()
        });
        let change = Podman::change(&resource(ResourceSpec::default()), &current).expect("change");
        assert!(matches!(change, PodmanChange::Recreate { .. }));
    }

    #[test]
    fn change_none_when_image_ref_matches_after_canonicalisation() {
        // Declared short form should match the fully-qualified inspect output.
        let declared = resource(ResourceSpec {
            image: "nginx:latest".into(),
            ..ResourceSpec::default()
        });
        assert!(Podman::change(&declared, &state(StateSpec::default())).is_none());
    }

    #[test]
    fn change_recreate_when_command_differs() {
        let declared = resource(ResourceSpec {
            command: Some(vec!["nginx".into(), "-g".into(), "daemon off;".into()]),
            ..ResourceSpec::default()
        });
        let current = state(StateSpec {
            command: Some(vec!["nginx".into()]),
            ..StateSpec::default()
        });
        let change = Podman::change(&declared, &current).expect("change");
        assert!(matches!(change, PodmanChange::Recreate { .. }));
    }

    #[test]
    fn change_recreate_when_restart_policy_differs() {
        let current = state(StateSpec {
            restart_policy: Some("always".into()),
            ..StateSpec::default()
        });
        let change = Podman::change(&resource(ResourceSpec::default()), &current).expect("change");
        assert!(matches!(change, PodmanChange::Recreate { .. }));
    }

    #[test]
    fn change_recreate_when_volumes_differ() {
        let declared = resource(ResourceSpec {
            volumes: vec!["/srv/data:/data".into()],
            ..ResourceSpec::default()
        });
        let change = Podman::change(&declared, &state(StateSpec::default())).expect("change");
        assert!(matches!(change, PodmanChange::Recreate { .. }));
    }

    #[test]
    fn change_none_when_undeclared_command_ignored() {
        // User didn't declare a command; inspect reports the image's default.
        // That's not drift — the declaration never constrained it.
        let current = state(StateSpec {
            command: Some(vec!["nginx".into(), "-g".into(), "daemon off;".into()]),
            ..StateSpec::default()
        });
        assert!(Podman::change(&resource(ResourceSpec::default()), &current).is_none());
    }

    #[test]
    fn change_none_when_undeclared_restart_policy_ignored() {
        // User didn't declare a restart policy; a container-side default isn't drift.
        let declared = resource(ResourceSpec {
            restart_policy: None,
            ..ResourceSpec::default()
        });
        let current = state(StateSpec {
            restart_policy: Some("always".into()),
            ..StateSpec::default()
        });
        assert!(Podman::change(&declared, &current).is_none());
    }

    #[test]
    fn change_start_when_only_running_differs() {
        let current = state(StateSpec {
            running: false,
            ..StateSpec::default()
        });
        let change = Podman::change(&resource(ResourceSpec::default()), &current).expect("change");
        assert!(matches!(change, PodmanChange::Start { .. }));
    }

    #[test]
    fn change_stop_when_declared_not_running() {
        let declared = resource(ResourceSpec {
            running: false,
            ..ResourceSpec::default()
        });
        let change = Podman::change(&declared, &state(StateSpec::default())).expect("change");
        assert!(matches!(change, PodmanChange::Stop { .. }));
    }

    #[test]
    fn change_remove_when_declared_absent_but_present() {
        let declared = PodmanResource::Absent { name: "web".into() };
        let change = Podman::change(&declared, &state(StateSpec::default())).expect("change");
        assert!(matches!(change, PodmanChange::Remove { .. }));
    }

    #[test]
    fn change_none_when_absent_matches() {
        let declared = PodmanResource::Absent { name: "web".into() };
        assert!(Podman::change(&declared, &PodmanState::Absent).is_none());
    }

    #[test]
    fn canonicalize_bare_image_adds_docker_hub_and_latest() {
        assert_eq!(
            canonicalize_image("nginx"),
            "docker.io/library/nginx:latest"
        );
    }

    #[test]
    fn canonicalize_tagged_bare_image_adds_docker_hub() {
        assert_eq!(
            canonicalize_image("nginx:1.25"),
            "docker.io/library/nginx:1.25"
        );
    }

    #[test]
    fn canonicalize_user_repo_adds_docker_hub() {
        assert_eq!(
            canonicalize_image("bitnami/redis"),
            "docker.io/bitnami/redis:latest"
        );
    }

    #[test]
    fn canonicalize_fully_qualified_passthrough() {
        assert_eq!(
            canonicalize_image("ghcr.io/foo/bar:v1"),
            "ghcr.io/foo/bar:v1"
        );
    }

    #[test]
    fn canonicalize_localhost_registry_preserved() {
        assert_eq!(
            canonicalize_image("localhost:5000/app:dev"),
            "localhost:5000/app:dev"
        );
    }

    #[test]
    fn canonicalize_digest_reference_unchanged() {
        let digest = "docker.io/library/nginx@sha256:deadbeef";
        assert_eq!(canonicalize_image(digest), digest);
    }
}
