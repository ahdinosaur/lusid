//! Planning: turn a `.lusid` plan (written in Rimu) into a tree of typed resource params.
//!
//! The entry point is [`plan`]. Given a root [`PlanId`] (local path, eventually also git),
//! optional Rimu params, and a reference to the current [`System`], it:
//!
//! 1. Reads the plan source from the [`Store`].
//! 2. Parses + evaluates Rimu into a [`Plan`] (via [`load::load`]).
//! 3. Validates user params against the plan's `params` schema.
//! 4. Invokes the plan's `setup(params, system)` function to get a list of `PlanItem`s.
//! 5. For each item, either:
//!    - If `module` starts with `@core/<id>` → convert to [`ResourceParams`] (a leaf).
//!    - Otherwise → resolve the module as a sibling `.lusid` file, recurse, and attach
//!      as a subtree (a branch).
//!
//! The result is a [`PlanTree<ResourceParams>`] whose branch/leaf metadata carries the
//! [`PlanNodeId`] identifiers used by causality scheduling downstream.

use displaydoc::Display;
use lusid_params::{ParamValuesFromRimuError, ParamsValidationError, validate};
use lusid_resource::ResourceParams;
use lusid_store::{Store, StoreError, StoreItemId};
use lusid_system::System;
use rimu::{Spanned, Value};
use std::{path::PathBuf, string::FromUtf8Error};
use thiserror::Error;

mod core;
mod eval;
mod id;
mod load;
mod model;
mod tree;

pub use crate::id::{PlanId, PlanNodeId};
pub use crate::tree::*;
use crate::{
    core::{core_module, is_core_module},
    eval::{EvalError, evaluate},
    load::{LoadError, load},
    model::Plan,
};

#[derive(Debug, Error, Display)]
pub enum PlanError {
    /// Failed to read plan source from store for id {id:?}: {source}
    StoreRead {
        id: StoreItemId,
        #[source]
        source: StoreError,
    },

    /// Failed to decode plan source as UTF-8: {0}
    InvalidUtf8(#[from] FromUtf8Error),

    /// Failed to load plan source: {0}
    Load(#[from] LoadError),

    /// Parameter validation failed: {0}
    Validate(#[from] ParamsValidationError),

    /// Failed to evaluate plan setup: {0}
    Eval(#[from] EvalError),

    /// Failed to convert plan item to resource: {0}
    PlanItemToResource(#[from] PlanItemToResourceError),
}

/// Plan a `.lusid` file recursively, producing a tree of typed resource params.
///
/// Wraps the recursive subplan in a root [`PlanTree::Branch`] with default metadata so
/// callers always get a tree (never a bare list).
#[tracing::instrument(skip_all)]
pub async fn plan(
    plan_id: PlanId,
    params_value: Option<Spanned<Value>>,
    store: &mut Store,
    system: &System,
) -> Result<PlanTree<ResourceParams>, PlanError> {
    tracing::debug!("Plan {plan_id:?} with params {params_value:?}");
    let children = plan_recursive(plan_id, params_value.as_ref(), store, system).await?;
    let tree = PlanTree::Branch {
        children,
        meta: PlanMeta::default(),
    };
    tracing::trace!("Planned resource tree: {:?}", tree);
    Ok(tree)
}

/// Inner recursive routine. Each call handles exactly one `.lusid` source: load, validate
/// params, evaluate `setup`, and convert each returned item into a subtree.
async fn plan_recursive(
    plan_id: PlanId,
    params_value: Option<&Spanned<Value>>,
    store: &mut Store,
    system: &System,
) -> Result<Vec<PlanTree<ResourceParams>>, PlanError> {
    let store_item_id: StoreItemId = plan_id.clone().into();
    let bytes = store
        .read(&store_item_id)
        .await
        .map_err(|source| PlanError::StoreRead {
            id: store_item_id.clone(),
            source,
        })?;
    let code = String::from_utf8(bytes)?;
    let plan = load(&code, &plan_id)?;

    let Plan {
        name: _,
        version: _,
        params: param_types,
        setup,
    } = plan.into_inner();

    let params_struct = validate(param_types.as_ref(), params_value)?;

    let plan_items = evaluate(setup, params_value.cloned(), params_struct, system)?;

    let mut resources = Vec::with_capacity(plan_items.len());
    for plan_item in plan_items {
        let node = Box::pin(plan_item_to_resource(plan_item, &plan_id, store, system)).await?;
        resources.push(node);
    }

    Ok(resources)
}

#[derive(Debug, Error, Display)]
pub enum PlanItemToResourceError {
    /// Missing required parameters in plan item
    MissingParams,

    /// Parameters validation for resource failed: {0}
    ParamsValidation(#[from] ParamsValidationError),

    /// Parameters value from rimu value for resource failed: {0}
    ParamsValueFromRimu(Spanned<ParamValuesFromRimuError>),

    /// Failed to convert parameter values to resource params: {0}
    SerdeValue(#[from] rimu::SerdeValueError),

    /// Unsupported core module id \"{id}\"
    UnsupportedCoreModuleId { id: String },

    /// Failed to compute subtree for nested plan: {0}
    PlanSubtree(#[from] Box<PlanError>),
}

/// Lower a single `PlanItem` to a subtree. Core modules produce a leaf with
/// [`ResourceParams`]; every other module name is treated as a path relative to the
/// parent plan and recursed into as a branch.
async fn plan_item_to_resource(
    plan_item: Spanned<crate::model::PlanItem>,
    current_plan_id: &PlanId,
    store: &mut Store,
    system: &System,
) -> Result<PlanTree<ResourceParams>, PlanItemToResourceError> {
    let (plan_item, _span) = plan_item.take();
    let crate::model::PlanItem {
        id: item_id,
        ref module,
        params: params_value,
        requires,
        required_by,
    } = plan_item;

    let id = item_id.map(|id| PlanNodeId::PlanItem {
        plan_id: current_plan_id.clone(),
        item_id: id.into_inner(),
    });
    let requires = requires
        .into_iter()
        .map(|v| v.into_inner())
        .map(|item_id| PlanNodeId::PlanItem {
            plan_id: current_plan_id.clone(),
            item_id,
        })
        .collect();
    let required_by = required_by
        .into_iter()
        .map(|v| v.into_inner())
        .map(|item_id| PlanNodeId::PlanItem {
            plan_id: current_plan_id.clone(),
            item_id,
        })
        .collect();

    if let Some(core_module_id) = is_core_module(module) {
        let params = core_module(core_module_id, params_value)?;
        Ok(PlanTree::Leaf {
            meta: PlanMeta {
                id,
                requires,
                required_by,
            },
            node: params,
        })
    } else {
        let path = PathBuf::from(module.inner());
        let plan_id = current_plan_id.join(path);
        let children = plan_recursive(plan_id, params_value.as_ref(), store, system)
            .await
            .map_err(Box::new)?;
        Ok(PlanTree::Branch {
            meta: PlanMeta {
                id,
                requires,
                required_by,
            },
            children,
        })
    }
}
