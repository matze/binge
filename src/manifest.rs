//! Manage the local installation manifest.

use crate::config::Config;
use anyhow::{Result, anyhow};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fs::File,
    io::{BufReader, BufWriter},
    path::PathBuf,
};

#[derive(Serialize, Deserialize, Debug, Default)]
pub(crate) struct Manifest {
    /// Version of the manifest format.
    pub version: i64,
    /// Installed binaries.
    pub binaries: Vec<Binary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq, PartialEq)]
pub(crate) struct Binary {
    /// Repository where this binary is from.
    pub repo: Repo,
    /// Path to the binary executable.
    pub path: PathBuf,
    /// Installed version of the executable.
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Eq)]
pub(crate) struct Repo {
    /// Owner of the repository
    pub owner: String,
    /// Name of the repository
    pub name: String,
    /// Optional name of the binary
    pub rename: Option<String>,
}

impl PartialEq for Repo {
    fn eq(&self, other: &Self) -> bool {
        self.owner.eq(&other.owner) && self.name.eq(&other.name)
    }
}

impl Ord for Repo {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.owner.cmp(&other.owner), self.name.cmp(&other.name)) {
            (Ordering::Less, Ordering::Less) => Ordering::Less,
            (Ordering::Less, Ordering::Equal) => Ordering::Less,
            (Ordering::Less, Ordering::Greater) => Ordering::Less,
            (Ordering::Equal, Ordering::Less) => Ordering::Less,
            (Ordering::Equal, Ordering::Equal) => Ordering::Equal,
            (Ordering::Equal, Ordering::Greater) => Ordering::Greater,
            (Ordering::Greater, Ordering::Less) => Ordering::Greater,
            (Ordering::Greater, Ordering::Equal) => Ordering::Greater,
            (Ordering::Greater, Ordering::Greater) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Repo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialOrd for Binary {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Binary {
    fn cmp(&self, other: &Self) -> Ordering {
        self.repo.cmp(&other.repo)
    }
}

impl std::str::FromStr for Repo {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let mut split = s.split('/');

        let owner = split
            .next()
            .ok_or(anyhow!("{} has no slash", s))?
            .to_owned();

        let repo = split.next().ok_or(anyhow!("{} has no repo", s))?;

        if split.next().is_some() {
            return Err(anyhow!("{s} is not of owner/repo format"));
        }

        let mut split = repo.split(':');

        let name = split.next().ok_or(anyhow!("{repo} is not a repo"))?;
        let rename = split.next().map(String::from);

        Ok(Self {
            owner,
            name: name.to_owned(),
            rename,
        })
    }
}

impl std::fmt::Display for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}",
            self.owner.bright_black(),
            self.name.bright_purple().bold(),
        )
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

    pub(crate) fn exists(&self, repo: &Repo) -> bool {
        self.binaries.iter().any(|binary| binary.repo == *repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn compare_repo() -> Result<()> {
        assert!(Repo::from_str("foo/bar")? < Repo::from_str("foo/qux")?);
        assert!(Repo::from_str("foo/bar")? < Repo::from_str("qux/bar")?);

        Ok(())
    }

    #[test]
    fn parse_repo() -> Result<()> {
        assert!(Repo::from_str("foo").is_err());
        assert!(Repo::from_str("foo/bar/baz").is_err());

        let repo = Repo::from_str("foo/bar")?;
        assert_eq!(repo.owner, "foo");
        assert_eq!(repo.name, "bar");
        assert!(repo.rename.is_none());

        let repo = Repo::from_str("foo/bar:baz")?;
        assert_eq!(repo.owner, "foo");
        assert_eq!(repo.name, "bar");
        let rename = repo.rename.unwrap();
        assert_eq!(rename, "baz");

        Ok(())
    }
}
