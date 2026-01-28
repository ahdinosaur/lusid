use displaydoc::Display;
use lusid_params::{ParamValues, ParamValuesFromRimuError, ParamsStruct};
use rimu::{call, Spanned, Value};
use rimu_interop::FromRimu;
use thiserror::Error;

use crate::model::{IntoPlanItemError, PlanItem, SetupFunction};

#[derive(Debug, Error, Display)]
pub enum EvalError {
    /// Converting params from rimu value failed: {0}
    Params(Box<Spanned<ParamValuesFromRimuError>>),
    /// Calling setup function failed: {0}
    RimuCall(#[from] Box<rimu::EvalError>),
    /// Setup returned a non-list value
    ReturnedNotList,
    /// Invalid PlanItem value: {0}
    InvalidPlanItem(Box<Spanned<IntoPlanItemError>>),
}

pub(crate) fn evaluate(
    setup: Spanned<SetupFunction>,
    params_value: Option<Spanned<Value>>,
    params_struct: Option<ParamsStruct>,
) -> Result<Vec<Spanned<PlanItem>>, EvalError> {
    let (setup, setup_span) = setup.take();

    let args = match params_value {
        None => vec![],
        Some(params_value) => {
            let params_struct =
                params_struct.expect("params struct should exist if params value exists");
            let param_values = ParamValues::from_rimu_spanned(params_value, params_struct)
                .map_err(|error| EvalError::Params(Box::new(error)))?;
            let value = ParamValues::into_rimu_spanned(param_values);
            vec![value]
        }
    };

    let result = call(setup_span, setup.0, &args).map_err(Box::new)?;
    let (result, _result_span) = result.take();

    let Value::List(items) = result else {
        return Err(EvalError::ReturnedNotList);
    };

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let call = PlanItem::from_rimu_spanned(item)
            .map_err(|error| EvalError::InvalidPlanItem(Box::new(error)))?;
        out.push(call)
    }
    Ok(out)
}
