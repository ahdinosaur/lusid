//! CPU architecture detection via Rust's `cfg(target_arch)`.
//!
//! The [`Bitness`] type is currently only `64-bit`; kept around so future 32-bit or
//! other width categorizations slot in without changing consumers.
// Note(cc): `Bitness` is defined here but not referenced anywhere else in the
// workspace. Delete if no use materializes soon.

use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Arch {
    #[serde(rename = "x86-64")]
    X86_64,
    #[serde(rename = "aarch64")]
    Aarch64,
}

impl Arch {
    #[cfg(target_arch = "x86_64")]
    pub fn get() -> Self {
        Arch::X86_64
    }

    #[cfg(target_arch = "aarch64")]
    pub fn get() -> Self {
        Arch::Aarch64
    }
}

impl Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arch::X86_64 => write!(f, "x86-64"),
            Arch::Aarch64 => write!(f, "aarch64"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Bitness {
    #[serde(rename = "64-bit")]
    X64,
}

impl Display for Bitness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bitness::X64 => write!(f, "64-bit"),
        }
    }
}

impl From<Arch> for Bitness {
    fn from(value: Arch) -> Self {
        use Bitness::*;
        match value {
            Arch::X86_64 => X64,
            Arch::Aarch64 => X64,
        }
    }
}
