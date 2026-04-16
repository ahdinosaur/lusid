//! Declarative description of a lusid *target* machine — distinct from [`lusid_system::System`],
//! which describes the machine lusid is currently running on.
//!
//! A `Machine` names the intended hostname/arch/OS (and, if it should be materialized as
//! a VM, [`MachineVmOptions`] covering cpu/memory/graphics). Wired into the `vm` crate as
//! the input to `Instance::start`.
//
// Note(cc): this crate is deliberately small. As the product picks up remote deployment,
// credentials, or lifecycle policies, those fields land here.

use lusid_system::{Arch, CpuCount, Hostname, MemorySize, Os};
use serde::{Deserialize, Serialize};

/// Declarative spec of a machine we want to provision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Machine {
    pub hostname: Hostname,
    pub arch: Arch,
    pub os: Os,
    pub vm: Option<MachineVmOptions>,
}

/// VM-specific knobs when `Machine::vm` is `Some`. All fields are optional so
/// defaults can apply per-backend.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineVmOptions {
    pub memory_size: Option<MemorySize>,
    pub cpu_count: Option<CpuCount>,
    pub graphics: Option<bool>,
}
