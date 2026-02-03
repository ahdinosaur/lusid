use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub home: PathBuf,
}

#[derive(Error, Debug)]
pub enum GetUserError {
    #[error("missing user")]
    MissingUser,

    #[error("missing home")]
    MissingHome,
}

impl User {
    pub fn get() -> Result<Self, GetUserError> {
        let name = get_user().ok_or(GetUserError::MissingUser)?;
        let home = get_home().ok_or(GetUserError::MissingHome)?;

        Ok(Self { name, home })
    }
}

fn get_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        env::var_os("HOME").map(PathBuf::from)
    }

    #[cfg(windows)]
    {
        env::var_os("USERPROFILE").or_else(|| {
            let home_drive = env::var_os("HOMEDRIVE")?;
            let home_path = env::var_os("HOMEPATH")?;
            Some({
                let mut path = PathBuf::from(home_drive);
                path.push(home_path);
                path
            })
        })
    }
}

fn get_user() -> Option<String> {
    #[cfg(unix)]
    {
        env::var("USER").ok()
    }

    #[cfg(windows)]
    {
        env::var("USERNAME").ok()
    }
}
