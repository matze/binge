//! Extractors for various archive types.
use anyhow::{Result, anyhow};
use std::fs::File;
use std::io::{Read, Seek, copy};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
pub(crate) fn extract_zip<R: Read + Seek>(input: R, dest_dir: &Path) -> Result<PathBuf> {
    let mut archive = zip::ZipArchive::new(input)?;

    for i in 0..archive.len() {
        let input = archive.by_index(i)?;

        if let Some((mode, name)) = input.unix_mode().zip(input.enclosed_name()) {
            // TODO: also check it's not a directory
            if (mode & 0o100) != 0 {
                let dest = dest_dir.join(&name);
                write(input, &dest, mode)?;
                return Ok(dest);
            }
        }
    }

    Err(anyhow!("failed to find executable"))
}

/// Extract contained binary and return [`PathBuf`] to where it is located now.
pub(crate) fn extract_tar<R: Read>(input: R, dest_dir: &Path) -> Result<PathBuf> {
    let mut archive = tar::Archive::new(input);

    for entry in archive.entries()? {
        let entry = entry?;
        let header = entry.header();

        if let Ok(mode) = header.mode() {
            if (mode & 0o100) != 0 && header.entry_type() == tar::EntryType::Regular {
                let path = entry.path()?;
                let name = path.file_name().ok_or_else(|| anyhow!("no filename"))?;
                let dest = dest_dir.join(name);
                write(entry, &dest, mode)?;
                return Ok(dest);
            }
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
