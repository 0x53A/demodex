//! Image uploads use generated names; client filenames are never filesystem paths.
use anyhow::{Result, bail, ensure};
use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

pub const MAX_BYTES: usize = 4 * 1024 * 1024;

// Matches the bounded whole-file SFTP transport. Files are never decoded or resized.
pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 240
            && name != "."
            && name != ".."
            && !name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\'),
        "Shared file needs a filename without path separators (maximum 240 bytes)"
    );
    Ok(())
}

pub fn decode_file(name: &str, data: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    validate_name(name)?;
    ensure!(
        data.len() <= MAX_FILE_BYTES.div_ceil(3) * 4,
        "Files are limited to 32 MiB"
    );
    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
    ensure!(bytes.len() <= MAX_FILE_BYTES, "Files are limited to 32 MiB");
    Ok(bytes)
}

pub fn save_file(root: &Path, bytes: &[u8], name: &str) -> Result<String> {
    validate_name(name)?;
    let directory = root.join("uploads").join(uuid::Uuid::new_v4().to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let path = directory.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(path.to_string_lossy().into_owned())
}

pub fn extension(bytes: &[u8]) -> Result<&'static str> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_BYTES,
        "Images must be between 1 byte and 4 MiB"
    );
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Ok("jpg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Ok("webp");
    }
    bail!("Choose a PNG, JPEG, GIF or WebP image")
}

pub fn save(root: &Path, bytes: &[u8], extension: &str) -> Result<String> {
    // The parent is the daemon's private data directory, not a client-supplied path.
    let directory = root.join("uploads");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let path = directory.join(format!("{}.{}", uuid::Uuid::new_v4(), extension));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&path);
        return Err(error.into());
    }
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn validates_size_and_format_and_preserves_private_unique_files() -> Result<()> {
        let png = b"\x89PNG\r\n\x1a\nfixture";
        assert_eq!(extension(png)?, "png");
        assert!(extension(b"<svg></svg>").is_err());
        assert!(extension(&[]).is_err());
        assert!(extension(&vec![0; MAX_BYTES + 1]).is_err());
        let root = tempfile::tempdir()?;
        let first = save(root.path(), png, extension(png)?)?;
        let second = save(root.path(), png, extension(png)?)?;
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first)?, png);
        assert_eq!(
            std::fs::metadata(first)?.permissions().mode() & 0o777,
            0o600
        );
        Ok(())
    }
}
