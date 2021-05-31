//! # External build system
//!
//! This module implements the core interface for Cargo to invoke an
//! external program to compile code, compute fingerprints and other
//! operations needed to build packages in build_systems other than Rust.
//!
use anyhow::{bail, Context};
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::core::compiler::{CrateType, OutputFile, Unit};
use crate::core::{Edition, Features, Target};
use crate::util::toml::TomlManifest;
use crate::util::{closest_msg, CargoResult};
use cargo_util::ProcessBuilder;

#[cfg(unix)]
fn is_executable<P: AsRef<Path>>(path: P) -> bool {
    use std::os::unix::prelude::*;
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable<P: AsRef<Path>>(path: P) -> bool {
    path.as_ref().is_file()
}

/// Provides information specific to building a package in a specific build system.
#[derive(Debug)]
struct BuildSystem {
    /// Path to executable to run.
    path: PathBuf,
    /// Hash of the build system toolset.
    hash: u64,
}

impl BuildSystem {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            hash: 0, // TODO: FIXME
        }
    }

    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Return new process builder for running build command.
    fn command(&self) -> ProcessBuilder {
        ProcessBuilder::new(self.path.as_os_str())
    }
}

/// Encapsulates access to external build systems.
#[derive(Debug)]
pub struct ExternalBuildMgr {
    /// Map from build system names to operations for build system.
    build_systems: HashMap<String, BuildSystem>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct TargetRequest {
    pub package_name: String,
    pub package_root: OsString,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub enum ExtTargetKind {
    Bin,
    Lib,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct ExtTarget {
    pub kind: ExtTargetKind,
    pub name: String,
    pub src_path: OsString,
}

impl ExtTarget {
    fn mk_target(&self) -> CargoResult<Target> {
        match self.kind {
            ExtTargetKind::Bin => Ok(Target::bin_target(
                &self.name,
                PathBuf::from(&self.src_path),
                None,
                Edition::Edition2021,
            )),
            ExtTargetKind::Lib => Ok(Target::lib_target(
                &self.name,
                vec![CrateType::Lib],
                PathBuf::from(&self.src_path),
                Edition::Edition2021,
            )),
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
pub enum TargetResult {
    Success {
        targets: Vec<ExtTarget>,
        warnings: Vec<String>,
        errors: Vec<String>,
    },
    Failure {
        message: String,
    },
}

impl ExternalBuildMgr {
    /// Create a new build system
    pub fn new<'a>(search_paths: impl Iterator<Item = &'a PathBuf>) -> Self {
        let mut build_systems = HashMap::new();

        let prefix = "cargobuild-";
        let suffix = env::consts::EXE_SUFFIX;
        for dir in search_paths {
            let entries = fs::read_dir(dir).into_iter().flatten().flatten();
            for entry in entries {
                let path = entry.path();
                let filename = match path.file_name().and_then(|s| s.to_str()) {
                    Some(filename) => filename,
                    _ => continue,
                };
                if !filename.starts_with(prefix) || !filename.ends_with(suffix) {
                    continue;
                }
                if is_executable(entry.path()) {
                    let end = filename.len() - suffix.len();
                    let build_id = filename[prefix.len()..end].to_string();
                    let r = BuildSystem::new(dir.join(filename));
                    build_systems.insert(build_id, r);
                }
            }
        }
        ExternalBuildMgr { build_systems }
    }

    /// Get runner associated with particular build system.
    fn build_system(&self, build_id: &str) -> CargoResult<&BuildSystem> {
        match self.build_systems.get(build_id) {
            Some(r) => Ok(r),
            None => {
                let suggestions = self.build_systems.keys();
                let did_you_mean = closest_msg(build_id, suggestions, |c| c);
                let msg = anyhow::format_err!("Unknown build system {}{}", build_id, did_you_mean);
                Err(msg)
            }
        }
    }

    pub fn targets(
        &self,
        build_id: &str,
        _features: &Features,
        _manifest: &TomlManifest,
        package_name: &str,
        package_root: &Path,
        warnings: &mut Vec<String>,
        errors: &mut Vec<String>,
    ) -> CargoResult<Vec<Target>> {
        let runner = self.build_system(build_id)?;

        let mut command = Command::new(runner.path.as_os_str());
        command.arg("targets");
        command.env_clear();
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| anyhow::format_err!("Could not launch {}", runner.path.display()))?;

        let stdin = child.stdin.take().unwrap();
        let req: TargetRequest = TargetRequest {
            package_name: package_name.to_string(),
            package_root: package_root.as_os_str().to_os_string(),
        };
        serde_json::to_writer(stdin, &req)?;

        let mut stdout = child.stdout.take().unwrap();
        let mut buffer = String::new();
        stdout.read_to_string(&mut buffer)?;
        let json_result: TargetResult = serde_json::from_str(&buffer)
            .with_context(|| format!("Invalid target result from `{}`", runner.path.display()))?;
        let ecode = child
            .wait()
            .map_err(|_| anyhow::format_err!("{} failed to terminate", runner.path.display()))?;
        if !ecode.success() {
            bail!("{} exited with {}", runner.path.display(), ecode);
        }

        match json_result {
            TargetResult::Success {
                targets: j_tgts,
                warnings: mut j_warnings,
                errors: mut j_errors,
            } => {
                let mut targets = vec![];
                for tgt in j_tgts {
                    targets.push(tgt.mk_target()?);
                }
                warnings.append(&mut j_warnings);
                errors.append(&mut j_errors);
                Ok(targets)
            }
            TargetResult::Failure { message } => Err(anyhow::format_err!(message)),
        }
    }

    /// This returns the hash of the toolchain for the given build system.
    pub fn toolchain_hash(&self, build_id: &str) -> CargoResult<u64> {
        Ok(self.build_system(build_id)?.hash())
    }

    /// Return outputs for unit.
    pub fn outputs(&self, _build_id: &str, _unit: &Unit) -> CargoResult<Vec<OutputFile>> {
        Ok(vec![]) // TODO: FIXME
    }

    /// Run the compiler for the given build system
    pub fn compiler(&self, build_id: &str) -> CargoResult<ProcessBuilder> {
        let r = self.build_system(build_id)?;
        let mut cmd = r.command();
        cmd.arg("build");
        Ok(cmd)
    }
}
