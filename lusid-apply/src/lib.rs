//! Pipeline orchestrator: loads a plan, validates params, builds the resource
//! → state → change → operation trees, schedules operations by epoch, and
//! applies them — all while streaming [`AppUpdate`]s as newline-delimited
//! JSON on stdout for the `lusid` TUI to render.
//!
//! The public surface is [`apply`] + [`ApplyOptions`]; `main.rs` is a thin
//! clap wrapper.
//!
//! ## Pipeline (one phase per [`AppUpdate`] group)
//!
//! 1. [`plan`](lusid_plan::plan) — evaluate the plan, validate params,
//!    produce a [`PlanTree<ResourceParams>`](lusid_plan::PlanTree).
//! 2. `ResourceParams → Resources` via `ResourceParams::resources` — each
//!    plan node can expand into multiple resources with intra-scope ordering
//!    (file mode/user/group, etc.), handled by
//!    [`map_plan_subitems`](lusid_plan::map_plan_subitems).
//! 3. `Resource → ResourceState` via async state probes. This is the only
//!    I/O-bound phase prior to apply; emits per-leaf `NodeStart`/`NodeComplete`
//!    so the TUI can show a spinner while each probe runs.
//! 4. `(Resource, State) → ResourceChange` — pure; `None` means "no-op, prune".
//! 5. `ResourceChange → Operations` tree — each change expands to one or
//!    more ordered operations. Short-circuits if step 4 produced no changes.
//! 6. [`compute_epochs`] — Kahn's topological layering over the causality
//!    metadata in the operations tree; operations within an epoch are
//!    independent, operations across epochs have a required-before edge.
//! 7. [`Operation::merge`] + [`Operation::apply`] — per-epoch, merge like
//!    operations (e.g. multiple `apt install`s into one), then apply
//!    sequentially. Stdout + stderr are streamed line-by-line back into
//!    `AppUpdate` events.
//!
//! Human-facing output belongs on stderr (via `tracing`); stdout is reserved
//! for the machine-readable protocol.

use std::path::PathBuf;

use lusid_apply_stdio::AppUpdate;
use lusid_causality::{CausalityTree, EpochError, compute_epochs};
use lusid_ctx::{Context, ContextError};
use lusid_operation::{Operation, OperationApplyError};
use lusid_plan::{
    self, PlanError, PlanId, PlanNodeId, PlanTree, map_plan_subitems, plan, render_plan_tree,
};
use lusid_resource::{Resource, ResourceState, ResourceStateError};
use lusid_secrets::{
    DecryptDirError, Identity, IdentityError, Recipients, RecipientsError, alias_for_identity,
    decrypt_dir,
};
use lusid_store::Store;
use lusid_system::{GetSystemError, System};
use lusid_tree::FlatTree;
use lusid_view::Render;
use rimu::SourceId;
use rimu_interop::{ToRimuError, to_rimu};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info};

/// Inputs for [`apply`]. `root_path` is the lusid working-dir root passed to
/// [`Context::create`]; `plan_id` selects a plan; `params_json` is an
/// optional JSON object (validated against the plan's params schema).
///
/// Secrets: if `identity_path` is `Some`, `lusid-apply` loads that identity,
/// reads `lusid-secrets.toml` from `secrets_dir` (defaulting to
/// `<root>/secrets`), matches the identity to an alias, and decrypts the
/// subset of `*.age` files declared for that alias. `None` skips secrets
/// entirely (plans that reference `@core/secret` will fail at apply with a
/// missing-secret error).
pub struct ApplyOptions {
    pub root_path: PathBuf,
    pub plan_id: PlanId,
    pub params_json: Option<String>,
    pub identity_path: Option<PathBuf>,
    pub secrets_dir: Option<PathBuf>,
}

#[derive(Error, Debug)]
pub enum ApplyError {
    #[error(transparent)]
    Context(#[from] ContextError),

    #[error("failed to get system: {0}")]
    GetSystem(#[from] GetSystemError),

    #[error("failed to parse JSON parameters: {0}")]
    JsonParameters(#[source] serde_json::Error),

    #[error("failed to parse parameters into rimu value: {0}")]
    RimuParameters(#[from] ToRimuError),

    #[error("failed to output JSON: {0}")]
    JsonOutput(#[source] serde_json::Error),

    #[error("failed to read operation stdio: {0}")]
    ReadOperationStdio(#[source] tokio::io::Error),

    #[error("failed to write to stdout: {0}")]
    WriteStdout(#[source] tokio::io::Error),

    #[error("failed to flush stdout: {0}")]
    FlushStdout(#[source] tokio::io::Error),

    #[error(transparent)]
    Plan(#[from] PlanError),

    #[error(transparent)]
    Epoch(#[from] EpochError<PlanNodeId>),

    #[error(transparent)]
    ResourceState(#[from] ResourceStateError),

    #[error(transparent)]
    OperationApply(#[from] OperationApplyError),

    #[error(transparent)]
    Identity(#[from] IdentityError),

    #[error(transparent)]
    Recipients(#[from] RecipientsError),

    #[error(transparent)]
    DecryptDir(#[from] DecryptDirError),

    /// The identity's public key does not match any alias in
    /// `[operators]` or `[machines]` of `lusid-secrets.toml`. Without a
    /// known alias we cannot pick which files to decrypt for this host.
    #[error("supplied identity matches no alias in lusid-secrets.toml")]
    NoAliasForIdentity,
}

/// Run the full apply pipeline, streaming [`AppUpdate`]s to stdout as it
/// goes. Returns `Ok(())` on success (including the "no changes" early
/// return after phase 4) or the first fatal error. On operation failure,
/// an `OperationApplyComplete { error: Some(..) }` is emitted before the
/// error propagates so the TUI can show which operation failed.
pub async fn apply(options: ApplyOptions) -> Result<(), ApplyError> {
    info!("starting");
    let ApplyOptions {
        root_path,
        plan_id,
        params_json,
        identity_path,
        secrets_dir,
    } = options;

    let mut ctx = Context::create(&root_path)?;
    let mut store = Store::new(ctx.paths().cache_dir());
    let system = System::get().await?;

    // Resolve secrets_dir to <root>/secrets by default. Only consulted when
    // an identity is supplied — without one, there's no key to decrypt with
    // so the directory's existence is irrelevant.
    let secrets_dir = secrets_dir.unwrap_or_else(|| root_path.join("secrets"));
    if let Some(identity_path) = identity_path.as_deref() {
        info!(
            identity = %identity_path.display(),
            secrets_dir = %secrets_dir.display(),
            "loading secrets",
        );
        let identity = Identity::from_file(identity_path).await?;
        let recipients = Recipients::load(&secrets_dir).await?;
        let alias =
            alias_for_identity(&identity, &recipients).ok_or(ApplyError::NoAliasForIdentity)?;
        let stems = recipients.files_for_alias(alias);
        debug!(alias, count = stems.len(), "alias matched");
        let secrets = decrypt_dir(&identity, &secrets_dir, &stems).await?;
        info!(count = secrets.len(), "secrets loaded");
        ctx.set_secrets(secrets);
    } else {
        debug!("no identity supplied; proceeding without secrets");
    }

    info!(plan = %plan_id, "using plan");

    let param_values = match params_json {
        None => {
            info!("no parameters provided");
            None
        }
        Some(json) => {
            let value: serde_json::Value =
                serde_json::from_str(&json).map_err(ApplyError::JsonParameters)?;
            let value = to_rimu(value, SourceId::empty())?;
            Some(value)
        }
    };

    // Parse/evaluate to tree of resource params.
    let resource_params = plan(plan_id, param_values, &mut store, &system).await?;
    debug!("Resource params: {resource_params:?}");
    emit(AppUpdate::ResourceParams {
        resource_params: render_plan_tree(resource_params.clone()),
    })
    .await?;
    let resource_params = FlatTree::from(resource_params);

    // Get tree of atomic resources.
    emit(AppUpdate::ResourcesStart).await?;
    let resources = resource_params
        .map_tree(
            |node, meta| PlanTree::branch(meta, map_plan_subitems(node, |node| node.resources())),
            |index, tree| {
                emit(AppUpdate::ResourcesNode {
                    index,
                    tree: render_plan_tree(tree),
                })
            },
        )
        .await?;
    debug!("Resources: {:?}", CausalityTree::from(resources.clone()));
    emit(AppUpdate::ResourcesComplete).await?;

    // Get tree of (resource, resource state)
    emit(AppUpdate::ResourceStatesStart).await?;
    let resource_states = resources
        .map_result_async(
            |resource| {
                let mut ctx = ctx.clone();
                async move {
                    let state = resource.state(&mut ctx).await?;
                    Ok::<(Resource, ResourceState), ApplyError>((resource, state))
                }
            },
            |index| emit(AppUpdate::ResourceStatesNodeStart { index }),
            |index, (_resource, resource_state)| {
                emit(AppUpdate::ResourceStatesNodeComplete {
                    index,
                    node: resource_state.render(),
                })
            },
        )
        .await?;
    debug!(
        "Resource states: {:?}",
        CausalityTree::from(resource_states.clone()).map(|(_resource, state)| state)
    );
    emit(AppUpdate::ResourceStatesComplete).await?;

    // Get tree of resource changes
    emit(AppUpdate::ResourceChangesStart).await?;
    let resource_changes = resource_states
        .map(
            |(resource, state)| resource.change(&state),
            |index, node| {
                emit(AppUpdate::ResourceChangesNode {
                    index,
                    node: node.map(|n| n.render()),
                })
            },
        )
        .await?;
    debug!(
        "Resource changes: {:?}",
        CausalityTree::from(resource_changes.clone())
    );

    let has_changes = resource_changes.leaves().any(|node| node.is_some());

    emit(AppUpdate::ResourceChangesComplete { has_changes }).await?;

    if !has_changes {
        info!("No changes to apply!");
        return Ok(());
    };

    // Get CausalityTree<Operations>
    emit(AppUpdate::OperationsStart).await?;
    let operations = resource_changes
        .map_tree(
            |node, meta| match node {
                Some(node) => {
                    let children = map_plan_subitems(node, |node| node.operations())
                        .map(|tree| tree.map(Some));
                    PlanTree::branch(meta, children)
                }
                None => PlanTree::leaf(meta, None),
            },
            |index, tree| {
                emit(AppUpdate::OperationsNode {
                    index,
                    operations: render_plan_tree(tree),
                })
            },
        )
        .await?;
    debug!(
        "Operations tree: {:?}",
        CausalityTree::from(operations.clone())
    );
    emit(AppUpdate::OperationsComplete).await?;

    let operation_epochs = compute_epochs(CausalityTree::from(operations))?;
    debug!("Operation epochs: {operation_epochs:?}");
    emit(AppUpdate::OperationsApplyStart {
        operations: operation_epochs
            .iter()
            .map(|epoch| epoch.iter().map(Render::render).collect())
            .collect(),
    })
    .await?;

    let epochs_count = operation_epochs.len();
    for (epoch_index, operations) in operation_epochs.into_iter().enumerate() {
        info!(
            epoch = epoch_index,
            count = epochs_count,
            "processing epoch"
        );
        debug!("Operations: {operations:?}");

        let operations = Operation::merge(operations);
        debug!("Merged operations: {operations:?}");

        for (operation_index, operation) in operations.iter().enumerate() {
            let index = (epoch_index, operation_index);

            let (output, stdout, stderr) = operation.apply(&mut ctx).await?;

            let output_task = async {
                output.await?;

                Ok::<(), ApplyError>(())
            };

            let stdout_task = {
                let mut lines = BufReader::new(stdout).lines();
                async move {
                    while let Some(line) = lines
                        .next_line()
                        .await
                        .map_err(ApplyError::ReadOperationStdio)?
                    {
                        emit(AppUpdate::OperationApplyStdout {
                            index,
                            stdout: line,
                        })
                        .await?;
                    }
                    Ok::<(), ApplyError>(())
                }
            };

            let stderr_task = {
                let mut lines = BufReader::new(stderr).lines();
                async move {
                    while let Some(line) = lines
                        .next_line()
                        .await
                        .map_err(ApplyError::ReadOperationStdio)?
                    {
                        emit(AppUpdate::OperationApplyStderr {
                            index,
                            stderr: line,
                        })
                        .await?;
                    }
                    Ok::<(), ApplyError>(())
                }
            };

            if let Err(error) = tokio::try_join!(output_task, stdout_task, stderr_task) {
                emit(AppUpdate::OperationApplyComplete {
                    index,
                    error: Some(error.to_string()),
                })
                .await?;
                return Err(error);
            } else {
                emit(AppUpdate::OperationApplyComplete { index, error: None }).await?;
            }
        }
    }

    info!("Apply completed");
    Ok(())
}

/// Serialize `update` to a single JSON line on stdout and flush.
///
/// The flush is load-bearing: the TUI reads line-by-line with
/// `AsyncBufRead::lines()`, so buffering would make progress updates
/// invisible to the reader even though the work completed long before.
async fn emit(update: AppUpdate) -> Result<(), ApplyError> {
    let mut stdout = tokio::io::stdout();

    stdout
        .write_all(&serde_json::to_vec(&update).map_err(ApplyError::JsonOutput)?)
        .await
        .map_err(ApplyError::WriteStdout)?;

    stdout
        .write_all(b"\n")
        .await
        .map_err(ApplyError::WriteStdout)?;

    stdout.flush().await.map_err(ApplyError::FlushStdout)?;

    Ok(())
}
