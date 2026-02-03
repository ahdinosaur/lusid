mod arch;
mod hardware;
mod id;
mod os;
mod user;

use std::io;

use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

pub use crate::arch::*;
pub use crate::hardware::*;
pub use crate::id::*;
pub use crate::os::*;
pub use crate::user::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct System {
    pub hostname: Hostname,
    pub arch: Arch,
    pub os: Os,
    pub user: User,
}

#[derive(Error, Debug)]
pub enum GetSystemError {
    #[error("failed to get hostname: {0}")]
    Hostname(#[source] io::Error),

    #[error("failed to get os: {0}")]
    Os(#[from] GetOsError),

    #[error("failed to get user: {0}")]
    User(#[from] GetUserError),
}

impl System {
    pub async fn get() -> Result<Self, GetSystemError> {
        let hostname = Hostname::get().map_err(GetSystemError::Hostname)?;
        let arch = Arch::get();
        let os = Os::get().await?;
        let user = User::get()?;

        Ok(Self {
            hostname,
            arch,
            os,
            user,
        })
    }
}
