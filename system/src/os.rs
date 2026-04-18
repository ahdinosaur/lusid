//! OS detection. On Linux we parse `/etc/os-release` via the `etc-os-release` crate
//! and map the `ID` to a known distro variant (Ubuntu / Debian / Arch for now). On
//! macOS we shell out to `sw_vers -productVersion` for the product version string.
//!
//! The serde shape uses nested internal tags: the outer `type: "linux"` discriminates
//! [`Os`], and the inner `linux: "ubuntu"` discriminates [`Linux`]. Version fields
//! are named after the distro (`ubuntu: "22.04"`, `debian: 12`, `macos: "15.3.1"`)
//! so the plan-facing YAML reads naturally.

use etc_os_release::{Error as OsReleaseError, OsRelease};
use serde::{Deserialize, Serialize, de};
use std::{
    fmt::{self, Display, Formatter},
    num::ParseIntError,
    process::ExitStatus,
    str::FromStr,
};
use thiserror::Error;
use tokio::task::block_in_place;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Os {
    #[serde(rename = "linux")]
    Linux(Linux),

    #[serde(rename = "macos")]
    MacOS {
        #[serde(rename = "macos")]
        version: String,
    },
}

#[derive(Error, Debug)]
pub enum GetOsError {
    #[error("failed to get OS on Linux: {0}")]
    Linux(#[from] GetLinuxError),

    #[error("failed to get OS on macOS: {0}")]
    MacOs(#[from] GetMacOsError),
}

impl Os {
    #[cfg(target_os = "linux")]
    pub async fn get() -> Result<Self, GetOsError> {
        Ok(Os::Linux(Linux::get().await?))
    }

    #[cfg(target_os = "macos")]
    pub async fn get() -> Result<Self, GetOsError> {
        let version = get_macos_version().await?;
        Ok(Os::MacOS { version })
    }

    /// Strip version/distro detail and return just the OS family.
    ///
    /// Useful when choosing an apply-binary or any other artifact that varies
    /// by OS family but not by minor release — `Os` carries distro/version
    /// information that most resource-selection logic doesn't care about.
    pub fn kind(&self) -> OsKind {
        match self {
            Os::Linux(_) => OsKind::Linux,
            Os::MacOS { .. } => OsKind::MacOS,
        }
    }
}

/// OS-family key: the discriminant of [`Os`] without the embedded distro/version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OsKind {
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "macos")]
    MacOS,
}

impl Display for OsKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            OsKind::Linux => write!(f, "linux"),
            OsKind::MacOS => write!(f, "macos"),
        }
    }
}

#[derive(Error, Debug)]
pub enum GetMacOsError {
    #[error("failed to execute sw_vers: {0}")]
    SwVersSpawn(#[source] std::io::Error),

    #[error("sw_vers exited with non-zero status: {status}")]
    SwVersStatus { status: ExitStatus },

    #[error("sw_vers returned empty version string")]
    EmptyVersion,
}

#[cfg(target_os = "macos")]
async fn get_macos_version() -> Result<String, GetMacOsError> {
    let output = tokio::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .await
        .map_err(GetMacOsError::SwVersSpawn)?;
    if !output.status.success() {
        return Err(GetMacOsError::SwVersStatus {
            status: output.status,
        });
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        return Err(GetMacOsError::EmptyVersion);
    }
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "linux")]
#[non_exhaustive]
pub enum Linux {
    #[serde(rename = "ubuntu")]
    Ubuntu {
        #[serde(deserialize_with = "validate_ubuntu_version_serde")]
        #[serde(rename = "ubuntu")]
        version: String,
    },
    #[serde(rename = "debian")]
    Debian {
        #[serde(rename = "debian")]
        version: u8,
    },
    #[serde(rename = "arch")]
    Arch, // no version
}

#[derive(Error, Debug)]
pub enum GetLinuxError {
    #[error(transparent)]
    OsRelease(#[from] OsReleaseError),

    #[error("missing version")]
    MissingVersionField,

    #[error("invalid ubuntu version {version_id}: {error}")]
    InvalidUbuntuVersion { version_id: String, error: String },

    #[error("invalid debian version {version_id}: {error}")]
    InvalidDebianVersion {
        version_id: String,
        #[source]
        error: ParseIntError,
    },

    #[error("unknown linux distribution")]
    UnknownLinux { id: String },
}

impl Linux {
    pub async fn get() -> Result<Self, GetLinuxError> {
        let os_release = block_in_place(OsRelease::open)?;

        let id = os_release.id();
        let version_id = os_release.version_id();

        let linux = match id {
            "ubuntu" => {
                let Some(version_id) = version_id else {
                    return Err(GetLinuxError::MissingVersionField);
                };
                validate_ubuntu_version(version_id).map_err(|error| {
                    GetLinuxError::InvalidUbuntuVersion {
                        version_id: version_id.to_owned(),
                        error,
                    }
                })?;
                Linux::Ubuntu {
                    version: version_id.to_owned(),
                }
            }
            "debian" => {
                let Some(version_id) = version_id else {
                    return Err(GetLinuxError::MissingVersionField);
                };
                let version = u8::from_str(version_id).map_err(|error| {
                    GetLinuxError::InvalidDebianVersion {
                        version_id: version_id.to_owned(),
                        error,
                    }
                })?;
                Linux::Debian { version }
            }
            "arch" => Linux::Arch,
            id => {
                return Err(GetLinuxError::UnknownLinux { id: id.to_owned() });
            }
        };
        Ok(linux)
    }
}

impl Display for Linux {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Linux::Ubuntu { version } => write!(f, "ubuntu-{}", version),
            Linux::Debian { version } => write!(f, "debian-{}", version),
            Linux::Arch => write!(f, "arch"),
        }
    }
}

impl Display for Os {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Os::Linux(l) => write!(f, "linux-{}", l),
            Os::MacOS { version } => write!(f, "macos-{}", version),
        }
    }
}

fn validate_ubuntu_version_serde<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: de::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if let Err(error) = validate_ubuntu_version(&s) {
        Err(de::Error::custom(error))
    } else {
        Ok(s)
    }
}

/// Validation for Ubuntu version "YY.MM"
fn validate_ubuntu_version(s: &str) -> Result<(), String> {
    // Must be exactly two digits, a dot, then two digits
    let mut parts = s.split('.');
    let (Some(yy), Some(mm), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("Ubuntu: expected YY.MM".into());
    };

    let year_ok = yy.len() == 2 && yy.chars().all(|c| c.is_ascii_digit());
    let month_ok = mm.len() == 2
        && mm.chars().all(|c| c.is_ascii_digit())
        && matches!(mm.parse::<u8>(), Ok(1..=12));

    if year_ok && month_ok {
        Ok(())
    } else {
        Err("invalid Ubuntu version (expected YY.MM, with 01-12 for MM)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::from_str;

    #[test]
    fn ubuntu_valid() {
        let j = r#"{
            "type": "linux",
            "linux": "ubuntu",
            "ubuntu": "22.04"
        }"#;
        let os: Os = from_str(j).unwrap();
        assert_eq!(os.to_string(), "linux-ubuntu-22.04");
    }

    #[test]
    fn ubuntu_invalid_month() {
        let j = r#"{
            "type": "linux",
            "linux": "ubuntu",
            "ubuntu": "22.15"
        }"#;
        let err = serde_json::from_str::<Os>(j).unwrap_err();
        assert!(err.to_string().contains("invalid Ubuntu version"));
    }

    #[test]
    fn debian_u8() {
        let j = r#"{
            "type": "linux",
            "linux": "debian",
            "debian": 12
        }"#;
        let os: Os = from_str(j).unwrap();
        assert_eq!(os.to_string(), "linux-debian-12");
    }

    #[test]
    fn arch_unit_variant() {
        let j = r#"{
            "type": "linux",
            "linux": "arch"
        }"#;
        let os: Os = from_str(j).unwrap();
        assert_eq!(os.to_string(), "linux-arch");
    }

    #[test]
    fn macos_version() {
        let j = r#"{
            "type": "macos",
            "macos": "15.3.1"
        }"#;
        let os: Os = from_str(j).unwrap();
        assert_eq!(os.to_string(), "macos-15.3.1");
    }
}
