//! [`Hostname`] newtype wrapping a `String`.
//
// Note(cc): this module lives in `id.rs` because the broader plan is to grow other
// machine identifiers (machine-id, serial numbers) alongside hostname.

use std::{
    convert::Infallible,
    ffi::OsString,
    fmt::{self, Display},
    io,
    str::FromStr,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hostname(String);

impl Display for Hostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Hostname {
    pub fn get() -> io::Result<Self> {
        hostname::get().map(Into::into)
    }
}

impl From<String> for Hostname {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<OsString> for Hostname {
    fn from(value: OsString) -> Self {
        Self(value.to_string_lossy().into_owned())
    }
}

impl FromStr for Hostname {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

impl AsRef<str> for Hostname {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
