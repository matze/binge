//! Extractors for various archive types.
use anyhow::{Result, anyhow};
use async_zip::base::read::seek::ZipFileReader;
use std::fs::File;
use std::io::{Cursor, Read, copy};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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
    permissions.set_mode(mode);
    output.set_permissions(permissions).await?;

    Ok(())
}

/// Write final binary.
fn write<R: Read>(mut input: R, dest: &Path, mode: u32) -> Result<()> {
    let mut output = File::create(dest)?;
    copy(&mut input, &mut output)?;

    let mut permissions = output.metadata()?.permissions();
    permissions.set_mode(mode);
    output.set_permissions(permissions)?;

    Ok(())
}

/// Extract contained binary and return [`PathBuf`] to where it is located now.
pub(crate) async fn extract_zip<B: AsRef<[u8]> + Unpin>(
    bytes: B,
    dest_dir: &Path,
) -> Result<PathBuf> {
    let mut archive = ZipFileReader::with_tokio(Cursor::new(bytes)).await?;

    let (index, mode, dest) = archive
        .file()
        .entries()
        .iter()
        .enumerate()
        .find_map(|(index, entry)| {
            let mode = entry
                .unix_permissions()
                .map(u32::from)
                .filter(|mode| (mode & 0o100) != 0)?;

            let name = entry
                .filename()
                .as_str()
                .ok()
                .filter(|name| !name.ends_with('/'))?;

            let basename = Path::new(name).file_name()?;
            Some((index, mode, dest_dir.join(basename)))
        })
        .ok_or_else(|| anyhow!("failed to find executable"))?;

    let reader = archive.reader_without_entry(index).await?;
    write_async(reader.compat(), &dest, mode).await?;
    Ok(dest)
}

/// Extract contained binary and return [`PathBuf`] to where it is located now.
pub(crate) fn extract_tar<R: Read>(input: R, dest_dir: &Path) -> Result<PathBuf> {
    let mut archive = tar::Archive::new(input);

    for entry in archive.entries()? {
        let entry = entry?;
        let header = entry.header();

        if let Ok(mode) = header.mode()
            && (mode & 0o100) != 0
            && header.entry_type() == tar::EntryType::Regular
        {
            let path = entry.path()?;
            let name = path.file_name().ok_or_else(|| anyhow!("no filename"))?;
            let dest = dest_dir.join(name);
            write(entry, &dest, mode)?;
            return Ok(dest);
        }
    }

    Err(anyhow!("failed to find executable"))
}

/// Extract single binary file.
pub(crate) fn extract_single<R: Read>(
    input: R,
    dest_dir: &Path,
    filename: &Path,
) -> Result<PathBuf> {
    let dest = dest_dir.join(filename);
    write(input, &dest, 0o755)?;
    Ok(dest)
}
