//! `lusid.toml` deserialization. Splits into an on-disk `ConfigToml`
//! (deserialized straight from TOML) and an in-memory [`Config`] where plan
//! paths have been resolved to absolute, CLI/env overrides have been
//! applied, and defaults filled in.

use comfy_table::Table;
use lusid_machine::Machine;
use lusid_system::{Arch, Hostname, OsKind};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs::read_to_string;
use toml::Value;

use crate::Cli;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("lusid config not found at: {path}")]
    ConfigNotFound { path: PathBuf },

    #[error("local machine not found: {hostname}")]
    LocalMachineNotFound { hostname: Hostname },

    #[error("machine id not found: {machine_id}")]
    MachineIdNotFound { machine_id: String },

    #[error("no lusid-apply binary configured for {os}/{arch}")]
    MissingApplyPath { os: OsKind, arch: Arch },

    #[error("failed to get hostname: {0}")]
    GetHostname(#[source] io::Error),

    #[error("failed to read machines file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse machines file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to resolve plan path: {base_path} + {plan_path}")]
    ResolvingPlanPath {
        base_path: PathBuf,
        plan_path: PathBuf,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ConfigToml {
    #[serde(default)]
    pub machines: BTreeMap<String, MachineConfigToml>,
    pub log: Option<String>,
    pub lusid_apply_linux_x86_64_path: Option<String>,
    pub lusid_apply_linux_aarch64_path: Option<String>,
    pub lusid_apply_macos_x86_64_path: Option<String>,
    pub lusid_apply_macos_aarch64_path: Option<String>,
}

/// Resolved configuration. `path` is the original config file location
/// (used to derive `root()`, the plan-resolution base). `machines` map is
/// keyed by the TOML section name. `apply_paths` maps each supported target
/// `(OsKind, Arch)` to the `lusid-apply` binary to use when applying to it —
/// looked up via [`Config::apply_path`].
#[derive(Debug, Clone)]
pub struct Config {
    pub path: PathBuf,
    pub machines: BTreeMap<String, MachineConfig>,
    pub log: String,
    pub apply_paths: HashMap<(OsKind, Arch), String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MachineConfigToml {
    #[serde(flatten)]
    pub machine: Machine,
    pub plan: PathBuf,
    pub params: Option<Value>,
}

/// Per-machine entry. `plan` is already resolved to an absolute path (see
/// [`Config::resolve_plan_path`]); `params` is a raw TOML value that will
/// be converted to JSON and handed to `lusid-apply --params`.
#[derive(Debug, Clone)]
pub struct MachineConfig {
    pub machine: Machine,
    pub plan: PathBuf,
    pub params: Option<Value>,
}

impl Config {
    pub async fn load(path: &Path, cli: &Cli) -> Result<Self, ConfigError> {
        let config = Self::load_config(path).await?;
        let ConfigToml {
            machines,
            log,
            lusid_apply_linux_x86_64_path,
            lusid_apply_linux_aarch64_path,
            lusid_apply_macos_x86_64_path,
            lusid_apply_macos_aarch64_path,
        } = config;

        let machines = Self::resolve_machines(machines, path)?;

        let log = cli.log.clone().or(log).unwrap_or("error".into());

        // Resolution per key: CLI flag → lusid.toml → default (`lusid-apply-<os>-<arch>`
        // resolved via PATH at spawn time). Keeping the defaults populated for all
        // known platforms means a user who just has the binaries on PATH doesn't need
        // any config at all.
        let apply_paths: HashMap<(OsKind, Arch), String> = [
            (
                (OsKind::Linux, Arch::X86_64),
                cli.lusid_apply_linux_x86_64_path
                    .clone()
                    .or(lusid_apply_linux_x86_64_path)
                    .unwrap_or_else(|| "lusid-apply-linux-x86-64".into()),
            ),
            (
                (OsKind::Linux, Arch::Aarch64),
                cli.lusid_apply_linux_aarch64_path
                    .clone()
                    .or(lusid_apply_linux_aarch64_path)
                    .unwrap_or_else(|| "lusid-apply-linux-aarch64".into()),
            ),
            (
                (OsKind::MacOS, Arch::X86_64),
                cli.lusid_apply_macos_x86_64_path
                    .clone()
                    .or(lusid_apply_macos_x86_64_path)
                    .unwrap_or_else(|| "lusid-apply-macos-x86-64".into()),
            ),
            (
                (OsKind::MacOS, Arch::Aarch64),
                cli.lusid_apply_macos_aarch64_path
                    .clone()
                    .or(lusid_apply_macos_aarch64_path)
                    .unwrap_or_else(|| "lusid-apply-macos-aarch64".into()),
            ),
        ]
        .into_iter()
        .collect();

        Ok(Config {
            path: path.to_owned(),
            machines,
            log,
            apply_paths,
        })
    }

    /// Look up the `lusid-apply` binary for a target `(os, arch)` pair.
    ///
    /// Returns the path (or plain binary name — callers pass through `Command::new`
    /// or `which`, both of which consult `PATH`).
    pub fn apply_path(&self, os: OsKind, arch: Arch) -> Result<&str, ConfigError> {
        self.apply_paths
            .get(&(os, arch))
            .map(|s| s.as_str())
            .ok_or(ConfigError::MissingApplyPath { os, arch })
    }

    pub fn get_machine(&self, machine_id: &str) -> Result<MachineConfig, ConfigError> {
        self.machines
            .get(machine_id)
            .cloned()
            .ok_or_else(|| ConfigError::MachineIdNotFound {
                machine_id: machine_id.to_string(),
            })
    }

    /// Look up the machine whose hostname matches the host we're running on.
    /// Used by `local apply` — the user doesn't specify which machine to
    /// apply, we infer it. Errors if no configured machine matches.
    pub fn local_machine(&self) -> Result<MachineConfig, ConfigError> {
        let hostname = Hostname::get().map_err(ConfigError::GetHostname)?;
        self.machines
            .values()
            .find(|cfg| cfg.machine.hostname == hostname)
            .ok_or(ConfigError::LocalMachineNotFound { hostname })
            .cloned()
    }

    pub fn print_machines(&self) {
        let mut table = Table::new();
        table
            .load_preset(comfy_table::presets::UTF8_FULL)
            .apply_modifier(comfy_table::modifiers::UTF8_ROUND_CORNERS)
            .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
            .set_header(vec!["id", "plan", "hostname", "arch", "os"]);

        for (machine_id, config) in self.machines.iter() {
            let MachineConfig {
                machine,
                plan,
                params: _,
            } = config;
            let Machine {
                hostname,
                arch,
                os,
                vm: _,
            } = machine;
            table.add_row(vec![
                machine_id,
                &plan.to_string_lossy().to_string(),
                &hostname.to_string(),
                &arch.to_string(),
                &os.to_string(),
            ]);
        }

        println!("{table}")
    }

    pub fn root(&self) -> &Path {
        self.path.parent().unwrap()
    }

    async fn load_config(path: &Path) -> Result<ConfigToml, ConfigError> {
        let path = if path.is_dir() {
            path.join("lusid.toml")
        } else {
            path.to_owned()
        };
        let string = read_to_string(&path)
            .await
            .map_err(|source| ConfigError::Read {
                path: path.to_owned(),
                source,
            })?;
        let config = toml::from_str(&string).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        Ok(config)
    }

    fn resolve_machines(
        machines: BTreeMap<String, MachineConfigToml>,
        plan_path: &Path,
    ) -> Result<BTreeMap<String, MachineConfig>, ConfigError> {
        machines
            .into_iter()
            .map(|(name, config)| {
                let MachineConfigToml {
                    machine,
                    plan,
                    params,
                } = config;
                Ok((
                    name,
                    MachineConfig {
                        machine,
                        plan: Self::resolve_plan_path(plan_path, &plan)?,
                        params,
                    },
                ))
            })
            .collect::<Result<_, _>>()
    }

    fn resolve_plan_path(base_path: &Path, plan_path: &Path) -> Result<PathBuf, ConfigError> {
        if plan_path.is_absolute() {
            Ok(plan_path.to_path_buf())
        } else {
            base_path
                .parent()
                .map(|parent| parent.join(plan_path))
                .ok_or_else(|| ConfigError::ResolvingPlanPath {
                    base_path: base_path.to_owned(),
                    plan_path: plan_path.to_owned(),
                })
        }
    }
}
