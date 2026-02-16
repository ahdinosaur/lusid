//! Load Rimu source into a Plan (spanned).

use std::{cell::RefCell, rc::Rc};

use displaydoc::Display;
use rimu::Spanned;
use rimu_interop::FromRimu;
use thiserror::Error;

use crate::{
    PlanId,
    model::{Plan, PlanFromRimuError},
};

#[derive(Debug, Error, Display)]
pub enum LoadError {
    /// Rimu parse failed: {0:?}
    RimuParse(Vec<rimu::ParseError>),

    /// No code found in source
    NoCode,

    /// Evaluating Rimu AST failed
    RimuEval(#[from] Box<rimu::EvalError>),

    /// Failed to convert Rimu value into Plan
    PlanFromRimu(Box<Spanned<PlanFromRimuError>>),
}

pub fn load(code: &str, plan_id: &PlanId) -> Result<Spanned<Plan>, LoadError> {
    let source_id = plan_id.clone().into();
    let (ast, errors) = rimu::parse(code, source_id);
    if !errors.is_empty() {
        return Err(LoadError::RimuParse(errors));
    }
    let Some(ast) = ast else {
        return Err(LoadError::NoCode);
    };

    let env = Rc::new(RefCell::new(rimu::Environment::new()));
    let value = rimu::evaluate(&ast, env).map_err(Box::new)?;
    let plan =
        Plan::from_rimu_spanned(value).map_err(|error| LoadError::PlanFromRimu(Box::new(error)))?;
    Ok(plan)
}
