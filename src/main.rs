mod config;
mod extract;
mod gh;
mod manifest;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use futures_lite::StreamExt;
use gh::Release;
use owo_colors::OwoColorize;
use strides::future::{FutureExt as _, join};
use strides::{Layout, Segment};
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::wrappers::UnboundedReceiverStream;

use manifest::Repo;
use manifest::{Binary, Manifest};

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
    /// Check for updates but do not install them.
    Check,
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

const SPINNER: strides::spinner::Spinner = strides::spinner::styles::DOTS_3;

const SPINNER_STYLE: owo_colors::Style = owo_colors::Style::new().bold().bright_green();

const PROGRESS_THEME: strides::Theme<'static> = strides::Theme::new()
    .with_spinner(SPINNER)
    .with_bar(
        strides::bar::styles::THIN_LINE
            .with_filled_style(owo_colors::Style::new().bright_purple())
            .with_empty_style(owo_colors::Style::new().bright_black()),
    )
    .with_bar_width(24);

/// Visible character count of `repo` rendered as `owner/name`, ignoring any ANSI styling added by
/// its `Display` impl.
fn repo_visible_len(repo: &Repo) -> usize {
    repo.owner.chars().count() + 1 + repo.name.chars().count()
}

/// Max width (in characters) of `"{prefix} owner/name"` across `repos`.
fn max_label_width<'a>(prefix: &str, repos: impl IntoIterator<Item = &'a Repo>) -> usize {
    let base = prefix.chars().count() + 1;
    repos
        .into_iter()
        .map(|r| base + repo_visible_len(r))
        .max()
        .unwrap_or(0)
}

/// Pre-pad `"{prefix} {repo}"` with trailing spaces so its visible width equals `max_width`. The
/// repo keeps its colored `Display` rendering; padding is added by us so strides does not have to
/// see (and miscount) the ANSI bytes.
fn aligned_label(prefix: &str, repo: &Repo, max_width: usize) -> String {
    let visible = prefix.chars().count() + 1 + repo_visible_len(repo);
    let pad = max_width.saturating_sub(visible);
    format!("{prefix} {repo}{:pad$}", "")
}

/// Spinner, label, bar, elapsed — space-separated.
fn progress_layout() -> Layout {
    Layout::new(&[])
        .with_segment(Segment::spinner())
        .with_segment(Segment::label())
        .with_segment(Segment::bar())
        .with_segment(Segment::elapsed())
}

fn progress_theme() -> strides::Theme<'static> {
    PROGRESS_THEME.with_layout(progress_layout())
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

    let label_width = max_label_width("installing", to_be_installed.iter());
    let mut group = strides::future::Group::new(progress_theme())
        .with_spinner_style(SPINNER_STYLE)
        .with_elapsed_time();

    let client = gh::make_client(token)?;
    let install_path = config.install_path()?;

    for repo in to_be_installed {
        let message = aligned_label("installing", &repo, label_width);
        let (tx, rx) = unbounded_channel::<f64>();

        group.push(
            {
                let client = client.clone();
                let install_path = install_path.clone();
                async move { gh::install(client, repo, &install_path, tx).await }
            }
            .with_label(message)
            .with_progress(UnboundedReceiverStream::new(rx)),
        );
    }

    let results = group.collect::<Vec<_>>().await;

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
        if let Err(err) = std::fs::remove_file(&binary.path) {
            eprintln!("failed to remove {:?}: {err}", binary.path);
        }

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

    let client = gh::make_client(token)?;

    let futs = binaries.into_iter().map(|binary| {
        let client = client.clone();
        async move {
            match gh::check(client, &binary).await {
                Ok(None) => Check::NotFound { binary },
                Ok(Some(release)) => Check::Found { binary, release },
                Err(err) => Check::Error { binary, err },
            }
        }
    });

    let checks: Vec<Check> = join(futs)
        .with_theme(progress_theme())
        .with_spinner_style(SPINNER_STYLE)
        .with_label("checking")
        .await;

    let to_update = checks
        .iter()
        .filter_map(|check| match check {
            Check::NotFound { binary: _ } => None,
            Check::Found { binary, release: _ } => Some(binary.repo.to_string()),
            Check::Error { binary: _, err: _ } => None,
        })
        .collect::<Vec<_>>();

    let have_updates = !to_update.is_empty();

    let update_width = max_label_width(
        "updating",
        checks.iter().filter_map(|c| match c {
            Check::Found { binary, .. } => Some(&binary.repo),
            _ => None,
        }),
    );

    let mut group = strides::future::Group::new(progress_theme()).with_spinner_style(SPINNER_STYLE);

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
                let message = aligned_label("updating", &old.repo, update_width);
                let (tx, rx) = unbounded_channel::<f64>();

                group.push(
                    async move {
                        match gh::update(&old, release, tx).await {
                            Ok(new) => Update::Installed { old, new },
                            Err(err) => Update::Error { binary: old, err },
                        }
                    }
                    .with_label(message)
                    .with_progress(UnboundedReceiverStream::new(rx)),
                );
            }
            Check::Error { binary, err } => {
                others.push(Update::Error { binary, err });
            }
        }
    }

    let updates = if have_updates {
        let mut updates = group.collect::<Vec<_>>().await;

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

/// Concurrently check all installed binaries listed in the manifest.
async fn check(manifest: Manifest, token: Option<String>) -> Result<()> {
    enum Check {
        Update { binary: Binary, release: Release },
        Error { err: anyhow::Error },
    }

    let client = gh::make_client(token)?;

    let futs = manifest.binaries.into_iter().map(|binary| {
        let client = client.clone();
        async move {
            match gh::check(client, &binary).await {
                Ok(None) => None,
                Ok(Some(release)) => Some(Check::Update { binary, release }),
                Err(err) => Some(Check::Error { err }),
            }
        }
    });

    let checks: Vec<Check> = join(futs)
        .with_theme(progress_theme())
        .with_spinner_style(SPINNER_STYLE)
        .with_label("checking")
        .await
        .into_iter()
        .flatten()
        .collect();

    for check in checks {
        match check {
            Check::Update { binary, release } => {
                println!(
                    "{} {} ({} -> {})",
                    "Found".bright_green(),
                    binary.repo,
                    binary.version,
                    release.tag_name
                );
            }
            Check::Error { err } => {
                eprintln!("{err}");
            }
        }
    }

    Ok(())
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

    if let Some(index) = binaries.iter().position(|binary| binary.repo == repo)
        && let Some(elem) = binaries.get_mut(index)
    {
        let from = elem.path.clone();

        elem.path.pop();
        elem.path.push(new_name);
        std::fs::rename(&from, &elem.path)?;

        println!("{} {:?} -> {:?}", "Renamed".bright_green(), from, elem.path);
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

fn token_from_gh_client() -> Option<String> {
    std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()
        .and_then(|output| {
            str::from_utf8(&output.stdout)
                .ok()
                .map(|s| s.trim_end().to_owned())
        })
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();
    let config = config::Config::new()?;
    let manifest = Manifest::load_or_create(&config)?;
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .or_else(token_from_gh_client);

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
        Commands::Check => check(manifest, token).await?,
        Commands::Rename { repo } => rename(repo, manifest)?.save(&config)?,
        Commands::List { format } => list(&manifest, format)?,
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let work = async {
        if let Err(err) = try_main().await {
            eprintln!("{}: {err:?}", "Error".bright_red().bold());
        }
    };

    let on_interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
        let _ = strides::term::reset();
    };

    futures_lite::future::race(work, on_interrupt).await;
}
