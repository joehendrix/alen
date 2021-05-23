//! # External Language
//!
//! This module implements the core interface for Cargo to invoke an
//! external program to compile code, compute fingerprints and other
//! operations needed to build packages in Languages other than Rust.
//!

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::compiler::context::OutputFile;
use crate::core::compiler::Unit;
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

/// Provides information specific to building a package in a specific language.
struct LanguageRunner {
    /// Path to executable to run.
    path: PathBuf,
    /// Hash of the language toolset.
    hash: u64,
}

impl LanguageRunner {
    pub fn new(path: PathBuf) -> CargoResult<Self> {
        Ok(Self {
            path,
            hash: 0, // TODO: FIXME
        })
    }

    pub fn hash(&self) -> u64 {
        self.hash
    }
}

/// Encapsulates access to external languages.
pub struct LanguageOps {
    /// Map from languages to path buffer
    languages: HashMap<String, LanguageRunner>,
}

impl LanguageOps {
    /// Create a new language
    pub fn new<'a>(search_paths: impl Iterator<Item = &'a PathBuf>) -> CargoResult<Self> {
        let mut languages = HashMap::new();

        let prefix = "cargobuild-";
        let suffix = env::consts::EXE_SUFFIX;
        println!("lang0");
        for dir in search_paths {
            println!("lang dir {:#?}", dir);
            let entries = fs::read_dir(dir).into_iter().flatten().flatten();
            for entry in entries {
                println!("lang entry {:#?}", entry);
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
                    let lang = filename[prefix.len()..end].to_string();
                    let r = LanguageRunner::new(dir.join(filename))?;
                    languages.insert(lang, r);
                }
            }
        }
        println!("LanguageOps::new done");
        Ok(LanguageOps { languages })
    }

    /// Get runner associated with particular language.
    fn language_runner(&self, lang: &str) -> CargoResult<&LanguageRunner> {
        match self.languages.get(lang) {
            Some(r) => Ok(r),
            None => {
                let suggestions = self.languages.keys();
                let did_you_mean = closest_msg(lang, suggestions, |c| c);
                let msg = anyhow::format_err!("Unknown language {}{}", lang, did_you_mean);
                Err(msg)
            }
        }
    }

    /// This returns the hash of the toolchain for the given language.
    pub fn toolchain_hash(&self, lang: &str) -> CargoResult<u64> {
        Ok(self.language_runner(lang)?.hash())
    }

    /// Return outputs for unit.
    pub fn outputs(&self, _lang: &str, _unit: &Unit) -> CargoResult<Vec<OutputFile>> {
        Ok(vec![]) // TODO: FIXME
    }

    /// Run the compiler for the given language
    pub fn compiler(&self, lang: &str) -> CargoResult<ProcessBuilder> {
        let r = self.language_runner(lang)?;
        let mut cmd = ProcessBuilder::new(r.path.as_os_str());
        cmd.arg("build");
        Ok(cmd)
    }
}
