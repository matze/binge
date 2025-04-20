use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use futures_lite::{FutureExt, StreamExt};
use gh::Update;
use manifest::{Binary, Manifest};
use owo_colors::OwoColorize;
use std::{io::Write, time::Duration};

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

/// Install all `repose` and update the `manifest`.
async fn install(
    repos: Vec<String>,
    config: &config::Config,
    mut manifest: Manifest,
) -> Result<Manifest> {
    let (already_installed, to_be_installed): (Vec<_>, Vec<_>) =
        repos.into_iter().partition(|repo| manifest.exists(repo));

    let already_installed = already_installed
        .into_iter()
        .map(|repo| gh::Location::new(&repo).map(|location| location.to_string()))
        .collect::<Result<Vec<_>>>()?;

    if !already_installed.is_empty() {
        println!("{} already installed", already_installed.join(", "));
    }

    let start = std::time::Instant::now();
    let mut group = futures_concurrency::future::FutureGroup::new();

    let client = gh::make_client()?;
    let install_path = config.install_path()?;

    for repo in to_be_installed {
        group.insert({
            let client = client.clone();
            let install_path = install_path.clone();
            async move { gh::install(client, &repo, &install_path).await }
        });
    }

    let group = std::pin::pin!(group);
    let results = group.collect::<Vec<_>>();

    let message = format!("{} ...", "Installing".bright_green().bold());
    let results = results.or(progress(&message)).await;
    let end = std::time::Instant::now();
    println!("\x1B[2K\r{message} took {:?}", end - start);

    for result in results {
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

/// Concurrently update all installed binaries listed in the manifest.
async fn update(Manifest { version, binaries }: Manifest) -> Result<Manifest> {
    let mut group = futures_concurrency::future::FutureGroup::new();
    let client = gh::make_client()?;

    let start = std::time::Instant::now();

    for binary in binaries {
        group.insert({
            let client = client.clone();

            async move {
                match gh::update(client, &binary).await {
                    Ok(Update::Existing) => binary,
                    Ok(Update::Updated(binary)) => binary,
                    Err(err) => {
                        // TODO: collect these and print them out later
                        eprintln!("err: failed to update: {err:?}");
                        binary
                    }
                }
            }
        });
    }

    let group = std::pin::pin!(group);
    let binaries = group.collect::<Vec<_>>();
    let message = format!("{} for new releases ...", "Checking".bright_green().bold());
    let binaries = binaries.or(progress(&message)).await;

    let end = std::time::Instant::now();
    println!("\x1B[2K\r{message} took {:?}", end - start);

    Ok(Manifest { version, binaries })
}

/// List all installed binaries in the `manifest`.
fn list(manifest: &Manifest) -> Result<()> {
    let mut binaries = manifest.binaries.iter().collect::<Vec<_>>();

    binaries.sort();

    for binary in binaries {
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
