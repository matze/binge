use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use futures_lite::{Stream, StreamExt};
use regex::Regex;
use reqwest::Url;
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::bytes::Bytes;

use crate::{Binary, Repo, extract};

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

/// Create a new client usable for GitHub APIs.
pub(crate) fn make_client(token: Option<String>) -> Result<reqwest::Client> {
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

    if let Some(token) = token {
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    }

    let client = reqwest::ClientBuilder::new()
        .default_headers(headers)
        .brotli(true)
        .zstd(true)
        .build()?;

    Ok(client)
}

fn parse_archive(path: PathBuf) -> Archive {
    let extension = match path.extension() {
        Some(extension) => extension,
        None => return Archive::None,
    };

    let extension = extension
        .to_ascii_lowercase()
        .into_string()
        .unwrap_or_default();

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

    let extension = extension
        .to_ascii_lowercase()
        .into_string()
        .unwrap_or_default();

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
        "(x86_64|amd64|x64)"
    } else {
        arch
    }
}

fn parse_file(filename: String, url: Url, arch: &'static str, os: &str) -> Option<File> {
    let arch = alt_arch_os(arch);

    let expr = Regex::new(&format!(
        "^.*({arch}-[\\w\\d-]*{os}|[\\w\\d-]*{os}-{arch}).*$"
    ))
    .expect("compiling the regex");

    let filename = PathBuf::from(filename);

    expr.find(filename.to_str()?)?;

    let kind = parse_compression(filename.clone());

    Some(File {
        url,
        filename,
        kind,
    })
}

/// Wrap a `reqwest::Response`'s byte stream so each chunk reports the cumulative download fraction
/// to `tx`. If the response did not advertise a `Content-Length`, no progress is emitted.
fn report_progress(
    response: reqwest::Response,
    tx: &UnboundedSender<f64>,
) -> impl Stream<Item = reqwest::Result<Bytes>> + use<> {
    let total = response.content_length().filter(|n| *n > 0);
    let tx = tx.clone();

    response
        .bytes_stream()
        .scan(0u64, move |downloaded, chunk| {
            if let Ok(c) = &chunk {
                *downloaded = downloaded.saturating_add(u64::try_from(c.len()).unwrap_or(u64::MAX));

                if let Some(total) = total {
                    #[allow(clippy::cast_precision_loss)]
                    let p = *downloaded as f64 / total as f64;
                    let _ = tx.send(p);
                }
            }

            Some(chunk)
        })
}

async fn fetch_and_extract(
    client: reqwest::Client,
    dest_dir: &Path,
    assets: Vec<Asset>,
    progress: UnboundedSender<f64>,
) -> Result<PathBuf> {
    let mut candidates = assets
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
        .filter(|f| {
            f.filename
                .extension()
                .map(|ext| ext != "vsix")
                .unwrap_or(true)
        });

    if let Some(candidate) = candidates.next() {
        let response = client.get(candidate.url).send().await?;
        let bytes = report_progress(response, &progress);

        let path = match candidate.kind {
            Compression::None(Archive::Zip) => {
                let mut buffer = Vec::new();
                let mut bytes = std::pin::pin!(bytes);

                while let Some(chunk) = bytes.next().await {
                    buffer.extend_from_slice(&chunk?);
                }

                extract::extract_zip(buffer, dest_dir).await?
            }
            Compression::None(Archive::Tar) => {
                let read = std::pin::pin!(stream_to_reader(bytes));
                extract::extract_tar(read, dest_dir).await?
            }
            Compression::Gz(archive) => {
                let read = stream_to_reader(bytes);
                let read = tokio::io::BufReader::new(read);
                let input = async_compression::tokio::bufread::GzipDecoder::new(read);

                match archive {
                    Archive::None => {
                        let path = dest_dir.join(candidate.filename);
                        extract::write_async(input, &path, 0o755).await?;
                        path
                    }
                    Archive::Zip => todo!(),
                    Archive::Tar => extract::extract_tar(input, dest_dir).await?,
                }
            }
            Compression::Zstd(Archive::Tar) => {
                let read = stream_to_reader(bytes);
                let read = tokio::io::BufReader::new(read);
                let input = async_compression::tokio::bufread::ZstdDecoder::new(read);
                extract::extract_tar(input, dest_dir).await?
            }
            Compression::Xz(Archive::Tar) => {
                let read = stream_to_reader(bytes);
                let read = tokio::io::BufReader::new(read);
                let input = async_compression::tokio::bufread::XzDecoder::new(read);
                extract::extract_tar(input, dest_dir).await?
            }
            Compression::None(Archive::None) => {
                let read = std::pin::pin!(stream_to_reader(bytes));
                let path = dest_dir.join(candidate.filename);
                extract::write_async(read, &path, 0o755).await?;
                path
            }
            missing => todo!("{missing:?}"),
        };

        return Ok(path);
    }

    Err(anyhow!("no asset found"))
}

fn stream_to_reader(
    stream: impl Stream<Item = reqwest::Result<Bytes>>,
) -> impl tokio::io::AsyncRead + Unpin {
    let stream = stream.then(|b| async { b.map_err(std::io::Error::other) });

    Box::pin(tokio_util::io::StreamReader::new(stream))
}

/// Install latest version and record in the local installation manifest.
pub(crate) async fn install(
    client: reqwest::Client,
    repo: Repo,
    dest_dir: &Path,
    progress: UnboundedSender<f64>,
) -> Result<Binary> {
    let url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        repo.owner, repo.name,
    ))?;
    let Release { tag_name, assets } = client.get(url).send().await?.json().await?;
    let mut path = fetch_and_extract(client, dest_dir, assets, progress).await?;

    if let Some(name) = &repo.rename {
        let from = path.clone();
        path.pop();
        path.push(name);

        std::fs::rename(from, &path)?;
    }

    Ok(Binary {
        repo,
        path,
        version: tag_name,
    })
}

/// Check if there is a new [`Release`] for `binary`.
pub(crate) async fn check(client: reqwest::Client, binary: &Binary) -> Result<Option<Release>> {
    let url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        binary.repo.owner, binary.repo.name,
    ))?;

    let release: Release = client.get(url).send().await?.json().await?;
    Ok((binary.version != release.tag_name).then_some(release))
}

/// Try to update `binary` with `release` info. Returns `Ok(binary)` on successful update.
pub(crate) async fn update(
    client: reqwest::Client,
    binary: &Binary,
    Release { tag_name, assets }: Release,
    progress: UnboundedSender<f64>,
) -> Result<Binary> {
    let dest_dir = &binary
        .path
        .parent()
        .ok_or_else(|| anyhow!("no parent for path found"))?;

    let mut path = fetch_and_extract(client, dest_dir, assets, progress)
        .await
        .with_context(|| "failed to extract".to_string())?;

    if let Some(name) = &binary.repo.rename {
        let from = path.clone();
        path.pop();
        path.push(name);

        std::fs::rename(from, &path)?;
    }

    Ok(Binary {
        repo: binary.repo.clone(),
        path: binary.path.clone(),
        version: tag_name,
    })
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

        let (name, url) = make_filename_and_url("tailwindcss-linux-x64");
        let file = parse_file(name, url, "x86_64", "linux").unwrap();
        assert!(matches!(file.kind, Compression::None(Archive::None)));

        Ok(())
    }
}
