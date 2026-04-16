//! Convenience constructors for [`Machine`] specs that match the images in
//! `vm/images.toml`. Tests can hand-roll `Machine` if they need something
//! exotic; these are just the common cases.

use lusid_machine::{Machine, MachineVmOptions};
use lusid_system::{Arch, Hostname, Linux, Os};

/// Debian 13 (trixie) on x86-64. Matches `debian-13-x86-64` in `vm/images.toml`.
pub fn debian_13(hostname: &str) -> Machine {
    Machine {
        hostname: Hostname::from(hostname.to_owned()),
        arch: Arch::X86_64,
        os: Os::Linux(Linux::Debian { version: 13 }),
        vm: Some(MachineVmOptions::default()),
    }
}

/// Arch Linux on x86-64. Matches `arch-x86-64` in `vm/images.toml`.
pub fn arch(hostname: &str) -> Machine {
    Machine {
        hostname: Hostname::from(hostname.to_owned()),
        arch: Arch::X86_64,
        os: Os::Linux(Linux::Arch),
        vm: Some(MachineVmOptions::default()),
    }
}
