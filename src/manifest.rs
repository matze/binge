//! Manage the local installation manifest.

use crate::config::Config;
use anyhow::{Result, anyhow};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::PathBuf,
};

#[derive(Serialize, Deserialize, Debug, Default)]
pub(crate) struct Manifest {
    /// Version of the manifest format.
    pub version: String,
    /// Installed binaries.
    pub binaries: Vec<Binary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub(crate) struct Binary {
    /// Repository where this binary is from.
    pub repo: String,
    /// Path to the binary executable.
    pub path: PathBuf,
    /// Installed version of the executable.
    pub version: String,
}

/// Split repo into owner and repo slices.
pub(crate) struct Location<'a> {
    owner: &'a str,
    repo: &'a str,
}

impl<'a> Location<'a> {
    pub(crate) fn new(repo: &'a str) -> Result<Self> {
        let mut split = repo.split('/');

        let owner = split.next().ok_or(anyhow!("{} has no slash", repo))?;
        let repo = split.next().ok_or(anyhow!("{} has no repo", repo))?;

        if split.next().is_some() {
            return Err(anyhow!("{repo} is not of owner/repo format"));
        }

        Ok(Self { owner, repo })
    }
}

impl Display for Location<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}",
            self.owner.bright_black(),
            self.repo.bright_purple().bold(),
        )
    }
}

impl PartialOrd for Binary {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Binary {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.repo.cmp(&other.repo)
    }
}

impl Binary {
    pub(crate) fn location(&self) -> Result<Location> {
        Location::new(&self.repo)
    }
}

impl Manifest {
    pub(crate) fn load_or_create(config: &Config) -> Result<Self> {
        let path = config.manifest_path()?;

        if path.exists() {
            Ok(serde_json::from_reader(BufReader::new(File::open(&path)?))?)
        } else {
            Ok(Self::default())
        }
    }

    pub(crate) fn save(self, config: &Config) -> Result<()> {
        let path = config.manifest_path()?;

        Ok(serde_json::to_writer(
            BufWriter::new(File::create(&path)?),
            &self,
        )?)
    }

    pub(crate) fn update(&mut self, binary: Binary) {
        if let Some(existing) = self
            .binaries
            .iter_mut()
            .find(|existing| existing.repo == binary.repo)
        {
            existing.version = binary.version;
            existing.path = binary.path;
        } else {
            self.binaries.push(binary);
        }

        self.binaries.sort();
    }

    pub(crate) fn exists(&self, repo: &str) -> bool {
        self.binaries.iter().any(|binary| binary.repo == repo)
    }
}
