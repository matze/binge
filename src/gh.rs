use crate::{Binary, extract};
use anyhow::{Result, anyhow};
use owo_colors::OwoColorize;
use regex::Regex;
use reqwest::Url;
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde::Deserialize;
use std::fmt::Display;
use std::io::BufReader;
use std::path::Path;
use std::{io::Cursor, path::PathBuf};

/// A GitHub repo.
pub(crate) struct Repo {
    location: String,
}

/// Split repo into owner and repo slices.
pub(crate) struct Location<'a> {
    owner: &'a str,
    repo: &'a str,
}

/// API release.
#[derive(Deserialize, Debug)]
pub(crate) struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

/// API asset.
#[derive(Deserialize, Debug)]
pub(crate) struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

/// Return value for updating.
pub(crate) enum Update {
    /// New version.
    Updated { old_version: String, binary: Binary },
    /// No new version found.
    Existing(Binary),
}

/// Supported compression type.
#[derive(Debug)]
pub(crate) enum Compression {
    /// Uncompressed.
    None(Archive),
    /// Gzip.
    Gz(Archive),
    /// Zstandard.
    Zstd(Archive),
    /// Xz.
    Xz(Archive),
}

/// Supported archive types.
#[derive(Debug)]
pub(crate) enum Archive {
    /// Single file
    None,
    /// Zip file.
    Zip,
    /// Tape Archive.
    Tar,
}

/// Release file information.
#[derive(Debug)]
pub(crate) struct File {
    url: Url,
    filename: PathBuf,
    kind: Compression,
}

pub(crate) fn make_client() -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();

    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );

    headers.insert(header::USER_AGENT, HeaderValue::from_static("matze"));

    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static("2022-11-28"),
    );

    let client = reqwest::ClientBuilder::new()
        .default_headers(headers)
        .brotli(true)
        .zstd(true)
        .build()?;

    Ok(client)
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

fn parse_archive(path: PathBuf) -> Archive {
    let extension = match path.extension() {
        Some(extension) => extension,
        None => return Archive::None,
    };

    let extension = extension.to_ascii_lowercase().into_string().unwrap();

    if extension.as_str() == "tar" {
        Archive::Tar
    } else {
        Archive::None
    }
}

fn parse_compression(mut path: PathBuf) -> Compression {
    let extension = match path.extension() {
        Some(extension) => extension,
        None => return Compression::None(Archive::None),
    };

    let extension = extension.to_ascii_lowercase().into_string().unwrap();
    path.set_extension("");

    let archive = parse_archive(path);

    match extension.as_str() {
        "gz" => Compression::Gz(archive),
        "xz" => Compression::Xz(archive),
        "zst" => Compression::Zstd(archive),
        "zip" => Compression::None(Archive::Zip),
        _ => Compression::None(archive),
    }
}

/// Map to alternative architecture/OS conventions.
fn alt_arch_os(arch: &'static str) -> &'static str {
    if arch == "x86_64" {
        "(x86_64|amd64)"
    } else {
        arch
    }
}

fn parse_file(filename: String, url: Url, arch: &'static str, os: &str) -> Option<File> {
    let arch = alt_arch_os(arch);

    let expr = Regex::new(&format!(
        "^.*({}-[\\w\\d-]*{}|[\\w\\d-]*{}-{}).*$",
        arch, os, os, arch
    ))
    .expect("compiling the regex");

    let filename = PathBuf::from(filename);

    if expr.find(filename.to_str().unwrap()).is_none() {
        return None;
    }

    let kind = parse_compression(filename.clone());

    Some(File {
        url,
        filename,
        kind,
    })
}

fn rename_known_binaries(path: PathBuf) -> PathBuf {
    if path
        .as_os_str()
        .to_str()
        .map_or(false, |s| s.starts_with("rust-analyzer"))
    {
        PathBuf::from("rust-analyzer")
    } else {
        path
    }
}

impl Repo {
    /// Create new repo from the given `location`.
    pub(crate) fn new(location: String) -> Result<Self, anyhow::Error> {
        let _ = Location::new(&location)?;
        Ok(Self { location })
    }

    /// Install latest version and record in the local installation manifest.
    pub(crate) async fn install(&self, client: reqwest::Client, dest_dir: &Path) -> Result<Binary> {
        let url = reqwest::Url::parse(&format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.location
        ))?;
        let Release { tag_name, assets } = client.get(url).send().await?.json().await?;
        let path = self.fetch_and_extract(client, dest_dir, assets).await?;

        Ok(Binary {
            repo: self.location.clone(),
            path,
            version: tag_name,
        })
    }

    pub(crate) async fn update(
        &self,
        client: reqwest::Client,
        version: String,
        path: PathBuf,
    ) -> Result<Update> {
        // FIXME: errors should return an Update containing an error rather than a Result
        let url = reqwest::Url::parse(&format!(
            "https://api.github.com/repos/{}/releases/latest",
            self.location
        ))?;
        let repo = self.location.clone();
        let Release { tag_name, assets } = client.get(url).send().await?.json().await?;

        // TODO: semver comparison
        if version != tag_name {
            let dest_dir = path
                .parent()
                .ok_or_else(|| anyhow!("no parent for path found"))?;

            let _ = self.fetch_and_extract(client, dest_dir, assets).await?;

            return Ok(Update::Updated {
                old_version: version,
                binary: Binary {
                    repo,
                    path,
                    version: tag_name,
                },
            });
        }

        Ok(Update::Existing(Binary {
            repo,
            path,
            version,
        }))
    }

    async fn fetch_and_extract(
        &self,
        client: reqwest::Client,
        dest_dir: &Path,
        assets: Vec<Asset>,
    ) -> Result<PathBuf> {
        let candidate = assets
            .into_iter()
            .filter_map(
                |Asset {
                     name,
                     url: browser_download_url,
                 }| {
                    let url: Url = browser_download_url.parse().ok()?;
                    parse_file(name, url, std::env::consts::ARCH, std::env::consts::OS)
                },
            )
            .filter(|file| !matches!(file.kind, Compression::None(Archive::None)))
            .next();

        if let Some(candidate) = candidate {
            let tmp = tempfile::tempdir()?.into_path();
            let filepath = tmp.join(&candidate.filename);
            let response = client.get(candidate.url).send().await?;
            let mut file = std::fs::File::create(&filepath)?;
            let mut content = Cursor::new(response.bytes().await?);
            std::io::copy(&mut content, &mut file)?;

            let reader = BufReader::new(std::fs::File::open(PathBuf::from(&filepath))?);

            let path = match candidate.kind {
                Compression::None(Archive::Zip) => extract::extract_zip(reader, dest_dir)?,
                Compression::None(Archive::Tar) => extract::extract_tar(reader, dest_dir)?,
                Compression::Gz(archive) => {
                    let input = flate2::read::GzDecoder::new(reader);

                    match archive {
                        Archive::None => {
                            let filename = rename_known_binaries(candidate.filename);
                            extract::extract_single(input, dest_dir, &filename)?
                        }
                        Archive::Zip => todo!(),
                        Archive::Tar => extract::extract_tar(input, dest_dir)?,
                    }
                }
                Compression::Zstd(Archive::Tar) => {
                    let input = zstd::Decoder::new(reader)?;
                    extract::extract_tar(input, dest_dir)?
                }
                Compression::Xz(Archive::Tar) => {
                    let input = xz2::read::XzDecoder::new(reader);
                    extract::extract_tar(input, dest_dir)?
                }
                missing => todo!("{missing:?}"),
            };

            return Ok(path);
        }

        Err(anyhow!("no asset found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Url;

    fn make_filename_and_url(name: &str) -> (String, Url) {
        let url: Url = format!("https://foo.com/{name}").parse().unwrap();
        (name.into(), url)
    }

    #[test]
    fn parse_arch_os() -> Result<()> {
        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.tar.gz");
        let file = parse_file(name.clone(), url.clone(), "x86_64", "linux").unwrap();

        assert_eq!(
            file.filename.as_os_str(),
            "bar-x86_64-unknown-linux-gnu.tar.gz"
        );

        assert!(parse_file(name, url, "aarch64", "linux").is_none());

        Ok(())
    }

    #[test]
    fn parse_compression() -> Result<()> {
        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.tar.gz");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Gz(Archive::Tar)));

        let (name, url) = make_filename_and_url("bar-amd64-unknown-linux-gnu.tar.gz");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Gz(Archive::Tar)));

        let (name, url) = make_filename_and_url("bar-linux-amd64.tar.gz");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Gz(Archive::Tar)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.tar.xz");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Xz(Archive::Tar)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.tar.zst");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Zstd(Archive::Tar)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.gz");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Gz(Archive::None)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.xz");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Xz(Archive::None)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.zst");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::Zstd(Archive::None)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu.zip");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::None(Archive::Zip)));

        let (name, url) = make_filename_and_url("bar-x86_64-unknown-linux-gnu");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::None(Archive::None)));

        Ok(())
    }
}
