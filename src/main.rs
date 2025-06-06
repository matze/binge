use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use futures::stream::futures_unordered::FuturesUnordered;
use futures_lite::{FutureExt, StreamExt};
use gh::Release;
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
    let group = FuturesUnordered::new();

    let client = gh::make_client(token)?;
    let install_path = config.install_path()?;

    for repo in to_be_installed {
        group.push({
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
        .partition(|binary| repos.contains(&binary.repo));

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
    enum Check {
        NotFound { binary: Binary },
        Found { binary: Binary, release: Release },
        Error { binary: Binary, err: anyhow::Error },
    }

    enum Update {
        None { binary: Binary },
        Installed { old: Binary, new: Binary },
        Error { binary: Binary, err: anyhow::Error },
    }

    let group = FuturesUnordered::new();
    let client = gh::make_client(token)?;

    for binary in binaries {
        group.push({
            let client = client.clone();

            async move {
                match gh::check(client, &binary).await {
                    Ok(None) => Check::NotFound { binary },
                    Ok(Some(release)) => Check::Found { binary, release },
                    Err(err) => Check::Error { binary, err },
                }
            }
        });
    }

    let group = std::pin::pin!(group);
    let checks = group.collect::<Vec<_>>();
    let message = format!("{} for new releases ...", "Checking".bright_green().bold());
    let checks = checks.or(progress(&message)).await;
    println!("\x1B[2K\r{message} ✔️");

    let to_update = checks
        .iter()
        .filter_map(|check| match check {
            Check::NotFound { binary: _ } => None,
            Check::Found { binary, release: _ } => Some(binary.repo.to_string()),
            Check::Error { binary: _, err: _ } => None,
        })
        .collect::<Vec<_>>();

    let have_updates = !to_update.is_empty();
    let group = FuturesUnordered::new();
    let mut others = Vec::new();

    for check in checks {
        match check {
            Check::NotFound { binary } => {
                others.push(Update::None { binary });
            }
            Check::Found {
                binary: old,
                release,
            } => {
                let client = client.clone();

                group.push(async move {
                    match gh::update(client, &old, release).await {
                        Ok(new) => Update::Installed { old, new },
                        Err(err) => Update::Error { binary: old, err },
                    }
                });
            }
            Check::Error { binary, err } => {
                others.push(Update::Error { binary, err });
            }
        }
    }

    let group = std::pin::pin!(group);
    let updates = group.collect::<Vec<_>>();

    let updates = if have_updates {
        let message = format!(
            "{} {} ...",
            "Updating".bright_green().bold(),
            to_update.join(", ")
        );

        let mut updates = updates.or(progress(&message)).await;
        println!("\x1B[2K\r{message} ✔️");

        updates.append(&mut others);
        updates
    } else {
        others
    };

    let binaries = updates
        .into_iter()
        .map(|update| match update {
            Update::None { binary } => binary,
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
            Update::Error { binary, err } => {
                eprintln!(
                    "{}: failed to update {}: {err:?}",
                    "Error".bright_red().bold(),
                    binary.repo,
                );

                binary
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
