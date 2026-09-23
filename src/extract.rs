//! Extractors for various archive types.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use async_zip::base::read1::seek::ZipArchiveReader;
use futures_lite::StreamExt;
use futures_lite::io::Cursor;
use tokio::io::AsyncRead;
use tokio_util::compat::FuturesAsyncReadCompatExt;

/// Async variant of [`write`].
pub(crate) async fn write_async<R: AsyncRead + Unpin>(
    mut input: R,
    dest: &Path,
    mode: u32,
) -> Result<()> {
    let mut output = tokio::fs::File::create(dest).await?;
    tokio::io::copy(&mut input, &mut output).await?;

    let mut permissions = output.metadata().await?.permissions();
    permissions.set_mode(mode & 0o755);
    output.set_permissions(permissions).await?;

    Ok(())
}

/// Extract contained binary and return [`PathBuf`] to where it is located now.
pub(crate) async fn extract_zip<B: AsRef<[u8]> + Unpin>(
    bytes: B,
    dest_dir: &Path,
) -> Result<PathBuf> {
    let mut archive = ZipArchiveReader::open(Cursor::new(bytes)).await?;

    let (index, mode, dest) = archive
        .cdrs()
        .iter()
        .enumerate()
        .find_map(|(index, cdr)| {
            // The external attributes' high 16 bits hold the Unix mode.
            // See <https://github.com/Majored/rs-async-zip/blob/main/SPECIFICATION.md#4422>.
            let mode = cdr.cdrh.exter_attr >> 16;
            if (mode & 0o100) == 0 {
                return None;
            }

            let name = cdr
                .insecure_file_name
                .as_str()
                .filter(|name| !name.ends_with('/'))?;

            let basename = Path::new(name).file_name()?;
            Some((index, mode, dest_dir.join(basename)))
        })
        .ok_or_else(|| anyhow!("failed to find executable"))?;

    let reader = archive.file(index).await?;
    write_async(reader.compat(), &dest, mode).await?;
    Ok(dest)
}

/// Extract contained binary and return [`PathBuf`] to where it is located now.
pub(crate) async fn extract_tar<R: AsyncRead + Unpin>(
    input: R,
    dest_dir: &Path,
) -> Result<PathBuf> {
    let mut archive = tokio_tar::Archive::new(input);
    let mut entries = archive.entries()?;

    while let Some(entry) = entries.next().await {
        let entry = entry?;
        let header = entry.header();

        if let Ok(mode) = header.mode()
            && (mode & 0o100) != 0
            && header.entry_type() == tokio_tar::EntryType::Regular
        {
            let path = entry.path()?;
            let name = path.file_name().ok_or_else(|| anyhow!("no filename"))?;
            let dest = dest_dir.join(name);
            write_async(entry, &dest, mode).await?;
            return Ok(dest);
        }
    }

    Err(anyhow!("failed to find executable"))
}
