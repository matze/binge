//! Default and loaded binge configuration.

use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::path::PathBuf;
use xdg::BaseDirectories;

#[derive(Deserialize)]
struct Toml {
    /// Installation path
    install_path: PathBuf,
}

pub(crate) struct Config {
    base_dir: BaseDirectories,
    toml: Option<Toml>,
}

impl Config {
    /// Load configuration or create a default one.
    pub(crate) fn new() -> Result<Self> {
        let base_dir = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));

        let toml = base_dir
            .find_config_file("binge.toml")
            .map(|path| std::fs::read_to_string(path))
            .transpose()?
            .map(|content| toml::from_str(&content))
            .transpose()?;

        Ok(Self { base_dir, toml })
    }

    /// Return path to [`crate::manifest::Manifest`] file.
    pub(crate) fn manifest_path(&self) -> Result<PathBuf> {
        Ok(self.base_dir.place_state_file("manifest.toml")?)
    }

    /// Return installation target directory. If not explicitly specified in the configuration,
    /// check if `~/.local/bin` is in `$PATH` and return that.
    pub(crate) fn install_path(&self) -> Result<PathBuf> {
        if let Some(toml) = &self.toml {
            return Ok(toml.install_path.clone());
        }

        // TODO: test
        let var = std::env::var("PATH")?;

        for path in std::env::split_paths(&var) {
            if path.ends_with(".local/bin") {
                return Ok(path);
            }
        }

        Err(anyhow!(
            "no suitable destination directory found, consider configuring one"
        ))
    }
}
