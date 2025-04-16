use std::{io::Write, time::Duration};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use futures_concurrency::future::Race;
use futures_lite::StreamExt;
use gh::Update;
use manifest::{Binary, Manifest};
use owo_colors::OwoColorize;

mod config;
mod extract;
mod gh;
mod manifest;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate shell completion.
    Completion { shell: Shell },
    /// Install release binaries from the given repos.
    Install { repos: Vec<String> },
    /// Uninstall release binaries.
    Uninstall { repos: Vec<String> },
    /// Find and install updates for installed binaries.
    Update,
    /// List installed binaries
    List,
}

/// Install all `repose` and update the `manifest`.
async fn install(
    repos: Vec<String>,
    config: &config::Config,
    mut manifest: Manifest,
) -> Result<Manifest> {
    let (already_installed, to_be_installed): (Vec<_>, Vec<_>) =
        repos.into_iter().partition(|repo| manifest.exists(&repo));

    let already_installed = already_installed
        .into_iter()
        .map(|repo| gh::Location::new(&repo).map(|location| location.to_string()))
        .collect::<Result<Vec<_>>>()?;

    if !already_installed.is_empty() {
        println!("{} already installed", already_installed.join(", "));
    }

    let mut group = futures_concurrency::future::FutureGroup::new();

    let client = gh::make_client()?;
    let install_path = config.install_path()?;

    for repo in to_be_installed {
        let location = gh::Location::new(&repo)?;
        println!("{} {location} ...", "Installing".bright_green().bold());

        let repo = gh::Repo::new(repo)?;

        group.insert({
            let client = client.clone();
            let install_path = install_path.clone();
            async move { repo.install(client, &install_path).await }
        });
    }

    let mut group = std::pin::pin!(group);

    while let Some(result) = group.next().await {
        match result {
            Ok(binary) => {
                let location = gh::Location::new(&binary.repo)?;

                println!(
                    "{} {location} {}",
                    "Installed ".bright_green().bold(),
                    binary.version
                );

                manifest.update(binary);
            }
            Err(err) => {
                eprintln!("{}: {err}", "Error".bright_red().bold());
            }
        }
    }

    Ok(manifest)
}

/// Uninstall all `repos` and update the provided manifest.
fn uninstall(repos: Vec<String>, Manifest { version, binaries }: Manifest) -> Result<Manifest> {
    let (to_be_uninstalled, binaries): (Vec<_>, Vec<_>) = binaries
        .into_iter()
        .partition(|binary| repos.iter().any(|repo| **repo == binary.repo));

    for binary in to_be_uninstalled {
        std::fs::remove_file(binary.path)?;
        let location = gh::Location::new(&binary.repo)?;
        println!("{} {location}", "Uninstalled".bright_green().bold());
    }

    Ok(Manifest { version, binaries })
}

/// Print `message` and a spinner on the same line forever.
async fn progress<T>(message: &str) -> T {
    let mut next_spinner = ["⠖", "⠲", "⠴", "⠦"].into_iter().cycle();
    let wait_duration = Duration::from_millis(100);

    loop {
        let spinner = next_spinner.next().expect("cycle to provide");
        print!("\x1B[2K\r{message} {}", spinner.bright_black());
        std::io::stdout().flush().unwrap();
        tokio::time::sleep(wait_duration).await;
    }
}

/// Concurrently update all installed binaries listed in the manifest.
async fn update(Manifest { version, binaries }: Manifest) -> Result<Manifest> {
    let mut group = futures_concurrency::future::FutureGroup::new();
    let client = gh::make_client()?;

    let start = std::time::Instant::now();

    for binary in binaries {
        let Binary {
            repo,
            path,
            version,
        } = binary;

        let repo = gh::Repo::new(repo)?;

        group.insert({
            let client = client.clone();
            async move { repo.update(client, version, path).await }
        });
    }

    let group = std::pin::pin!(group);

    let binaries = group
        .filter_map(|result| match result {
            Ok(Update::Updated {
                old_version,
                binary,
            }) => {
                let location = gh::Location::new(&binary.repo).unwrap();

                println!(
                    "{} {} ({} -> {})",
                    "Updated".bright_green(),
                    location,
                    old_version,
                    binary.version
                );

                Some(binary)
            }
            Ok(Update::Existing(binary)) => Some(binary),
            Err(err) => {
                eprintln!("{}: {err}", "Error".bright_red().bold());
                None
            }
        })
        .collect::<Vec<_>>();

    let message = format!("{} for new releases ...", "Checking".bright_green().bold());
    let binaries = (progress(&message), binaries).race().await;
    let end = std::time::Instant::now();
    println!("\x1B[2K\r{message} took {:?}", end - start);

    Ok(Manifest { version, binaries })
}

/// List all installed binaries in the `manifest`.
fn list(manifest: &Manifest) -> Result<()> {
    for binary in &manifest.binaries {
        let location = gh::Location::new(&binary.repo)?;
        println!("{} {}", location, binary.version);
    }

    Ok(())
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::Config::new()?;
    let manifest = Manifest::load_or_create(&config)?;

    match cli.command {
        Commands::Completion { shell } => {
            let mut cmd = Cli::command();
            let cmd = &mut cmd;

            generate(
                shell,
                cmd,
                cmd.get_name().to_string(),
                &mut std::io::stdout(),
            );
        }
        Commands::Install { repos } => install(repos, &config, manifest).await?.save(&config)?,
        Commands::Uninstall { repos } => uninstall(repos, manifest)?.save(&config)?,
        Commands::Update => update(manifest).await?.save(&config)?,
        Commands::List => list(&manifest)?,
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(err) = try_main().await {
        eprintln!("{}: {err:?}", "Error".bright_red().bold());
    }
}
