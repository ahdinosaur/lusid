//! "Core modules" are the built-in resource types exposed to plans under the
//! `@core/<id>` namespace (e.g. `@core/apt`, `@core/file`). This module routes a plan
//! item's module string to the matching [`ResourceType`] impl.

use lusid_params::{ParamValues, validate};
use lusid_resource::{
    ResourceParams, ResourceType, apt::Apt, apt_repo::AptRepo, command::Command, file::File,
    directory::Directory, git::Git, group::Group, pacman::Pacman, systemd::Systemd, user::User,
};
use rimu::{Spanned, Value};

use crate::PlanItemToResourceError;

/// Returns the core id (e.g. `"apt"`) if `module` uses the `@core/<id>` prefix,
/// otherwise `None` — meaning the module should be resolved as a nested plan.
pub fn is_core_module(module: &Spanned<String>) -> Option<&str> {
    module.inner().strip_prefix("@core/")
}

/// Validate & deserialise `params` against the matching core module's schema, returning
/// the wrapped [`ResourceParams`] variant. Errors if `id` is unknown.
pub fn core_module(
    core_module_id: &str,
    params: Option<Spanned<Value>>,
) -> Result<ResourceParams, PlanItemToResourceError> {
    match core_module_id {
        Apt::ID => core_module_for_resource::<Apt>(params).map(ResourceParams::Apt),
        AptRepo::ID => core_module_for_resource::<AptRepo>(params).map(ResourceParams::AptRepo),
        File::ID => core_module_for_resource::<File>(params).map(ResourceParams::File),
        Directory::ID => {
            core_module_for_resource::<Directory>(params).map(ResourceParams::Directory)
        }
        Pacman::ID => core_module_for_resource::<Pacman>(params).map(ResourceParams::Pacman),
        Command::ID => core_module_for_resource::<Command>(params).map(ResourceParams::Command),
        Git::ID => core_module_for_resource::<Git>(params).map(ResourceParams::Git),
        Systemd::ID => core_module_for_resource::<Systemd>(params).map(ResourceParams::Systemd),
        User::ID => core_module_for_resource::<User>(params).map(ResourceParams::User),
        Group::ID => core_module_for_resource::<Group>(params).map(ResourceParams::Group),
        other => Err(PlanItemToResourceError::UnsupportedCoreModuleId {
            id: other.to_string(),
        }),
    }
}

fn core_module_for_resource<R: ResourceType>(
    params_value: Option<Spanned<Value>>,
) -> Result<R::Params, PlanItemToResourceError> {
    let params_value = params_value.ok_or(PlanItemToResourceError::MissingParams)?;
    let param_types = R::param_types();

    let params_struct = validate(param_types.as_ref(), Some(&params_value))?;
    let params_struct = params_struct.expect("params struct should exist for core module");

    let param_values = ParamValues::from_rimu_spanned(params_value, params_struct)
        .map_err(PlanItemToResourceError::ParamsValueFromRimu)?;

    let params: R::Params = param_values.into_inner().into_type()?;
    Ok(params)
}
