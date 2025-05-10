use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use futures_lite::{FutureExt, StreamExt};
use manifest::Repo;
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
    Install { repos: Vec<Repo> },
    /// Uninstall release binaries.
    Uninstall { repos: Vec<Repo> },
    /// Find and install updates for installed binaries.
    Update,
    /// Rename a binary.
    Rename { repo: Repo },
    /// List installed binaries
    List {
        /// Dump the list in a format that can be used in the install command.
        #[arg(value_enum, default_value_t = Format::Default)]
        format: Format,
    },
}

#[derive(Clone, ValueEnum)]
enum Format {
    /// Default list format with one binary per line.
    Default,
    /// List format suitable for the install command.
    Install,
}

/// Print `message` and a spinner on the same line forever.
async fn progress<T>(message: &str) -> T {
    let mut next_spinner = ["⠖", "⠲", "⠴", "⠦"].into_iter().cycle();
    let wait_duration = Duration::from_millis(100);

    loop {
        let spinner = next_spinner.next().expect("cycle to provide");
        print!("\x1B[2K\r{message} {}", spinner.bright_black());
        std::io::stdout().flush().expect("flushing stdout");
        tokio::time::sleep(wait_duration).await;
    }
}

/// Install all `repose` and update the `manifest`.
async fn install(
    repos: Vec<Repo>,
    config: &config::Config,
    mut manifest: Manifest,
    token: Option<String>,
) -> Result<Manifest> {
    let (already_installed, to_be_installed): (Vec<_>, Vec<_>) =
        repos.into_iter().partition(|repo| manifest.exists(repo));

    let already_installed = already_installed
        .into_iter()
        .map(|repo| repo.to_string())
        .collect::<Vec<_>>();

    if !already_installed.is_empty() {
        println!("{} already installed", already_installed.join(", "));
    }

    let start = std::time::Instant::now();
    let mut group = futures_concurrency::future::FutureGroup::new();

    let client = gh::make_client(token)?;
    let install_path = config.install_path()?;

    for repo in to_be_installed {
        group.insert({
            let client = client.clone();
            let install_path = install_path.clone();
            async move { gh::install(client, repo, &install_path).await }
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
                println!(
                    "{} {} {}",
                    "Installed ".bright_green().bold(),
                    binary.repo,
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
fn uninstall(repos: Vec<Repo>, Manifest { version, binaries }: Manifest) -> Result<Manifest> {
    let (to_be_uninstalled, binaries): (Vec<_>, Vec<_>) = binaries
        .into_iter()
        .partition(|binary| repos.iter().any(|repo| *repo == binary.repo));

    for binary in to_be_uninstalled {
        std::fs::remove_file(&binary.path)?;
        println!("{} {}", "Uninstalled".bright_green().bold(), binary.repo);
    }

    Ok(Manifest { version, binaries })
}

/// Concurrently update all installed binaries listed in the manifest.
async fn update(
    Manifest { version, binaries }: Manifest,
    token: Option<String>,
) -> Result<Manifest> {
    enum Update {
        NotFound(Binary),
        Installed { old: Binary, new: Binary },
        Error { old: Binary, err: anyhow::Error },
    }

    let mut group = futures_concurrency::future::FutureGroup::new();
    let client = gh::make_client(token)?;
    let start = std::time::Instant::now();

    for binary in binaries {
        group.insert({
            let client = client.clone();

            async move {
                match gh::update(client, &binary).await {
                    Ok(None) => Update::NotFound(binary),
                    Ok(Some(new)) => Update::Installed { old: binary, new },
                    Err(err) => Update::Error { old: binary, err },
                }
            }
        });
    }

    let group = std::pin::pin!(group);
    let updates = group.collect::<Vec<_>>();
    let message = format!("{} for new releases ...", "Checking".bright_green().bold());
    let updates = updates.or(progress(&message)).await;

    let end = std::time::Instant::now();
    println!("\x1B[2K\r{message} took {:?}", end - start);

    let binaries = updates
        .into_iter()
        .map(|update| match update {
            Update::NotFound(old) => old,
            Update::Installed { old, new } => {
                println!(
                    "{} {} ({} -> {})",
                    "Updated".bright_green(),
                    old.repo,
                    old.version,
                    new.version
                );

                new
            }
            Update::Error { old, err } => {
                eprintln!(
                    "{}: failed to update {}: {err:?}",
                    "Error".bright_red().bold(),
                    old.repo,
                );

                old
            }
        })
        .collect::<_>();

    Ok(Manifest { version, binaries })
}

/// Rename `repo` found in the manifest's binaries.
fn rename(
    repo: Repo,
    Manifest {
        version,
        mut binaries,
    }: Manifest,
) -> Result<Manifest> {
    let Some(new_name) = &repo.rename else {
        return Ok(Manifest { version, binaries });
    };

    if let Some(index) = binaries.iter().position(|binary| binary.repo == repo) {
        if let Some(elem) = binaries.get_mut(index) {
            let from = elem.path.clone();

            elem.path.pop();
            elem.path.push(new_name);
            std::fs::rename(&from, &elem.path)?;

            println!("{} {:?} -> {:?}", "Renamed".bright_green(), from, elem.path);
        }
    }

    Ok(Manifest { version, binaries })
}

/// List all installed binaries in the `manifest`.
fn list(manifest: &Manifest, format: Format) -> Result<()> {
    let mut binaries = manifest.binaries.iter().collect::<Vec<_>>();

    binaries.sort();

    match format {
        Format::Default => {
            for binary in binaries {
                println!("{} {}", binary.repo, binary.version);
            }
        }
        Format::Install => {
            let output = binaries
                .iter()
                .map(|binary| {
                    let Repo {
                        owner,
                        name,
                        rename,
                    } = &binary.repo;

                    if let Some(rename) = rename {
                        format!("{owner}/{name}:{rename}")
                    } else {
                        format!("{owner}/{name}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            println!("{output}");
        }
    }

    Ok(())
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::Config::new()?;
    let manifest = Manifest::load_or_create(&config)?;
    let token = std::env::var("GITHUB_TOKEN").ok();

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
        Commands::Install { repos } => install(repos, &config, manifest, token)
            .await?
            .save(&config)?,
        Commands::Uninstall { repos } => uninstall(repos, manifest)?.save(&config)?,
        Commands::Update => update(manifest, token).await?.save(&config)?,
        Commands::Rename { repo } => rename(repo, manifest)?.save(&config)?,
        Commands::List { format } => list(&manifest, format)?,
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(err) = try_main().await {
        eprintln!("{}: {err:?}", "Error".bright_red().bold());
    }
}
